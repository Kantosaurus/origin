// SPDX-License-Identifier: Apache-2.0
//! Minimal stdio JSON-RPC client for Language Servers.
//!
//! Implements the subset needed by `Diagnostics` and code navigation:
//! `initialize`, `initialized`, `textDocument/didOpen`, `textDocument/didChange`,
//! listening for `textDocument/publishDiagnostics`, and request/response
//! round-trips (`textDocument/definition`, `textDocument/references`,
//! `callHierarchy/*`) correlated by JSON-RPC `id`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex, RwLock};

/// Map of outstanding JSON-RPC request ids to the oneshot that delivers their
/// response frame. Shared between [`LspClient`] and its [`reader_loop`].
pub(crate) type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
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

/// A resolved source location (0-based line/column, exactly as it arrives on the
/// LSP wire). The tool layer is responsible for converting to 1-based display
/// coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
}

/// One node in a call-hierarchy result (a caller for incoming calls, a callee
/// for outgoing calls). Line/column are 0-based wire coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyItem {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
}

pub struct LspClient {
    /// Holds the child process alive for the lifetime of the client.
    _child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    /// Outstanding request-id → response-oneshot correlation table.
    pending: PendingMap,
    /// Monotonic request id allocator. Id `0` is reserved for the `initialize`
    /// handshake, so request ids start at `1`.
    next_id: AtomicI64,
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
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // Reader loop.
        let diags_clone = diags.clone();
        let pending_clone = pending.clone();
        tokio::spawn(reader_loop(stdout, diags_clone, pending_clone));

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
            pending,
            next_id: AtomicI64::new(1),
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

    /// Issue a JSON-RPC request and await its correlated response.
    ///
    /// Allocates a fresh id, registers a oneshot in the pending table, writes the
    /// request frame, then waits up to `timeout` for [`reader_loop`] to route the
    /// matching response back. On success the `result` field of the response is
    /// returned (`Value::Null` if the server replied with an empty result); a
    /// server-side `error` object is surfaced as [`LspError::Protocol`].
    ///
    /// # Errors
    /// * [`LspError::Timeout`] if no response arrives within `timeout`.
    /// * [`LspError::Protocol`] if the channel closes early (server died) or the
    ///   response carries an `error`.
    /// * [`LspError::Io`] if writing the request frame fails.
    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, LspError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(e) = write_frame(self.stdin.clone(), &frame).await {
            // Drop the now-unanswerable pending entry before bubbling up.
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => {
                if let Some(err) = resp.get("error") {
                    return Err(LspError::Protocol(format!("server error: {err}")));
                }
                Ok(resp.get("result").cloned().unwrap_or(Value::Null))
            }
            // Sender dropped without sending (server exited / loop ended).
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                Err(LspError::Protocol("response channel closed".into()))
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(LspError::Timeout(timeout))
            }
        }
    }

    /// Resolve the definition(s) of the symbol at 0-based (`line`, `col`) in `path`.
    ///
    /// Opens `path` (best-effort `did_open` with `text`) then issues
    /// `textDocument/definition`. Returns an empty `Vec` when the server lacks the
    /// capability (replies `null`).
    ///
    /// # Errors
    /// Propagates [`request`](Self::request) failures (timeout / io / protocol).
    pub async fn definition(
        &self,
        path: &Path,
        language_id: &str,
        text: &str,
        line: u32,
        col: u32,
        timeout: Duration,
    ) -> Result<Vec<Location>, LspError> {
        self.did_open(path, language_id, text).await?;
        let params = json!({
            "textDocument": { "uri": path_to_uri(path) },
            "position": { "line": line, "character": col },
        });
        let result = self.request("textDocument/definition", params, timeout).await?;
        Ok(parse_locations(&result))
    }

    /// Find references to the symbol at 0-based (`line`, `col`) in `path`.
    ///
    /// `include_declaration` maps to `context.includeDeclaration`. Returns an
    /// empty `Vec` when the server has no references / lacks the capability.
    ///
    /// # Errors
    /// Propagates [`request`](Self::request) failures (timeout / io / protocol).
    #[allow(clippy::too_many_arguments)]
    pub async fn references(
        &self,
        path: &Path,
        language_id: &str,
        text: &str,
        line: u32,
        col: u32,
        include_declaration: bool,
        timeout: Duration,
    ) -> Result<Vec<Location>, LspError> {
        self.did_open(path, language_id, text).await?;
        let params = json!({
            "textDocument": { "uri": path_to_uri(path) },
            "position": { "line": line, "character": col },
            "context": { "includeDeclaration": include_declaration },
        });
        let result = self.request("textDocument/references", params, timeout).await?;
        Ok(parse_locations(&result))
    }

    /// Callers of the symbol at 0-based (`line`, `col`) in `path`.
    ///
    /// Performs the two-step LSP dance: `textDocument/prepareCallHierarchy` to
    /// obtain an item, then `callHierarchy/incomingCalls`. Returns an empty `Vec`
    /// when the server cannot prepare an item (no capability / not a symbol).
    ///
    /// # Errors
    /// Propagates [`request`](Self::request) failures (timeout / io / protocol).
    pub async fn incoming_calls(
        &self,
        path: &Path,
        language_id: &str,
        text: &str,
        line: u32,
        col: u32,
        timeout: Duration,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        self.call_hierarchy(path, language_id, text, line, col, true, timeout)
            .await
    }

    /// Callees of the symbol at 0-based (`line`, `col`) in `path`.
    ///
    /// Performs `textDocument/prepareCallHierarchy` then
    /// `callHierarchy/outgoingCalls`. Returns an empty `Vec` when the server
    /// cannot prepare an item.
    ///
    /// # Errors
    /// Propagates [`request`](Self::request) failures (timeout / io / protocol).
    pub async fn outgoing_calls(
        &self,
        path: &Path,
        language_id: &str,
        text: &str,
        line: u32,
        col: u32,
        timeout: Duration,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        self.call_hierarchy(path, language_id, text, line, col, false, timeout)
            .await
    }

    /// Shared body for [`incoming_calls`](Self::incoming_calls) /
    /// [`outgoing_calls`](Self::outgoing_calls). `incoming == true` selects
    /// `callHierarchy/incomingCalls` (whose nodes are under `from`), otherwise
    /// `callHierarchy/outgoingCalls` (nodes under `to`).
    #[allow(clippy::too_many_arguments)]
    async fn call_hierarchy(
        &self,
        path: &Path,
        language_id: &str,
        text: &str,
        line: u32,
        col: u32,
        incoming: bool,
        timeout: Duration,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        self.did_open(path, language_id, text).await?;
        let prepare_params = json!({
            "textDocument": { "uri": path_to_uri(path) },
            "position": { "line": line, "character": col },
        });
        let prepared = self
            .request("textDocument/prepareCallHierarchy", prepare_params, timeout)
            .await?;
        // `prepareCallHierarchy` returns an array of items (or null). Take the
        // first; without one the server cannot answer the follow-up.
        let Some(item) = prepared.as_array().and_then(|a| a.first()).cloned() else {
            return Ok(Vec::new());
        };
        let (method, key) = if incoming {
            ("callHierarchy/incomingCalls", "from")
        } else {
            ("callHierarchy/outgoingCalls", "to")
        };
        let result = self.request(method, json!({ "item": item }), timeout).await?;
        Ok(parse_call_hierarchy_items(&result, key))
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

/// Build a `file://` URI for `path`, matching the form the server expects and the
/// keys [`file_uri_to_path`] round-trips.
fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display().to_string().replace('\\', "/"))
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

/// Background task that reads frames from the server. Diagnostics notifications
/// update `diags`; correlated responses (frames carrying an integer `id` plus a
/// `result`/`error`) are routed to the matching oneshot in `pending`. Server→
/// client request frames (method + id, no result) are ignored without error.
async fn reader_loop(
    stdout: ChildStdout,
    diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    pending: PendingMap,
) {
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
        } else {
            dispatch_response(&v, &pending).await;
        }
    }
}

/// Route a non-diagnostics frame. If `v` carries an integer `id` and a `result`
/// or `error`, pop the matching oneshot from `pending` and deliver the whole
/// frame to it. Frames with a `method` but no `result`/`error` are server→client
/// requests/notifications and are ignored. The delivered `Value` is the full
/// frame so the receiver can read `result` or `error`.
pub(crate) async fn dispatch_response(v: &Value, pending: &PendingMap) {
    // Server->client request frames (method + id, no result/error) carry a
    // method; ignore them. Genuine responses have no `method`.
    let has_result = v.get("result").is_some() || v.get("error").is_some();
    if !has_result {
        return;
    }
    let Some(id) = v.get("id").and_then(Value::as_i64) else {
        return;
    };
    let sender = pending.lock().await.remove(&id);
    if let Some(tx) = sender {
        // If the receiver was dropped (caller timed out) the send fails; that is
        // expected and harmless.
        let _ = tx.send(v.clone());
    }
}

/// Parse a `textDocument/definition` / `references` result into [`Location`]s.
///
/// Handles all three wire shapes plus the empty cases:
///   * a single `Location` object (`{ uri, range }`);
///   * an array of `Location`;
///   * an array of `LocationLink` (`{ targetUri, targetRange }`);
///   * `null` / anything else → empty `Vec`.
pub(crate) fn parse_locations(v: &Value) -> Vec<Location> {
    match v {
        Value::Array(arr) => arr.iter().filter_map(loc_from_value).collect(),
        Value::Object(_) => loc_from_value(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Extract a single [`Location`] from a `Location` or `LocationLink` object.
fn loc_from_value(v: &Value) -> Option<Location> {
    // `Location` uses uri/range; `LocationLink` uses targetUri/targetRange.
    let uri = v
        .get("uri")
        .and_then(Value::as_str)
        .or_else(|| v.get("targetUri").and_then(Value::as_str))?;
    let range = v.get("range").or_else(|| v.get("targetRange"))?;
    let (line, col) = range_start(range);
    Some(Location {
        file: file_uri_to_path(uri),
        line,
        col,
    })
}

/// Parse a `callHierarchy/incomingCalls` (`key = "from"`) or `outgoingCalls`
/// (`key = "to"`) result into [`CallHierarchyItem`]s. Non-array / empty inputs
/// yield an empty `Vec`.
pub(crate) fn parse_call_hierarchy_items(v: &Value, key: &str) -> Vec<CallHierarchyItem> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let item = entry.get(key)?;
            let name = item.get("name").and_then(Value::as_str)?.to_string();
            let uri = item.get("uri").and_then(Value::as_str)?;
            // Prefer the precise `selectionRange`; fall back to the full `range`.
            let range = item.get("selectionRange").or_else(|| item.get("range"))?;
            let (line, col) = range_start(range);
            Some(CallHierarchyItem {
                name,
                file: file_uri_to_path(uri),
                line,
                col,
            })
        })
        .collect()
}

/// Read the 0-based `(line, character)` from an LSP `range.start`, saturating to
/// `u32::MAX` on overflow and defaulting absent fields to `0`.
fn range_start(range: &Value) -> (u32, u32) {
    let line =
        u32::try_from(range.pointer("/start/line").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(u32::MAX);
    let col = u32::try_from(
        range
            .pointer("/start/character")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
    .unwrap_or(u32::MAX);
    (line, col)
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
    use super::{
        dispatch_response, file_uri_to_path, handle_diagnostics, parse_call_hierarchy_items, parse_locations,
        percent_decode, Diagnostic, Location, PendingMap,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::{Mutex, RwLock};

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
        let diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> = Arc::new(RwLock::new(HashMap::new()));
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
        let got = diags
            .read()
            .await
            .get(&PathBuf::from("/tmp/a.rs"))
            .cloned()
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 9);
        assert_eq!(got[0].col, 4);
        assert_eq!(got[0].severity, 1);
        assert_eq!(got[0].message, "mismatched types");
        assert_eq!(got[0].code.as_deref(), Some("E0308"));
    }

    #[tokio::test]
    async fn handle_diagnostics_defaults_missing_fields() {
        let diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> = Arc::new(RwLock::new(HashMap::new()));
        // No severity, no code, no range → severity defaults to 2 (warning),
        // line/col default to 0, and the key is still inserted (empty-but-present).
        let payload = json!({
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///tmp/b.rs", "diagnostics": [ { "message": "bare" } ] }
        });
        handle_diagnostics(&payload, &diags).await;
        let got = diags
            .read()
            .await
            .get(&PathBuf::from("/tmp/b.rs"))
            .cloned()
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].severity, 2);
        assert_eq!(got[0].line, 0);
        assert!(got[0].code.is_none());
    }

    #[test]
    fn parse_locations_single_object() {
        // `textDocument/definition` may return a single `Location` object.
        let v = json!({
            "uri": "file:///tmp/a.rs",
            "range": { "start": { "line": 4, "character": 8 }, "end": { "line": 4, "character": 12 } }
        });
        let locs = parse_locations(&v);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, PathBuf::from("/tmp/a.rs"));
        assert_eq!(locs[0].line, 4);
        assert_eq!(locs[0].col, 8);
    }

    #[test]
    fn parse_locations_array_of_references() {
        // `textDocument/references` returns an array of `Location`.
        let v = json!([
            {
                "uri": "file:///tmp/a.rs",
                "range": { "start": { "line": 1, "character": 2 } }
            },
            {
                "uri": "file:///tmp/b.rs",
                "range": { "start": { "line": 30, "character": 0 } }
            }
        ]);
        let locs = parse_locations(&v);
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].file, PathBuf::from("/tmp/a.rs"));
        assert_eq!(locs[0].line, 1);
        assert_eq!(locs[0].col, 2);
        assert_eq!(locs[1].file, PathBuf::from("/tmp/b.rs"));
        assert_eq!(locs[1].line, 30);
        assert_eq!(locs[1].col, 0);
    }

    #[test]
    fn parse_locations_location_link() {
        // `LocationLink` carries `targetUri` + `targetRange` instead of `uri`/`range`.
        let v = json!([
            {
                "targetUri": "file:///tmp/c.rs",
                "targetRange": { "start": { "line": 7, "character": 3 } },
                "targetSelectionRange": { "start": { "line": 7, "character": 3 } }
            }
        ]);
        let locs = parse_locations(&v);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, PathBuf::from("/tmp/c.rs"));
        assert_eq!(locs[0].line, 7);
        assert_eq!(locs[0].col, 3);
    }

    #[test]
    fn parse_locations_null_is_empty() {
        // A server lacking the capability returns `null` → empty, not an error.
        assert!(parse_locations(&json!(null)).is_empty());
    }

    #[test]
    fn parse_call_hierarchy_incoming_calls() {
        // `callHierarchy/incomingCalls` returns `[{ from: CallHierarchyItem, ... }]`.
        let v = json!([
            {
                "from": {
                    "name": "caller_fn",
                    "kind": 12,
                    "uri": "file:///tmp/caller.rs",
                    "range": { "start": { "line": 11, "character": 0 } },
                    "selectionRange": { "start": { "line": 11, "character": 3 } }
                },
                "fromRanges": []
            }
        ]);
        let items = parse_call_hierarchy_items(&v, "from");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "caller_fn");
        assert_eq!(items[0].file, PathBuf::from("/tmp/caller.rs"));
        // Prefers selectionRange (col 3) over range (col 0).
        assert_eq!(items[0].line, 11);
        assert_eq!(items[0].col, 3);
    }

    #[test]
    fn parse_call_hierarchy_outgoing_calls() {
        // `callHierarchy/outgoingCalls` returns `[{ to: CallHierarchyItem, ... }]`.
        let v = json!([
            {
                "to": {
                    "name": "callee_fn",
                    "uri": "file:///tmp/callee.rs",
                    "range": { "start": { "line": 2, "character": 4 } },
                    "selectionRange": { "start": { "line": 2, "character": 4 } }
                },
                "fromRanges": []
            }
        ]);
        let items = parse_call_hierarchy_items(&v, "to");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "callee_fn");
        assert_eq!(items[0].file, PathBuf::from("/tmp/callee.rs"));
        assert_eq!(items[0].line, 2);
        assert_eq!(items[0].col, 4);
    }

    #[tokio::test]
    async fn dispatch_response_delivers_definition_to_pending() {
        // Feed a canned `textDocument/definition` response Value through the
        // id-routing path and assert the registered oneshot is delivered, then
        // parse the delivered result into a `Location`.
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = tokio::sync::oneshot::channel();
        pending.lock().await.insert(7, tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "uri": "file:///tmp/def.rs",
                "range": { "start": { "line": 5, "character": 9 } }
            }
        });
        dispatch_response(&frame, &pending).await;

        // The oneshot must have fired and the pending entry removed.
        assert!(pending.lock().await.is_empty());
        let delivered = rx.await.unwrap();
        let result = delivered.get("result").unwrap();
        let locs = parse_locations(result);
        assert_eq!(locs.len(), 1);
        assert_eq!(
            locs[0],
            Location {
                file: PathBuf::from("/tmp/def.rs"),
                line: 5,
                col: 9
            }
        );
    }

    #[tokio::test]
    async fn dispatch_response_ignores_server_request_frames() {
        // A server->client request (method + id, NO result/error) must be
        // ignored without erroring and without touching pending.
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
        pending.lock().await.insert(3, tx);

        let frame = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "window/workDoneProgress/create",
            "params": {}
        });
        dispatch_response(&frame, &pending).await;
        // Pending entry untouched because there was no result/error.
        assert_eq!(pending.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn dispatch_response_delivers_error_frame() {
        // An error frame still resolves the pending oneshot (caller decides).
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = tokio::sync::oneshot::channel();
        pending.lock().await.insert(9, tx);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": { "code": -32601, "message": "method not found" }
        });
        dispatch_response(&frame, &pending).await;
        assert!(pending.lock().await.is_empty());
        let delivered = rx.await.unwrap();
        assert!(delivered.get("error").is_some());
    }
}
