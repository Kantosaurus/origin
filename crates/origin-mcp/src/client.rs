//! `McpClient` — handshake, `list_tools`, `call_tool`.

use crate::jsonrpc::{JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use crate::transport::{Transport, TransportError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use thiserror::Error;

// `ClientError` repeats the module name `client`; suppressed so callers can
// write `origin_mcp::ClientError` without a module-disambiguating rename.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("rpc: {0}")]
    Rpc(#[from] JsonRpcError),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_schema")]
    pub input_schema: Value,
}

fn default_schema() -> Value {
    json!({"type":"object"})
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallResult {
    pub content: Value,
}

// `McpClient` — the `Mcp` prefix repeats the module name `client`, but the
// `Mcp*` prefix is the convention across this crate (see `McpTool`,
// `McpToolProxy`) so disambiguates well at use sites.
#[allow(clippy::module_name_repetitions)]
pub struct McpClient {
    transport: Arc<dyn Transport>,
    next_id: AtomicI64,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").finish_non_exhaustive()
    }
}

impl McpClient {
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            next_id: AtomicI64::new(1),
        }
    }

    fn fresh_id(&self) -> JsonRpcId {
        JsonRpcId::Num(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// MCP `initialize` handshake. Returns the server's reported tool-list.
    ///
    /// # Errors
    /// Returns [`ClientError`] on transport or RPC failure.
    pub async fn initialize(&self) -> Result<(), ClientError> {
        let req = JsonRpcRequest::new(
            self.fresh_id(),
            "initialize",
            json!({"protocolVersion":"2024-11-05","clientInfo":{"name":"origin","version":"0.0.1"}}),
        );
        let payload = serde_json::to_string(&req)?;
        let resp_value = self.transport.round_trip(&payload).await?;
        let resp: JsonRpcResponse = serde_json::from_value(resp_value)?;
        resp.into_result()?;
        Ok(())
    }

    /// `tools/list`.
    ///
    /// # Errors
    /// Returns [`ClientError`] on transport or RPC failure.
    pub async fn list_tools(&self) -> Result<ListToolsResult, ClientError> {
        let req = JsonRpcRequest::new(self.fresh_id(), "tools/list", json!({}));
        let payload = serde_json::to_string(&req)?;
        let resp_value = self.transport.round_trip(&payload).await?;
        let resp: JsonRpcResponse = serde_json::from_value(resp_value)?;
        let result = resp.into_result()?;
        Ok(serde_json::from_value(result)?)
    }

    /// `tools/call` with the given name and args.
    ///
    /// # Errors
    /// Returns [`ClientError`] on transport or RPC failure.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolCallResult, ClientError> {
        let req = JsonRpcRequest::new(
            self.fresh_id(),
            "tools/call",
            json!({"name": name, "arguments": args}),
        );
        let payload = serde_json::to_string(&req)?;
        let resp_value = self.transport.round_trip(&payload).await?;
        let resp: JsonRpcResponse = serde_json::from_value(resp_value)?;
        let result = resp.into_result()?;
        Ok(ToolCallResult { content: result })
    }
}
