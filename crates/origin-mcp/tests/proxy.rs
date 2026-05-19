use origin_mcp::{McpClient, McpToolProxy, Transport, TransportError};
use origin_tools::{DynTool, SideEffects, Tier, ToolMeta, Urgency};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Default)]
struct LoopbackTransport;

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    async fn round_trip(&self, request: &str) -> Result<Value, TransportError> {
        // Pretend every call returns the args echoed inside content.
        let v: Value = serde_json::from_str(request)?;
        let id = v.get("id").cloned().unwrap_or_else(|| json!(1));
        Ok(
            json!({"jsonrpc":"2.0","id":id,"result":{"echo": v.get("params").cloned().unwrap_or_else(|| json!({}))}}),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_proxy_invocation_runs_through_dyntool() {
    let transport: Arc<dyn Transport> = Arc::new(LoopbackTransport);
    let client = Arc::new(McpClient::new(transport));
    let proxy = McpToolProxy::new(
        client.clone(),
        ToolMeta {
            name: "mcp_echo",
            description: "echo via mcp",
            tier: Tier::RequiresPermission,
            urgency: Urgency::Low,
            side_effects: SideEffects::Pure,
            input_schema: "{\"type\":\"object\"}",
        },
        "echo".to_string(),
    );

    let dyn_tool: &dyn DynTool = &proxy;
    let result = dyn_tool.invoke(json!({"hello":"world"})).await.expect("invoke");
    assert!(result.get("echo").is_some(), "proxy should forward args");
}
