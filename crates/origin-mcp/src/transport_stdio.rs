//! Stdio JSON-RPC transport over a spawned child process.

use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

pub struct StdioTransport {
    inner: Mutex<Inner>,
}

struct Inner {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    /// Spawn `program` with `args`, pipe stdio.
    ///
    /// # Errors
    /// Returns [`TransportError::Io`] on spawn or pipe-take failure.
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, TransportError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::Other("no stdin".into()))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| TransportError::Other("no stdout".into()))?,
        );
        Ok(Self {
            inner: Mutex::new(Inner {
                _child: child,
                stdin,
                stdout,
            }),
        })
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError> {
        let mut inner = self.inner.lock().await;
        inner.stdin.write_all(request_json.as_bytes()).await?;
        inner.stdin.write_all(b"\n").await?;
        inner.stdin.flush().await?;
        let mut line = String::new();
        let n = inner.stdout.read_line(&mut line).await?;
        drop(inner);
        if n == 0 {
            return Err(TransportError::Other("eof".into()));
        }
        Ok(serde_json::from_str(&line)?)
    }
}
