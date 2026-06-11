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
}

#[derive(Debug)]
struct ProcSlot {
    buf: Vec<u8>,
    base_offset: u64,
    cap: usize,
    status: ProcStatus,
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
        {
            let mut guard = unlock(self.inner.lock());
            guard.insert(
                pid,
                ProcSlot {
                    buf: Vec::new(),
                    base_offset: 0,
                    cap,
                    status: ProcStatus::Running,
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
        tokio::spawn(supervise(pid, child, opts.timeout, table));
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

    /// Mark a process as killed in the slot table.
    ///
    /// # Panics
    /// Never panics in normal operation.
    pub fn kill(&self, pid: ProcessId) {
        let mut guard = unlock(self.inner.lock());
        if let Some(slot) = guard.get_mut(&pid) {
            slot.status = ProcStatus::Killed;
        }
    }
}

async fn supervise(
    pid: ProcessId,
    mut child: Child,
    timeout: Option<Duration>,
    table: Arc<Mutex<HashMap<ProcessId, ProcSlot>>>,
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
    let mut timed_out = false;
    let terminal_status = if let Some(d) = timeout {
        match tokio::time::timeout(d, child.wait()).await {
            Ok(Ok(status)) => ProcStatus::Exited(status.code().unwrap_or(-1)),
            Ok(Err(_e)) => ProcStatus::Exited(-1),
            Err(_elapsed) => {
                timed_out = true;
                // Deliver SIGKILL without awaiting the reap, then reap with a
                // bound — the signal lands regardless; only the wait may wedge.
                let _ = child.start_kill();
                let _ = tokio::time::timeout(KILL_CLEANUP_BOUND, child.wait()).await;
                ProcStatus::TimedOut
            }
        }
    } else {
        match child.wait().await {
            Ok(status) => ProcStatus::Exited(status.code().unwrap_or(-1)),
            Err(_e) => ProcStatus::Exited(-1),
        }
    };
    // The child has exited (or been killed), so its pipe write-ends are closed
    // and the reader tasks will hit EOF. Wait for them to finish so every byte
    // is appended to the ring buffer BEFORE we flip the status to terminal —
    // otherwise a foreground reader observing the terminal status could return
    // before in-flight output is captured. After a timeout kill the drain is
    // bounded for the orphaned-grandchild reason above.
    let drain = async move {
        for r in readers {
            let _ = r.await;
        }
    };
    if timed_out {
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
