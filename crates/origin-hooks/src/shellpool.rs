//! Pre-spawned shell pool. Each pool member is a long-lived
//! `tokio::process::Child` with piped stdin + stdout. Dispatch writes a
//! script to stdin and reads until the configured terminator byte on stdout.
//!
//! N9.7 — amortized cost per hook dispatch is one `write_all` + one
//! `read_until`, not a fresh `fork+exec`.

use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// How to spawn one shell worker.
#[derive(Debug, Clone)]
pub struct ShellSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Byte that terminates one response on stdout. We standardise on NUL.
    pub read_terminator: u8,
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("stdin closed unexpectedly")]
    StdinClosed,
    #[error("stdout closed unexpectedly")]
    StdoutClosed,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    spec: ShellSpec,
}

impl Worker {
    fn spawn(spec: &ShellSpec) -> Result<Self, PoolError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(PoolError::StdinClosed)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(PoolError::StdoutClosed)?);
        Ok(Self {
            child,
            stdin,
            stdout,
            spec: spec.clone(),
        })
    }

    async fn dispatch(&mut self, script: &str) -> Result<Vec<u8>, PoolError> {
        self.stdin.write_all(script.as_bytes()).await?;
        self.stdin.flush().await?;
        let mut buf = Vec::with_capacity(256);
        let n = self
            .stdout
            .read_until(self.spec.read_terminator, &mut buf)
            .await?;
        if n == 0 {
            return Err(PoolError::StdoutClosed);
        }
        // `read_until` also returns on EOF. If the last byte is NOT the
        // terminator, the worker closed stdout mid-response: `buf` is a
        // truncated/partial reply, so surface an error instead of handing back
        // partial bytes as if they were a complete response.
        if buf.last() != Some(&self.spec.read_terminator) {
            return Err(PoolError::StdoutClosed);
        }
        // Strip trailing terminator.
        buf.pop();
        Ok(buf)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// Pre-spawned shell pool.
pub struct ShellPool {
    spec: ShellSpec,
    workers: Vec<Mutex<Option<Worker>>>,
    spawn_count: AtomicUsize,
    next: AtomicUsize,
}

impl ShellPool {
    /// Create a pool of `size` workers up front.
    ///
    /// # Errors
    /// Returns [`PoolError::Spawn`] if any worker fails to start.
    #[allow(clippy::unused_async)] // API is async for future extension (e.g. health-check ping)
    pub async fn new(spec: ShellSpec, size: usize) -> Result<Self, PoolError> {
        let mut workers = Vec::with_capacity(size.max(1));
        let mut spawn_count = 0usize;
        for _ in 0..size.max(1) {
            workers.push(Mutex::new(Some(Worker::spawn(&spec)?)));
            spawn_count += 1;
        }
        Ok(Self {
            spec,
            workers,
            spawn_count: AtomicUsize::new(spawn_count),
            next: AtomicUsize::new(0),
        })
    }

    /// Dispatch `script` to one worker (round-robin) and return its bytes up
    /// to (and not including) the configured terminator.
    ///
    /// If the chosen worker has died since last use, a fresh worker is spawned
    /// in its slot and the dispatch is retried once on the new worker.
    ///
    /// # Errors
    /// Forwards [`PoolError`] from spawn / IO.
    pub async fn dispatch(&self, script: &str) -> Result<Vec<u8>, PoolError> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let mut slot = self.workers[idx].lock().await;
        let alive = slot.as_mut().map_or(false, Worker::is_alive);
        if !alive {
            *slot = Some(Worker::spawn(&self.spec)?);
            self.spawn_count.fetch_add(1, Ordering::Relaxed);
        }
        match slot.as_mut() {
            Some(w) => match w.dispatch(script).await {
                Ok(b) => Ok(b),
                Err(PoolError::StdoutClosed) => {
                    // Respawn and retry once.
                    *slot = Some(Worker::spawn(&self.spec)?);
                    self.spawn_count.fetch_add(1, Ordering::Relaxed);
                    slot.as_mut()
                        .ok_or(PoolError::StdoutClosed)?
                        .dispatch(script)
                        .await
                }
                Err(e) => Err(e),
            },
            None => Err(PoolError::StdinClosed),
        }
    }

    /// Total `Worker::spawn` calls (including respawns). Used by tests to
    /// assert no per-event spawn.
    #[must_use]
    pub fn spawn_count(&self) -> usize {
        self.spawn_count.load(Ordering::Relaxed)
    }
}
