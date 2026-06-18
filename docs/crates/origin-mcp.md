# origin-mcp

> Model Context Protocol client with stdio/HTTP-SSE transports and OAuth

## Purpose

`origin-mcp` lets origin consume tools published by external Model Context
Protocol servers. It implements the JSON-RPC handshake, `tools/list`, and
`tools/call` over two transports — a stdio child process and an HTTP server with
SSE responses — and adapts each discovered tool into origin's runtime tool
object so MCP tools appear alongside builtins. Bearer credentials are bridged in
from `origin-keyvault`, and oversized results are offloaded to the CAS.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `McpClient` | struct | Handshake, `list_tools`, `call_tool`; correlates JSON-RPC by atomic `id`. |
| `McpTool` / `ListToolsResult` / `ToolCallResult` | struct | Decoded `tools/list` and `tools/call` payloads (`inputSchema` rename handled). |
| `ClientError` | enum | `Transport` / `Rpc` / `Decode` / `SchemaMismatch`. |
| `Transport` | trait | One method: `round_trip(request_json) -> Value`. |
| `StdioTransport` / `HttpTransport` | struct | The two concrete transports. |
| `TransportError` | enum | `Io` / `Serde` / `TooLarge { observed, cap }` / `Other`. |
| `McpToolProxy` | struct | Adapts an `McpClient` tool into origin's runtime `DynTool`. |
| `JsonRpcRequest` / `JsonRpcResponse` / `JsonRpcError` / `JsonRpcId` | struct/enum | JSON-RPC 2.0 frames. |
| `SchemaCache` / `ValidationError` | struct/enum | Caches + validates MCP tool input schemas. |
| `attach_bearer` / `OAuthBridgeError` | fn/enum | Reads a stored OAuth bearer from the vault onto an `HttpTransport`. |
| `cas_envelope` / `cas_handoff_if_large` / `HandoffOutcome` / `HandoffError` | fn/enum | Offload large tool results to CAS. |

## Key types

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    // MCP names this field `inputSchema`; the rename keeps the model from
    // losing every tool's parameter information.
    #[serde(rename = "inputSchema", alias = "input_schema", default = "default_schema")]
    pub input_schema: Value,
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError>;
}
```

## How it works

`McpClient` is transport-agnostic: it holds an `Arc<dyn Transport>` and an
`AtomicI64` id allocator, serialises a `JsonRpcRequest`, and hands the bytes to
the transport's `round_trip`. `StdioTransport` writes framed JSON to a child's
stdin and reads its stdout; `HttpTransport` POSTs and parses an
`eventsource-stream` SSE body, and exposes `set_bearer` for auth. Discovered
tools become `McpToolProxy` values implementing `DynTool`, so the dispatch layer
calls them exactly like builtins. The `limits` module caps response sizes
(`TransportError::TooLarge`), and `cas_handoff` swaps a bulky result for a CAS
envelope before it reaches the model.

```
keyvault ──attach_bearer──▶ HttpTransport.set_bearer
McpClient ──JsonRpcRequest──▶ Transport(round_trip) ──▶ stdio child | HTTP+SSE
        ◀──ListToolsResult/ToolCallResult── (schema-validated, CAS-offloaded)
```

## Dependencies & features

No cargo features. Builds on `origin-tools` (the `DynTool` target), `origin-cas`
(result hand-off), and `origin-keyvault` (OAuth bearer). Networking uses
`reqwest` + `eventsource-stream` + `futures-util`; schema validation uses
`jsonschema`; child processes and async use `tokio`. Dev-deps spin a real
`hyper` server to exercise the HTTP/SSE transport end to end.

## Used by

```
crates/origin-daemon/Cargo.toml
crates/origin-mcp/Cargo.toml
```

## Testing

Transport tests run against an in-process `hyper` server (HTTP/SSE) and a
spawned echo child (stdio). Client tests cover the handshake, the `inputSchema`
rename/alias, schema-mismatch errors, and the `TooLarge` cap. `tempfile` and
`tokio` test utilities support fixtures and deterministic timing.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
