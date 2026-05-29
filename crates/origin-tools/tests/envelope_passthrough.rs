#![allow(clippy::unwrap_used)]

use origin_tools::tool_envelope::{run_passthrough, EnvelopeCtx};
use serde_json::json;

#[tokio::test]
async fn passthrough_returns_inner_value() {
    let ctx = EnvelopeCtx::default();
    let result = run_passthrough(&ctx, "Test", json!({}), |_args| async {
        Ok::<_, origin_tools::ToolError>(json!({"ok": true}))
    })
    .await
    .unwrap();
    assert_eq!(result["ok"], true);
}

#[tokio::test]
async fn passthrough_surfaces_tool_error_as_json() {
    let ctx = EnvelopeCtx::default();
    let result = run_passthrough(&ctx, "Test", json!({}), |_args| async {
        Err::<serde_json::Value, _>(origin_tools::ToolError::new(
            origin_tools::ErrClass::Edit,
            "no_match",
            "not found",
        ))
    })
    .await
    .unwrap();
    assert_eq!(result["kind"], "edit.no_match");
}
