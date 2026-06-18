# origin-lsp-client

> Minimal stdio JSON-RPC Language Server client for diagnostics

## Purpose

`origin-lsp-client` speaks just enough of the Language Server Protocol to power
origin's `Diagnostics` and code-navigation tools. It spawns a server as a child
process, completes the `initialize`/`initialized` handshake, opens documents,
listens for `publishDiagnostics`, and runs request/response round-trips
(`definition`, `references`, call hierarchy) correlated by JSON-RPC `id`. It is
the low-level transport that `origin-lspfleet` decides *which* server to drive.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `LspClient` | struct | A live server connection with a background reader loop. |
| `LspClient::spawn` / `spawn_with_args` | fn | Start the binary (optionally with `--stdio`-style args) and handshake against a workspace root. |
| `did_open` / `did_change` | fn | Notify the server of open/changed documents (full sync). |
| `diagnostics` | fn | Currently-known diagnostics for a path (or all). |
| `request` | fn | Generic id-correlated JSON-RPC request with a timeout. |
| `definition` / `references` | fn | Resolve symbol locations (0-based wire coords). |
| `incoming_calls` / `outgoing_calls` | fn | Call-hierarchy via prepare + incoming/outgoing. |
| `diagnose_file` | fn | One-shot probe: spawn → open → bounded poll → return `(client, diags)`. |
| `Diagnostic` / `Location` / `CallHierarchyItem` | struct | Result types. |
| `LspError` | enum | `Spawn` / `Io` / `Protocol` / `Timeout(Duration)`. |

## Key types

```rust
pub struct LspClient {
    _child: Child,                                  // kept alive; kill_on_drop
    stdin: Arc<Mutex<ChildStdin>>,
    diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    pending: PendingMap,                            // id → oneshot<Value>
    next_id: AtomicI64,                             // id 0 is the initialize handshake
}

pub struct Diagnostic {
    pub file: PathBuf, pub line: u32, pub col: u32,
    pub severity: u8,  // 1=error 2=warn 3=info 4=hint
    pub message: String, pub code: Option<String>,
}
```

## How it works

Each frame is `Content-Length: N\r\n\r\n<json>`. `spawn_with_args` pipes the
child's stdin/stdout, sets `kill_on_drop` (so a short diagnostics probe never
leaks a server), then sends `initialize` (id 0) and `initialized`. A background
`reader_loop` parses frames: `textDocument/publishDiagnostics` updates the
shared `diags` map; any frame carrying an `id` plus a `result`/`error` is routed
to the matching `oneshot` in `pending`; server→client request frames are
ignored. `request` allocates an id, registers a oneshot, writes the frame, and
awaits with a `tokio::time::timeout`. Location parsing tolerates the three wire
shapes (single `Location`, array, `LocationLink`), and `file://` URIs are
percent-decoded with Windows drive-letter handling (`/C:/x` → `C:\x`).

```
spawn ─▶ initialize(id 0) ─▶ initialized
            │
   ┌────────┴── reader_loop ──┬─ publishDiagnostics ─▶ diags map
   │                          └─ {id,result|error}  ─▶ pending[id].send
request(method) ─▶ pending[id] ─(timeout)─▶ result Value ─▶ parse_locations/items
```

## Dependencies & features

No cargo features. `tokio` (process, io-util, sync, time) drives the child and
the reader loop; `serde`/`serde_json` build and parse frames; `thiserror` for
`LspError`. A `MAX_BODY_BYTES` (64 MiB) cap guards against a hostile server
driving an unbounded allocation.

## Used by

```
crates/origin-daemon/Cargo.toml
crates/origin-lsp-client/Cargo.toml
```

## Testing

In-file async tests feed canned wire payloads through the routing helpers:
`handle_diagnostics` (full and defaulted fields), `parse_locations` (object,
array, `LocationLink`, `null`), call-hierarchy incoming/outgoing parsing,
`dispatch_response` (delivers results and error frames, ignores server→client
requests), and `percent_decode`/`file_uri_to_path` round-trips (including the
Windows drive form). `tempfile` provides workspace fixtures.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
