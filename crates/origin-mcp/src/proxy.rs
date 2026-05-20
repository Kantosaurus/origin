//! MCP tool proxy — implements [`origin_tools::DynTool`] via an [`McpClient`].
//!
//! The proxy is what the daemon's tool dispatcher walks over when it sees an MCP
//! tool, so MCP and native tools share the same code path.

use crate::cas_handoff::{cas_envelope, cas_handoff_if_large, HandoffOutcome};
use crate::client::{ClientError, McpClient};
use crate::schema::SchemaCache;
use async_trait::async_trait;
use origin_cas::Store as CasStore;
use origin_tools::{DynTool, ToolMeta};
use serde_json::Value;
use std::sync::Arc;

// `McpToolProxy` — `Mcp` prefix is the crate's naming convention even though
// it repeats `proxy`'s parent crate name; kept for symmetry with `McpClient`.
#[allow(clippy::module_name_repetitions)]
pub struct McpToolProxy {
    client: Arc<McpClient>,
    meta: ToolMeta,
    /// Server-side tool name (may differ from `meta.name` if we prefix with
    /// e.g. `mcp/<server>/` for namespacing).
    remote_name: String,
    cas: Option<Arc<CasStore>>,
    cas_threshold: usize,
    /// Per-server schema cache (P11.8). When set, [`invoke`] validates
    /// `args` against the registered schema for `remote_name` before
    /// calling the MCP server. Defaults to `None` so existing call-sites
    /// remain pass-through.
    schemas: Option<Arc<SchemaCache>>,
}

impl std::fmt::Debug for McpToolProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolProxy")
            .field("meta", &self.meta.name)
            .field("remote_name", &self.remote_name)
            .field("cas_threshold", &self.cas_threshold)
            .finish_non_exhaustive()
    }
}

impl McpToolProxy {
    #[must_use]
    pub const fn new(client: Arc<McpClient>, meta: ToolMeta, remote_name: String) -> Self {
        Self {
            client,
            meta,
            remote_name,
            cas: None,
            cas_threshold: 16 * 1024,
            schemas: None,
        }
    }

    /// Enable CAS hand-off for tool results exceeding `threshold` bytes.
    #[must_use]
    pub fn with_cas(mut self, store: Arc<CasStore>, threshold: usize) -> Self {
        self.cas = Some(store);
        self.cas_threshold = threshold;
        self
    }

    /// Attach a schema cache shared with the [`McpClient`]'s
    /// `tools/list` refresh. When set, [`invoke`] validates args against
    /// the registered schema before sending the JSON-RPC call.
    #[must_use]
    pub fn with_schemas(mut self, schemas: Arc<SchemaCache>) -> Self {
        self.schemas = Some(schemas);
        self
    }
}

#[async_trait]
impl DynTool for McpToolProxy {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }

    async fn invoke(&self, args: Value) -> Result<Value, String> {
        if let Some(cache) = &self.schemas {
            if let Err(e) = cache.validate(&self.remote_name, &args) {
                return Err(format!("mcp schema: {e}"));
            }
        }
        let result = match self.client.call_tool(&self.remote_name, args).await {
            Ok(r) => r,
            Err(ClientError::Rpc(e)) => return Err(format!("mcp rpc: {e}")),
            Err(other) => return Err(format!("mcp: {other}")),
        };
        if let Some(store) = &self.cas {
            match cas_handoff_if_large(store, result.content, self.cas_threshold) {
                Ok(HandoffOutcome::Inline(v)) => Ok(v),
                Ok(HandoffOutcome::Cas { handle, byte_len }) => Ok(cas_envelope(handle, byte_len)),
                Err(e) => Err(format!("cas: {e}")),
            }
        } else {
            Ok(result.content)
        }
    }
}
