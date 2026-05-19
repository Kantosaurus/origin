//! MCP tool proxy — implements [`origin_tools::DynTool`] via an [`McpClient`].
//!
//! The proxy is what the daemon's tool dispatcher walks over when it sees an MCP
//! tool, so MCP and native tools share the same code path.

use crate::client::{ClientError, McpClient};
use async_trait::async_trait;
use origin_tools::{DynTool, ToolMeta};
use serde_json::Value;
use std::sync::Arc;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct McpToolProxy {
    client: Arc<McpClient>,
    meta: ToolMeta,
    /// Server-side tool name (may differ from `meta.name` if we prefix with
    /// e.g. `mcp/<server>/` for namespacing).
    remote_name: String,
}

impl McpToolProxy {
    #[must_use]
    pub const fn new(client: Arc<McpClient>, meta: ToolMeta, remote_name: String) -> Self {
        Self {
            client,
            meta,
            remote_name,
        }
    }
}

#[async_trait]
impl DynTool for McpToolProxy {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }

    async fn invoke(&self, args: Value) -> Result<Value, String> {
        match self.client.call_tool(&self.remote_name, args).await {
            Ok(r) => Ok(r.content),
            Err(ClientError::Rpc(e)) => Err(format!("mcp rpc: {e}")),
            Err(other) => Err(format!("mcp: {other}")),
        }
    }
}
