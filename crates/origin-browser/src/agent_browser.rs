//! Subprocess client for the `agent-browser` CLI.
//!
//! Speaks stdio-JSON. One verb in, one response out. Long-lived per session.

use crate::protocol::{SnapshotResp, Verb};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(String),
    #[error("backend exited")]
    Exited,
}

pub struct AgentBrowserClient {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl AgentBrowserClient {
    /// Spawn the real `agent-browser` CLI from PATH.
    ///
    /// # Errors
    /// Forwards spawn IO errors.
    pub async fn spawn() -> Result<Self, ClientError> {
        #[cfg(windows)]
        let (prog, args): (&str, &[&str]) = ("agent-browser.cmd", &["--stdio"]);
        #[cfg(not(windows))]
        let (prog, args): (&str, &[&str]) = ("agent-browser", &["--stdio"]);
        Self::spawn_with_command(prog, args).await
    }

    /// Spawn an explicit command — used by tests to point at the fake CLI.
    ///
    /// # Errors
    /// Forwards spawn IO errors.
    pub async fn spawn_with_command(prog: &str, args: &[&str]) -> Result<Self, ClientError> {
        let mut child = Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or(ClientError::Exited)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(ClientError::Exited)?);
        Ok(Self { child, stdin, stdout })
    }

    /// Send a verb, read one response line.
    ///
    /// # Errors
    /// IO/encode errors; `Exited` if the child closed stdout.
    pub async fn send(&mut self, verb: &Verb) -> Result<SnapshotResp, ClientError> {
        let mut line = serde_json::to_vec(verb)?;
        line.push(b'\n');
        self.stdin.write_all(&line).await.map_err(|e| ClientError::Io(e.to_string()))?;
        self.stdin.flush().await.map_err(|e| ClientError::Io(e.to_string()))?;
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await.map_err(|e| ClientError::Io(e.to_string()))?;
        if n == 0 { return Err(ClientError::Exited); }
        let resp: SnapshotResp = serde_json::from_str(buf.trim_end())
            .map_err(|e| ClientError::Io(format!("decode: {e}")))?;
        Ok(resp)
    }
}
