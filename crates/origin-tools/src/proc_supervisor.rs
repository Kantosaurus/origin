// SPDX-License-Identifier: Apache-2.0
//! Process supervisor: owns long-running children, exposes a byte-offset
//! ring-buffer per process for the `Monitor` tool to tail.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Notify;

use crate::error::{ErrClass, ToolError};

pub type ProcessId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcStatus {
    Running,
    Exited(i32),
    TimedOut,
    Killed,
}

impl ProcStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Debug, Clone)]
pub struct ReadChunk {
    pub bytes: String,
    pub next_offset: u64,
    pub status: ProcStatus,
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOpts {
    pub timeout: Option<Duration>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub buffer_cap_bytes: Option<usize>,
    /// Per-tool sandbox confinement applied to the spawned child (P11.5).
    ///
    /// `None` (the default everywhere) is treated exactly like
    /// [`SandboxProfile::Inherit`]: the child inherits the daemon's privileges
    /// and [`origin_sandbox::apply`] is never invoked, so the spawn is
    /// byte-identical to the pre-wiring path. When `Some(profile)`, the profile
    /// is handed to [`origin_sandbox::apply`] just before `spawn()` so the
    /// per-OS backend can confine the child.
    ///
    /// Enforcement is Linux/macOS-only and only when the matching
    /// `origin-sandbox` cargo feature is compiled in; on every other host (and
    /// in the default feature set used here) `apply` resolves to the
    /// [`origin_sandbox::backend_noop`] backend, which mutates nothing and only
    /// logs a warning for a non-`Inherit` profile. The Windows backend needs a
    /// post-spawn Job-Object handshake that this supervisor does not perform, so
    /// the `windows` feature is intentionally not engaged here.
    pub sandbox_profile: Option<origin_sandbox::SandboxProfile>,
}

#[derive(Debug)]
struct ProcSlot {
    buf: Vec<u8>,
    base_offset: u64,
    cap: usize,
    status: ProcStatus,
    /// Fired by [`Supervisor::kill`] to ask the detached `supervise` task to
    /// deliver a real OS kill (SIGKILL / `TerminateProcess`) to its child
    /// immediately, rather than waiting for the natural exit or timeout. The
    /// `supervise` task holds a clone and races a `notified()` future against
    /// `child.wait()`. Always present; an empty `Notify` is cheap.
    kill: Arc<Notify>,
}

impl ProcSlot {
    fn append(&mut self, more: &[u8]) {
        self.buf.extend_from_slice(more);
        if self.buf.len() > self.cap {
            let overflow = self.buf.len() - self.cap;
            self.buf.drain(..overflow);
            self.base_offset += overflow as u64;
        }
    }

    fn read_since(&self, offset: u64, max: usize) -> ReadChunk {
        let start = offset.max(self.base_offset);
        // Clamp to usize::MAX to avoid truncation on 32-bit targets; ring
        // buffer capacity is also bounded by `usize` so this is safe.
        #[allow(clippy::cast_possible_truncation)]
        let diff = (start - self.base_offset).min(usize::MAX as u64) as usize;
        let available = self.buf.get(diff..).unwrap_or_default();
        let take = available.len().min(max);
        let slice = &available[..take];
        ReadChunk {
            bytes: String::from_utf8_lossy(slice).into_owned(),
            next_offset: start + take as u64,
            status: self.status,
        }
    }
}

/// Convenience: unlock a `Mutex`, recovering from poison.
fn unlock<T>(r: Result<T, PoisonError<T>>) -> T {
    r.unwrap_or_else(PoisonError::into_inner)
}

/// Apply the requested [`origin_sandbox::SandboxProfile`] to `cmd` before spawn.
///
/// `None` (or any caller that never sets `sandbox_profile`) is a complete no-op:
/// `apply` is never called, so the spawn is byte-identical to the pre-wiring
/// path. A `Some(profile)` is forwarded to [`origin_sandbox::apply`] via the
/// std command behind tokio's `Command` (`as_std_mut`); tokio preserves the
/// `pre_exec`/creation-flag mutations the backend makes when it later spawns.
///
/// Enforcement is Linux/macOS-only and only when the matching `origin-sandbox`
/// OS feature is compiled in. origin-tools enables no such feature, so in this
/// dependency graph `apply` resolves to [`origin_sandbox::backend_noop`], which
/// mutates nothing and only warns on a non-`Inherit` profile — Bash keeps
/// working. The Windows enforcement backend would set `CREATE_SUSPENDED` and
/// depend on a post-spawn Job-Object attach + `ResumeThread` this supervisor
/// does not perform; because that backend is never compiled here it cannot be
/// reached, but the hazard is documented so it is not wired naively later.
fn apply_sandbox(cmd: &mut Command, profile: Option<origin_sandbox::SandboxProfile>) {
    let Some(profile) = profile else { return };
    if profile == origin_sandbox::SandboxProfile::Inherit {
        return;
    }
    // Confinement is best-effort here: a backend that rejects the policy must
    // not fail the tool. The real enforcement backends (Linux/macOS) succeed at
    // apply-time and surface kernel denials inside the child; the no-op backend
    // already logs its own warning. We therefore drop any apply error and spawn
    // the child unconfined rather than failing the spawn.
    let _ = origin_sandbox::apply(profile, cmd.as_std_mut());
}

#[derive(Debug, Clone)]
pub struct Supervisor {
    inner: Arc<Mutex<HashMap<ProcessId, ProcSlot>>>,
    next: Arc<AtomicU32>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(AtomicU32::new(1)),
        }
    }

    /// Spawn `command` through the platform shell.
    ///
    /// # Errors
    /// Returns `bash.spawn_failed` if the shell child cannot be spawned.
    ///
    /// # Panics
    /// Never panics in normal operation (mutex poison recovery is handled
    /// internally).
    pub fn spawn(&self, command: &str, opts: &SpawnOpts) -> Result<ProcessId, ToolError> {
        let pid = self.next.fetch_add(1, Ordering::Relaxed);
        let cap = opts.buffer_cap_bytes.unwrap_or(512 * 1024);
        // Per-pid kill signal: `kill(pid)` fires this so the detached
        // `supervise` task delivers an immediate OS kill. Cloned into the slot
        // (so `kill` can find it) and into the task (so the task can wait on it).
        let kill = Arc::new(Notify::new());
        {
            let mut guard = unlock(self.inner.lock());
            guard.insert(
                pid,
                ProcSlot {
                    buf: Vec::new(),
                    base_offset: 0,
                    cap,
                    status: ProcStatus::Running,
                    kill: Arc::clone(&kill),
                },
            );
        }

        let mut cmd: Command;
        #[cfg(unix)]
        {
            cmd = Command::new("sh");
            cmd.arg("-c").arg(command);
        }
        #[cfg(windows)]
        {
            cmd = Command::new("pwsh");
            cmd.args(["-NoProfile", "-Command", command]);
        }
        if let Some(cwd) = &opts.cwd {
            cmd.current_dir(cwd);
        }
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        cmd.kill_on_drop(true);

        // P11.5: confine the child to the requested sandbox profile, if any.
        // `None`/`Inherit` skip `apply` entirely so the spawn is byte-identical
        // to the pre-wiring path. `apply` takes a `std::process::Command`;
        // tokio's `Command` exposes the wrapped std command via `as_std_mut`,
        // and tokio preserves any `pre_exec`/creation-flags set there when it
        // spawns. The Windows enforcement backend sets `CREATE_SUSPENDED` and
        // relies on a post-spawn Job-Object attach this supervisor does not run,
        // so it would wedge the child — we keep that backend out of the spawn
        // path. The default build has no OS backend feature enabled, so this
        // resolves to the no-op backend that only warns on a non-Inherit
        // profile and mutates nothing.
        apply_sandbox(&mut cmd, opts.sandbox_profile);

        let spawned = cmd.spawn();
        // On Windows, if the direct spawn fails (e.g. the command is a shell
        // builtin), retry once through PowerShell. On Unix there is no fallback,
        // so the spawn result is propagated as-is.
        #[cfg(windows)]
        let spawned = spawned.or_else(|_| {
            let mut fallback = Command::new("powershell");
            fallback.args(["-NoProfile", "-Command", command]);
            if let Some(cwd) = &opts.cwd {
                fallback.current_dir(cwd);
            }
            for (k, v) in &opts.env {
                fallback.env(k, v);
            }
            fallback
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null());
            fallback.kill_on_drop(true);
            fallback.spawn()
        });
        let child = spawned.map_err(|e| ToolError::new(ErrClass::Bash, "spawn_failed", e.to_string()))?;

        let table = self.inner.clone();
        tokio::spawn(supervise(pid, child, opts.timeout, table, kill));
        Ok(pid)
    }

    /// Read bytes from `offset` for at most `max` bytes.
    ///
    /// # Errors
    /// Returns `validation.unknown_pid` if pid was never spawned.
    ///
    /// # Panics
    /// Never panics in normal operation.
    pub fn read_since(&self, pid: ProcessId, offset: u64, max: usize) -> Result<ReadChunk, ToolError> {
        let guard = unlock(self.inner.lock());
        guard
            .get(&pid)
            .map(|s| s.read_since(offset, max))
            .ok_or_else(|| ToolError::new(ErrClass::Validation, "unknown_pid", format!("no such pid {pid}")))
    }

    /// Deliver a real OS kill (SIGKILL / `TerminateProcess`) to the running
    /// child and mark the slot `Killed`.
    ///
    /// The slot's status is flipped to `Killed` synchronously (so a foreground
    /// poller observes a terminal status on its next read), and the per-pid
    /// kill signal is fired so the detached `supervise` task — the sole owner of
    /// the `Child` handle — races out of its `wait()` and calls `start_kill()`
    /// on the OS process. Without the signal `kill` only set a flag while the
    /// child kept running; with it the process is genuinely terminated.
    ///
    /// A no-op for an unknown / already-reaped pid.
    ///
    /// # Panics
    /// Never panics in normal operation.
    pub fn kill(&self, pid: ProcessId) {
        let mut guard = unlock(self.inner.lock());
        let Some(slot) = guard.get_mut(&pid) else {
            return;
        };
        // Don't clobber a real terminal status (Exited/TimedOut) the
        // `supervise` task may have already recorded.
        if !slot.status.is_terminal() {
            slot.status = ProcStatus::Killed;
        }
        let kill = Arc::clone(&slot.kill);
        // Release the table lock BEFORE the notify so we never hold the mutex
        // across it (clippy `significant_drop_tightening`).
        drop(guard);
        // Wake the supervise task so it delivers the OS kill.
        kill.notify_one();
    }
}

/// RAII guard that hard-kills a supervised foreground process if its waiter is
/// dropped before the process terminates.
///
/// A FOREGROUND `Bash` call polls the supervisor's ring buffer in a loop while
/// the child runs. When the daemon's interrupt path drops the whole turn future
/// (Ctrl+C → `select!` drops the loser), that poll future is dropped at its next
/// `.await` — but the detached `supervise` task owns the `Child` and would keep
/// it alive to natural completion. Holding this guard across the poll loop turns
/// that drop into an immediate `Supervisor::kill(pid)`, `SIGKILLing` the child.
///
/// [`disarm`](KillOnDrop::disarm) cancels the kill once the process has
/// terminated normally, so a clean foreground completion never fires a kill.
/// BACKGROUND spawns (`run_in_background`) never construct this guard, so a
/// backgrounded pid keeps running by design.
#[derive(Debug)]
pub struct KillOnDrop {
    sup: Supervisor,
    pid: ProcessId,
    armed: bool,
}

impl KillOnDrop {
    /// Arm a kill-on-drop guard for `pid` on `sup`. `sup` is `Arc`-backed, so
    /// the clone is cheap.
    #[must_use]
    pub fn new(sup: &Supervisor, pid: ProcessId) -> Self {
        Self {
            sup: sup.clone(),
            pid,
            armed: true,
        }
    }

    /// Cancel the pending kill — call this once the process has terminated on
    /// its own (normal foreground completion) so dropping the guard is a no-op.
    pub const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.sup.kill(self.pid);
        }
    }
}

async fn supervise(
    pid: ProcessId,
    mut child: Child,
    timeout: Option<Duration>,
    table: Arc<Mutex<HashMap<ProcessId, ProcSlot>>>,
    kill: Arc<Notify>,
) {
    // Ceiling on the post-kill cleanup awaits below. After a timeout kill the
    // status flip must not hinge on cooperative behavior: the reap can miss a
    // SIGCHLD wakeup (observed with multiple tokio runtimes in one process),
    // and an orphaned grandchild of `sh -c` can inherit the pipe write-ends
    // and hold them open long after the direct child is dead. Without these
    // bounds the slot stayed `Running` for the orphan's whole lifetime.
    const KILL_CLEANUP_BOUND: Duration = Duration::from_secs(2);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut readers = Vec::new();
    if let Some(out) = stdout {
        let t = table.clone();
        readers.push(tokio::spawn(async move {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut g = unlock(t.lock());
                if let Some(s) = g.get_mut(&pid) {
                    s.append(line.as_bytes());
                    s.append(b"\n");
                }
            }
        }));
    }
    if let Some(err) = stderr {
        let t = table.clone();
        readers.push(tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut g = unlock(t.lock());
                if let Some(s) = g.get_mut(&pid) {
                    s.append(b"stderr: ");
                    s.append(line.as_bytes());
                    s.append(b"\n");
                }
            }
        }));
    }
    // `force_killed` covers BOTH the timeout kill and an explicit
    // `Supervisor::kill` (the daemon's Ctrl+C hard-kill / foreground-bash
    // kill-on-drop). Either way the child was SIGKILLed, so the post-kill drain
    // is bounded for the same orphaned-grandchild reason as the timeout path.
    let mut force_killed = false;
    let terminal_status = if let Some(d) = timeout {
        tokio::select! {
            // The wait races the timeout AND the explicit kill signal.
            biased;
            () = kill.notified() => {
                force_killed = true;
                // Deliver SIGKILL without awaiting the reap, then reap with a
                // bound — the signal lands regardless; only the wait may wedge.
                let _ = child.start_kill();
                let _ = tokio::time::timeout(KILL_CLEANUP_BOUND, child.wait()).await;
                ProcStatus::Killed
            }
            res = tokio::time::timeout(d, child.wait()) => match res {
                Ok(Ok(status)) => ProcStatus::Exited(status.code().unwrap_or(-1)),
                Ok(Err(_e)) => ProcStatus::Exited(-1),
                Err(_elapsed) => {
                    force_killed = true;
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(KILL_CLEANUP_BOUND, child.wait()).await;
                    ProcStatus::TimedOut
                }
            },
        }
    } else {
        tokio::select! {
            biased;
            () = kill.notified() => {
                force_killed = true;
                let _ = child.start_kill();
                let _ = tokio::time::timeout(KILL_CLEANUP_BOUND, child.wait()).await;
                ProcStatus::Killed
            }
            res = child.wait() => match res {
                Ok(status) => ProcStatus::Exited(status.code().unwrap_or(-1)),
                Err(_e) => ProcStatus::Exited(-1),
            },
        }
    };
    // The child has exited (or been killed), so its pipe write-ends are closed
    // and the reader tasks will hit EOF. Wait for them to finish so every byte
    // is appended to the ring buffer BEFORE we flip the status to terminal —
    // otherwise a foreground reader observing the terminal status could return
    // before in-flight output is captured. After a force-kill the drain is
    // bounded for the orphaned-grandchild reason above.
    let drain = async move {
        for r in readers {
            let _ = r.await;
        }
    };
    if force_killed {
        let _ = tokio::time::timeout(KILL_CLEANUP_BOUND, drain).await;
    } else {
        drain.await;
    }
    let mut g = unlock(table.lock());
    if let Some(s) = g.get_mut(&pid) {
        if !s.status.is_terminal() {
            s.status = terminal_status;
        }
    }
}
