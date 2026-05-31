// SPDX-License-Identifier: Apache-2.0
//! Minimal stdio JSON-RPC client for Language Servers.
//!
//! Implements only the subset needed by `Diagnostics`:
//! `initialize`, `initialized`, `textDocument/didOpen`, `textDocument/didChange`,
//! and listening for `textDocument/publishDiagnostics`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: u8, // 1=error, 2=warn, 3=info, 4=hint
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

pub struct LspClient {
    /// Holds the child process alive for the lifetime of the client.
    _child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
}

impl LspClient {
    /// Spawn the server and complete the `initialize` handshake against `workspace_root`.
    ///
    /// # Errors
    /// `LspError::Spawn` if the binary cannot be started, `Protocol` if init fails.
    pub async fn spawn(binary: &str, workspace_root: &Path) -> Result<Self, LspError> {
        Self::spawn_with_args(binary, &[], workspace_root).await
    }

    /// Spawn `binary` with explicit launch `args` (for example `--stdio`) and
    /// complete the `initialize` handshake against `workspace_root`.
    ///
    /// Most servers in the fleet registry need a launch flag (`pyright-langserver
    /// --stdio`, `solargraph stdio`, …); the bare [`spawn`](Self::spawn) covers
    /// the argv-free servers (such as `rust-analyzer`). The daemon's autonomous
    /// probe splits the registry `launch` string and routes here.
    ///
    /// # Errors
    /// `LspError::Spawn` if the binary cannot be started, `Protocol` if init fails.
    pub async fn spawn_with_args(
        binary: &str,
        args: &[&str],
        workspace_root: &Path,
    ) -> Result<Self, LspError> {
        let mut cmd = Command::new(binary);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Reap the server when the client handle is dropped. Without this a
            // short-lived diagnostics probe (spawn → did_open → drop) would leak
            // a long-running language-server child after the handle goes away.
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| LspError::Spawn(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Spawn("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Spawn("no stdout".into()))?;

        let stdin = Arc::new(Mutex::new(stdin));
        let diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> = Arc::new(RwLock::new(HashMap::new()));

        // Reader loop.
        let diags_clone = diags.clone();
        tokio::spawn(reader_loop(stdout, diags_clone));

        // initialize.
        let root_uri = format!(
            "file://{}",
            workspace_root.display().to_string().replace('\\', "/")
        );
        let init = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {}
            }
        });
        write_frame(stdin.clone(), &init).await?;
        // initialized.
        let initd = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        write_frame(stdin.clone(), &initd).await?;

        Ok(Self {
            _child: child,
            stdin,
            diags,
        })
    }

    /// Notify the server about a file the client has open.
    ///
    /// # Errors
    /// Returns `LspError::Io` if the write fails.
    pub async fn did_open(&self, path: &Path, language_id: &str, text: &str) -> Result<(), LspError> {
        let uri = format!("file://{}", path.display().to_string().replace('\\', "/"));
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }
        });
        write_frame(self.stdin.clone(), &msg).await
    }

    /// Notify the server that a file changed (full sync).
    ///
    /// # Errors
    /// Returns `LspError::Io` if the write fails.
    pub async fn did_change(&self, path: &Path, text: &str) -> Result<(), LspError> {
        let uri = format!("file://{}", path.display().to_string().replace('\\', "/"));
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ { "text": text } ]
            }
        });
        write_frame(self.stdin.clone(), &msg).await
    }

    /// Currently-known diagnostics for `path` (or all files when `None`).
    pub async fn diagnostics(&self, path: Option<&Path>) -> Vec<Diagnostic> {
        let g = self.diags.read().await;
        path.map_or_else(
            || g.values().flatten().cloned().collect(),
            |p| g.get(p).cloned().unwrap_or_default(),
        )
    }

    /// One-shot diagnostics probe for a single file.
    ///
    /// Spawns `program` (with launch `args` such as `--stdio`) against
    /// `workspace_root`, opens `path` (`did_open` with `language_id` + `text`),
    /// then polls for `textDocument/publishDiagnostics` until either the server
    /// has reported diagnostics for `path` or `timeout` elapses, whichever comes
    /// first. The spawned server is reaped when the returned client is dropped
    /// (`kill_on_drop`), so the caller need only drop the [`LspClient`] this
    /// returns (or let it fall out of scope).
    ///
    /// Returns the diagnostics observed for `path` (possibly empty) and the live
    /// [`LspClient`] so the caller controls teardown timing. This is the routine
    /// the daemon's autonomous post-edit feedback uses: spawn → open → bounded
    /// wait → drop.
    ///
    /// The poll loop sleeps in short slices so a server that publishes quickly
    /// returns well before `timeout`; a server that never publishes simply hits
    /// the deadline and returns whatever (if anything) it had reported.
    ///
    /// # Errors
    /// `LspError::Spawn` if the binary cannot be started; `Protocol`/`Io` if the
    /// initialize handshake or the `did_open` write fails.
    pub async fn diagnose_file(
        program: &str,
        args: &[&str],
        workspace_root: &Path,
        path: &Path,
        language_id: &str,
        text: &str,
        timeout: std::time::Duration,
    ) -> Result<(Self, Vec<Diagnostic>), LspError> {
        let client = Self::spawn_with_args(program, args, workspace_root).await?;
        client.did_open(path, language_id, text).await?;
        // Poll in short slices: many servers publish within a few hundred ms of
        // `did_open`, so we return as soon as `path` has any reported entry
        // rather than always waiting the full `timeout`. A server that reports
        // an empty diagnostic set never inserts the key here, so a clean file
        // costs the full deadline — acceptable for a best-effort probe.
        let deadline = std::time::Instant::now() + timeout;
        let slice = std::time::Duration::from_millis(100);
        loop {
            let ready = {
                let g = client.diags.read().await;
                g.get(path).cloned()
            };
            if let Some(diags) = ready {
                return Ok((client, diags));
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(slice).await;
        }
        let diags = client.diagnostics(Some(path)).await;
        Ok((client, diags))
    }
}

/// Write one JSON-RPC frame (`Content-Length: …\r\n\r\n<body>`).
async fn write_frame(stdin: Arc<Mutex<ChildStdin>>, msg: &Value) -> Result<(), LspError> {
    let body = serde_json::to_vec(msg).map_err(|e| LspError::Protocol(e.to_string()))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut g = stdin.lock().await;
    g.write_all(header.as_bytes()).await?;
    g.write_all(&body).await?;
    g.flush().await?;
    drop(g);
    Ok(())
}

/// Background task that reads frames from the server and updates the diagnostics map.
async fn reader_loop(stdout: ChildStdout, diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>) {
    // Cap the server-declared body size so a malformed/hostile language
    // server cannot drive an unbounded `vec![0u8; len]` allocation (OOM).
    const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
    let mut reader = BufReader::new(stdout);
    loop {
        let mut header = String::new();
        let mut content_length: Option<usize> = None;
        // Read headers terminated by an empty line.
        loop {
            header.clear();
            if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
                return;
            }
            let line = header.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                content_length = v.parse().ok();
            }
        }
        let Some(len) = content_length else {
            continue;
        };
        if len > MAX_BODY_BYTES {
            return;
        }
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        if v.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics") {
            handle_diagnostics(&v, &diags).await;
        }
    }
}

/// Convert an LSP `file://` URI to a filesystem path that matches the keys the
/// daemon queries with. Handles two things the naive `strip_prefix("file://")`
/// got wrong:
///   * percent-decoding (`%20` → space) so paths with spaces/special chars match;
///   * the Windows drive form `file:///C:/x` → `/C:/x`, where the leading slash
///     before the drive letter must be dropped (else the key never matches the
///     native `C:\x` path).
fn file_uri_to_path(uri: &str) -> PathBuf {
    let rest = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = percent_decode(rest);
    #[cfg(windows)]
    {
        let b = decoded.as_bytes();
        // "/C:/..." → "C:/..."
        if b.len() >= 3 && b[0] == b'/' && b[2] == b':' && b[1].is_ascii_alphabetic() {
            return PathBuf::from(&decoded[1..]);
        }
    }
    PathBuf::from(decoded)
}

/// Minimal percent-decoder (`%XX` → byte). Leaves malformed escapes verbatim.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn handle_diagnostics(v: &Value, diags: &Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>) {
    let Some(params) = v.get("params") else { return };
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return;
    };
    let path = file_uri_to_path(uri);
    let mut out = Vec::new();
    if let Some(arr) = params.get("diagnostics").and_then(Value::as_array) {
        for d in arr {
            let line = u32::try_from(
                d.pointer("/range/start/line")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(u32::MAX);
            let col = u32::try_from(
                d.pointer("/range/start/character")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
            .unwrap_or(u32::MAX);
            let severity = u8::try_from(d.get("severity").and_then(Value::as_u64).unwrap_or(2)).unwrap_or(2);
            let message = d.get("message").and_then(Value::as_str).unwrap_or("").to_string();
            let code = d.get("code").and_then(|c| {
                c.as_str()
                    .map(str::to_string)
                    .or_else(|| c.as_i64().map(|n| n.to_string()))
            });
            out.push(Diagnostic {
                file: path.clone(),
                line,
                col,
                severity,
                message,
                code,
            });
        }
    }
    diags.write().await.insert(path, out);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{file_uri_to_path, handle_diagnostics, percent_decode, Diagnostic};
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn percent_decode_handles_spaces_and_literals() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        // Malformed escape is left verbatim.
        assert_eq!(percent_decode("a%zz"), "a%zz");
    }

    #[test]
    fn file_uri_round_trips_unix_path() {
        let p = file_uri_to_path("file:///home/u/my%20proj/src.rs");
        assert_eq!(p, PathBuf::from("/home/u/my proj/src.rs"));
    }

    #[cfg(windows)]
    #[test]
    fn file_uri_drops_leading_slash_before_drive() {
        let p = file_uri_to_path("file:///C:/Users/x/main.rs");
        assert_eq!(p, PathBuf::from("C:/Users/x/main.rs"));
    }

    #[tokio::test]
    async fn handle_diagnostics_parses_publish_payload() {
        let diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let payload = json!({
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/a.rs",
                "diagnostics": [
                    {
                        "range": { "start": { "line": 9, "character": 4 } },
                        "severity": 1,
                        "message": "mismatched types",
                        "code": "E0308"
                    }
                ]
            }
        });
        handle_diagnostics(&payload, &diags).await;
        let got = diags.read().await.get(&PathBuf::from("/tmp/a.rs")).cloned().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 9);
        assert_eq!(got[0].col, 4);
        assert_eq!(got[0].severity, 1);
        assert_eq!(got[0].message, "mismatched types");
        assert_eq!(got[0].code.as_deref(), Some("E0308"));
    }

    #[tokio::test]
    async fn handle_diagnostics_defaults_missing_fields() {
        let diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        // No severity, no code, no range → severity defaults to 2 (warning),
        // line/col default to 0, and the key is still inserted (empty-but-present).
        let payload = json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///tmp/b.rs", "diagnostics": [ { "message": "bare" } ] }
        });
        handle_diagnostics(&payload, &diags).await;
        let got = diags.read().await.get(&PathBuf::from("/tmp/b.rs")).cloned().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].severity, 2);
        assert_eq!(got[0].line, 0);
        assert!(got[0].code.is_none());
    }
}
