#![allow(clippy::unwrap_used, clippy::bool_assert_comparison)]

use origin_tools::tool_envelope::{run, EnvelopeCtx, EnvelopeMode};
use origin_tools::SideEffects;
use serde_json::json;

#[tokio::test]
async fn pure_tool_second_call_returns_ref() {
    let ctx = EnvelopeCtx::default();
    let r1 = run(
        &ctx,
        "Read",
        SideEffects::Pure,
        EnvelopeMode::CasEligible,
        json!({}),
        |_| async { Ok(json!({"body": "abc"})) },
    )
    .await
    .unwrap();
    assert_eq!(r1["body"], "abc");
    let r2 = run(
        &ctx,
        "Read",
        SideEffects::Pure,
        EnvelopeMode::CasEligible,
        json!({}),
        |_| async { Ok(json!({"body": "abc"})) },
    )
    .await
    .unwrap();
    assert_eq!(
        r2["tool_result_ref"].as_str().unwrap().starts_with("blake3:"),
        true
    );
}

#[tokio::test]
async fn mutating_tool_never_returns_ref() {
    let ctx = EnvelopeCtx::default();
    let r1 = run(
        &ctx,
        "Edit",
        SideEffects::Mutating,
        EnvelopeMode::CasEligible,
        json!({}),
        |_| async { Ok(json!({"ok": true})) },
    )
    .await
    .unwrap();
    let r2 = run(
        &ctx,
        "Edit",
        SideEffects::Mutating,
        EnvelopeMode::CasEligible,
        json!({}),
        |_| async { Ok(json!({"ok": true})) },
    )
    .await
    .unwrap();
    assert_eq!(r1, r2);
    assert!(r1.get("tool_result_ref").is_none());
}
