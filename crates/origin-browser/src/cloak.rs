// SPDX-License-Identifier: Apache-2.0
//! Subprocess client for the vendored `CloakBrowser` sidecar.
//!
//! Resolves the sidecar via `ORIGIN_CLOAK_DIR` env var, or falls back to
//! `<exe-dir>/../vendor/cloak-browser/cloak-cli.mjs`. Runs `node` on it.

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
    #[error("sidecar not found at {0}")]
    NotFound(std::path::PathBuf),
}

// name `CloakClient` mirrors `AgentBrowserClient` as the matching backend
// pair — renaming to drop the module-name overlap would obscure the
// pair-symmetry that the router relies on.
#[allow(clippy::module_name_repetitions)]
pub struct CloakClient {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CloakClient {
    /// Spawn the vendored sidecar.
    ///
    /// # Errors
    /// Returns [`ClientError::NotFound`] if the sidecar path resolves to a
    /// missing file. IO errors otherwise.
    pub async fn spawn() -> Result<Self, ClientError> {
        let sidecar = resolve_sidecar()?;
        Self::spawn_with_command("node", &[sidecar.to_str().unwrap_or_default()]).await
    }

    /// Test-visible variant.
    ///
    /// # Errors
    /// Forwards spawn IO errors.
    // signature stays async so callers can keep their `.await` and the
    // pair `AgentBrowserClient::spawn_with_command` (also async) shares the
    // same shape; the body uses no await today but the trait pair is async.
    #[allow(clippy::unused_async)]
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
        self.stdin
            .write_all(&line)
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        let mut buf = String::new();
        let n = self
            .stdout
            .read_line(&mut buf)
            .await
            .map_err(|e| ClientError::Io(e.to_string()))?;
        if n == 0 {
            return Err(ClientError::Exited);
        }
        let resp: SnapshotResp =
            serde_json::from_str(buf.trim_end()).map_err(|e| ClientError::Io(format!("decode: {e}")))?;
        Ok(resp)
    }
}

fn resolve_sidecar() -> Result<std::path::PathBuf, ClientError> {
    if let Ok(p) = std::env::var("ORIGIN_CLOAK_DIR") {
        let cli = std::path::PathBuf::from(p).join("cloak-cli.mjs");
        if cli.exists() {
            return Ok(cli);
        }
        return Err(ClientError::NotFound(cli));
    }
    let exe = std::env::current_exe().map_err(ClientError::Spawn)?;
    let cand = exe
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("vendor/cloak-browser/cloak-cli.mjs"))
        .unwrap_or_default();
    if cand.exists() {
        return Ok(cand);
    }
    Err(ClientError::NotFound(cand))
}
