// SPDX-License-Identifier: Apache-2.0
//! Minimal stdio JSON-RPC client for Language Servers.
//!
//! Implements the subset needed by `Diagnostics` and code navigation:
//! `initialize`, `initialized`, `textDocument/didOpen`, `textDocument/didChange`,
//! `textDocument/didClose`, listening for `textDocument/publishDiagnostics`, and
//! request/response round-trips (`textDocument/definition`,
//! `textDocument/references`, `callHierarchy/*`) correlated by JSON-RPC `id`.
//! [`LspPool`] keeps servers alive across diagnostics calls.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
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

/// Latest published diagnostics per file, with the document `version` the
/// server tagged that publish with (`None` when it did not say).
type DiagMap = Arc<RwLock<HashMap<PathBuf, (Option<i64>, Vec<Diagnostic>)>>>;

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
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    diags: DiagMap,
    /// Outstanding request-id → response-oneshot correlation table.
    pending: PendingMap,
    /// Monotonic request id allocator. Id `0` is reserved for the `initialize`
    /// handshake, so request ids start at `1`.
    next_id: AtomicI64,
    /// Monotonic document version allocator shared by every document, so each
    /// `didOpen`/`didChange` carries a version newer than any sent before.
    next_version: AtomicI32,
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
        // A6: the server gets the allow-listed environment (PATH, CARGO_HOME,
        // RUSTUP_HOME, … stay), never the daemon's provider keys.
        origin_sandbox::scrub_env(cmd.as_std_mut());
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
        let diags: DiagMap = Arc::new(RwLock::new(HashMap::new()));
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
                // We send `workspace/didChangeWatchedFiles` for files we know
                // changed (see `LspPool::notify_changed`); no dynamic watchers.
                "capabilities": {
                    "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": false } }
                }
            }
        });
        write_frame(stdin.clone(), &init).await?;
        // initialized.
        let initd = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        write_frame(stdin.clone(), &initd).await?;

        Ok(Self {
            child,
            stdin,
            diags,
            pending,
            next_id: AtomicI64::new(1),
            next_version: AtomicI32::new(1),
        })
    }

    /// Notify the server about a file the client has open. Returns the
    /// document version sent.
    ///
    /// # Errors
    /// Returns `LspError::Io` if the write fails.
    pub async fn did_open(&self, path: &Path, language_id: &str, text: &str) -> Result<i32, LspError> {
        let uri = format!("file://{}", path.display().to_string().replace('\\', "/"));
        let version = self.next_version.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": version,
                    "text": text,
                }
            }
        });
        write_frame(self.stdin.clone(), &msg).await?;
        Ok(version)
    }

    /// Tell the server the client closed `path`; the server then reads the
    /// file from disk instead of the copy sent with `did_open`.
    async fn did_close(&self, path: &Path) -> Result<(), LspError> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": { "textDocument": { "uri": path_to_uri(path) } }
        });
        write_frame(self.stdin.clone(), &msg).await
    }

    /// `true` once the server process has exited (crashed or quit).
    fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    /// `true` if the server answers a request within `timeout`. LSP requires
    /// servers to reject `$/` requests they do not know with `MethodNotFound`,
    /// so any reply proves the server's main loop is alive.
    async fn ping(&self, timeout: Duration) -> bool {
        match self.request("$/origin/ping", Value::Null, timeout).await {
            Ok(_) => true,
            Err(LspError::Protocol(m)) => m.starts_with("server error"),
            Err(_) => false,
        }
    }

    /// Poll until the server publishes diagnostics for `path` tagged with
    /// `version` (or untagged), for at most `timeout`. Returns the diagnostics
    /// and whether the publish was untagged; `None` on timeout.
    async fn wait_for(
        &self,
        path: &Path,
        version: i32,
        timeout: Duration,
    ) -> Option<(Vec<Diagnostic>, bool)> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let published = self.diags.read().await.get(path).cloned();
            // A publish tagged with another version is a late answer to an
            // earlier call: returning it would report stale diagnostics.
            if let Some((v, diags)) = published {
                if v.is_none_or(|v| v == i64::from(version)) {
                    return Some((diags, v.is_none()));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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
                "textDocument": { "uri": uri, "version": self.next_version.fetch_add(1, Ordering::Relaxed) },
                "contentChanges": [ { "text": text } ]
            }
        });
        write_frame(self.stdin.clone(), &msg).await
    }

    /// Currently-known diagnostics for `path` (or all files when `None`).
    pub async fn diagnostics(&self, path: Option<&Path>) -> Vec<Diagnostic> {
        let g = self.diags.read().await;
        path.map_or_else(
            || g.values().flat_map(|(_, d)| d).cloned().collect(),
            |p| g.get(p).map(|(_, d)| d.clone()).unwrap_or_default(),
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
}

/// How long a server that missed the diagnostics deadline gets to answer a
/// liveness ping before it is treated as hung and dropped.
const PING_TIMEOUT: Duration = Duration::from_secs(1);

/// How long after spawn a server that has never answered counts as still
/// starting (indexing, loading the workspace) rather than hung, so a slow
/// starter is kept warming up instead of being killed on every call.
const STARTUP_GRACE: Duration = Duration::from_secs(60);

/// Consecutive startup failures (spawn error, or exit/eviction before it ever
/// answered) after which a program is not spawned again by this pool.
const MAX_STARTUP_FAILURES: u32 = 3;

/// Outcome of [`LspPool::diagnose`].
#[derive(Debug)]
pub enum Probe {
    /// The server published diagnostics for exactly the text just sent.
    Fresh(Vec<Diagnostic>),
    /// A publish arrived, but without a document version while the previous
    /// call for this file had timed out: it may answer that earlier edit.
    MaybeStale(Vec<Diagnostic>),
    /// Nothing was published within the deadline. `previous` is the last
    /// result the server reported for this file before this call (it may be
    /// stale; empty when there was none). `starting` is true when the server
    /// has not answered anything yet and is kept because it is still starting.
    TimedOut {
        previous: Vec<Diagnostic>,
        starting: bool,
    },
}

/// One pooled server.
struct Server {
    client: LspClient,
    spawned: std::time::Instant,
    /// Has published or answered a request at least once.
    ready: bool,
    /// Files whose last probe timed out: an untagged publish for them may be
    /// that probe's late answer.
    timed_out: std::collections::HashSet<PathBuf>,
}

#[derive(Default)]
struct PoolState {
    servers: HashMap<String, Server>,
    startup_failures: HashMap<String, u32>,
}

impl PoolState {
    /// Drop (kill) `program`'s server. One that never answered counts as a
    /// startup failure.
    fn evict(&mut self, program: &str) {
        if self.servers.remove(program).is_some_and(|s| !s.ready) {
            *self.startup_failures.entry(program.to_owned()).or_default() += 1;
        }
    }
}

/// Language servers kept alive across diagnostics calls, one per launch program.
///
/// A server stays warm instead of being spawned per call. One that exited is
/// respawned on the next call; one that misses a deadline AND a liveness ping
/// is dropped (killed), unless it is still within its startup grace. Dropping
/// the pool kills every server (`kill_on_drop`).
#[derive(Default)]
pub struct LspPool {
    state: Mutex<PoolState>,
}

impl LspPool {
    /// Tell every live server that `paths` changed on disk
    /// (`workspace/didChangeWatchedFiles`, type Changed), so a server does not
    /// keep analysing other files from a copy older than the agent's edits. A
    /// server whose input does not accept the write in time is dropped.
    pub async fn notify_changed(&self, paths: &[&Path]) {
        let changes: Vec<Value> = paths
            .iter()
            .map(|p| json!({ "uri": path_to_uri(p), "type": 2 }))
            .collect();
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": { "changes": changes }
        });
        let mut state = self.state.lock().await;
        let mut dead = Vec::new();
        for (program, s) in &state.servers {
            let sent = tokio::time::timeout(PING_TIMEOUT, write_frame(s.client.stdin.clone(), &msg)).await;
            if !matches!(sent, Ok(Ok(()))) {
                dead.push(program.clone());
            }
        }
        for program in dead {
            state.evict(&program);
        }
    }

    /// Drop (kill) every server; the next probe spawns a fresh one that reads
    /// the whole workspace from disk. Use when files changed in ways that
    /// cannot be reported path by path. Startup-failure counts are kept.
    pub async fn recycle(&self) {
        self.state.lock().await.servers.clear();
    }

    /// Diagnose `path`, whose current contents are `text`, with the pooled
    /// server launched as `program args` (spawned on first use).
    ///
    /// Each call re-syncs the document — `didOpen` with a new version, wait up
    /// to `timeout` for that version's publish, `didClose` — so the server
    /// never keeps a stale in-memory copy of it between calls.
    ///
    /// # Errors
    /// `LspError::Spawn` / `Protocol` / `Io` if the server cannot be started
    /// (or has failed to start [`MAX_STARTUP_FAILURES`] times in a row, after
    /// which it is no longer spawned), `Io` if the `didOpen` write fails (the
    /// server is dropped and respawned next call).
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub async fn diagnose(
        &self,
        program: &str,
        args: &[&str],
        workspace_root: &Path,
        path: &Path,
        language_id: &str,
        text: &str,
        timeout: Duration,
    ) -> Result<Probe, LspError> {
        let mut guard = self.state.lock().await;
        let state = &mut *guard;
        if state
            .servers
            .get_mut(program)
            .is_some_and(|s| s.client.has_exited())
        {
            state.evict(program);
        }
        let failures = state.startup_failures.get(program).copied().unwrap_or(0);
        let server = match state.servers.entry(program.to_owned()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(_) if failures >= MAX_STARTUP_FAILURES => {
                return Err(LspError::Spawn(format!(
                    "{program} failed to start {failures} times in a row; not restarting it this session"
                )));
            }
            Entry::Vacant(e) => match LspClient::spawn_with_args(program, args, workspace_root).await {
                Ok(client) => e.insert(Server {
                    client,
                    spawned: std::time::Instant::now(),
                    ready: false,
                    timed_out: std::collections::HashSet::new(),
                }),
                Err(err) => {
                    *state.startup_failures.entry(program.to_owned()).or_default() += 1;
                    return Err(err);
                }
            },
        };
        let previous = server
            .client
            .diags
            .write()
            .await
            .remove(path)
            .map(|(_, d)| d)
            .unwrap_or_default();
        let late_answer_possible = server.timed_out.remove(path);
        // Every write is bounded: a server that stopped reading its input would
        // otherwise block it forever once the pipe buffer is full.
        let version =
            match tokio::time::timeout(timeout, server.client.did_open(path, language_id, text)).await {
                Ok(Ok(version)) => version,
                Ok(Err(e)) => {
                    state.evict(program);
                    return Err(e);
                }
                Err(_) => {
                    state.evict(program);
                    return Ok(Probe::TimedOut {
                        previous,
                        starting: false,
                    });
                }
            };
        let published = server.client.wait_for(path, version, timeout).await;
        // On a miss, slow or hung? A live server answers the ping and is kept;
        // a hung or dead one is killed and respawned on the next call instead
        // of costing every call a timeout.
        let answered = tokio::time::timeout(PING_TIMEOUT, async {
            server.client.did_close(path).await.is_ok()
                && (published.is_some() || server.client.ping(PING_TIMEOUT).await)
        })
        .await
        .unwrap_or(false);
        if answered || published.is_some() {
            server.ready = true;
            state.startup_failures.remove(program);
        }
        // Never answered yet, but young and alive: still starting, keep it.
        let starting =
            !server.ready && server.spawned.elapsed() < STARTUP_GRACE && !server.client.has_exited();
        let Some((diags, untagged)) = published else {
            if answered || starting {
                server.timed_out.insert(path.to_path_buf());
            } else {
                state.evict(program);
            }
            drop(guard);
            return Ok(Probe::TimedOut { previous, starting });
        };
        if !answered {
            state.evict(program);
        }
        drop(guard);
        Ok(if untagged && late_answer_possible {
            Probe::MaybeStale(diags)
        } else {
            Probe::Fresh(diags)
        })
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
async fn reader_loop(stdout: ChildStdout, diags: DiagMap, pending: PendingMap) {
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

async fn handle_diagnostics(v: &Value, diags: &DiagMap) {
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
    let version = params.get("version").and_then(Value::as_i64);
    diags.write().await.insert(path, (version, out));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        dispatch_response, file_uri_to_path, handle_diagnostics, parse_call_hierarchy_items, parse_locations,
        percent_decode, DiagMap, Location, PendingMap,
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
        let diags: DiagMap = Arc::new(RwLock::new(HashMap::new()));
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
            .unwrap()
            .1;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].line, 9);
        assert_eq!(got[0].col, 4);
        assert_eq!(got[0].severity, 1);
        assert_eq!(got[0].message, "mismatched types");
        assert_eq!(got[0].code.as_deref(), Some("E0308"));
    }

    #[tokio::test]
    async fn handle_diagnostics_defaults_missing_fields() {
        let diags: DiagMap = Arc::new(RwLock::new(HashMap::new()));
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
            .unwrap()
            .1;
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
