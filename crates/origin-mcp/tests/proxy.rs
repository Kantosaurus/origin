use origin_mcp::{McpClient, McpToolProxy, SchemaCache, Transport, TransportError};
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

/// P11.8 — bad-shape args are rejected at the proxy boundary, before the
/// JSON-RPC call is ever issued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_proxy_rejects_args_violating_input_schema() {
    let transport: Arc<dyn Transport> = Arc::new(LoopbackTransport);
    let client = Arc::new(McpClient::new(transport));

    let schemas = Arc::new(SchemaCache::new());
    schemas
        .register(
            "echo",
            &json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        )
        .expect("register schema");

    let proxy = McpToolProxy::new(
        client,
        ToolMeta {
            name: "mcp_echo",
            description: "echo via mcp",
            tier: Tier::RequiresPermission,
            urgency: Urgency::Low,
            side_effects: SideEffects::Pure,
            input_schema: "{\"type\":\"object\"}",
        },
        "echo".to_string(),
    )
    .with_schemas(schemas);

    let dyn_tool: &dyn DynTool = &proxy;
    let err = dyn_tool
        .invoke(json!({})) // missing required `path`
        .await
        .expect_err("expected validation rejection");
    assert!(
        err.contains("schema"),
        "expected schema error message, got: {err}"
    );
}

/// P11.8 — valid args still flow through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_proxy_accepts_args_matching_input_schema() {
    let transport: Arc<dyn Transport> = Arc::new(LoopbackTransport);
    let client = Arc::new(McpClient::new(transport));

    let schemas = Arc::new(SchemaCache::new());
    schemas
        .register(
            "echo",
            &json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        )
        .expect("register schema");

    let proxy = McpToolProxy::new(
        client,
        ToolMeta {
            name: "mcp_echo",
            description: "echo via mcp",
            tier: Tier::RequiresPermission,
            urgency: Urgency::Low,
            side_effects: SideEffects::Pure,
            input_schema: "{\"type\":\"object\"}",
        },
        "echo".to_string(),
    )
    .with_schemas(schemas);

    let dyn_tool: &dyn DynTool = &proxy;
    let result = dyn_tool.invoke(json!({"path": "/tmp/x"})).await.expect("invoke");
    assert!(result.get("echo").is_some(), "proxy should forward args");
}
