//! P11.6 — `LifecycleEvent::PreTool` / `PostTool` carry the triggering
//! tool's sandbox `ProfileOrdinal`. Hook scripts receive the ordinal in
//! the event payload and can short-circuit when policy disagrees.

#![allow(clippy::panic)] // assertion via panic in test fallbacks is fine

use origin_hooks::event::{LifecycleEvent, ToolPhase};
use origin_sandbox::{ProfileOrdinal, SandboxProfile};

#[test]
fn pre_tool_event_carries_profile_ordinal() {
    let ev = LifecycleEvent::PreTool {
        tool: "Bash".into(),
        args_preview: "ls -la".into(),
        sandbox_ordinal: SandboxProfile::Shell.ordinal(),
    };
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(
        json.contains("\"sandbox_ordinal\":3"),
        "expected sandbox_ordinal=3 in JSON, got: {json}"
    );
    let parsed: LifecycleEvent = serde_json::from_str(&json).expect("round-trip");
    match parsed {
        LifecycleEvent::PreTool { sandbox_ordinal, .. } => assert_eq!(sandbox_ordinal, ProfileOrdinal(3)),
        other => panic!("expected PreTool, got {other:?}"),
    }
}

#[test]
fn post_tool_event_carries_profile_ordinal() {
    let ev = LifecycleEvent::PostTool {
        tool: "Edit".into(),
        phase: ToolPhase::Ok,
        sandbox_ordinal: SandboxProfile::WriteCwd.ordinal(),
    };
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(
        json.contains("\"sandbox_ordinal\":2"),
        "expected sandbox_ordinal=2 in JSON, got: {json}"
    );
}

#[test]
fn inherit_ordinal_is_zero() {
    let ev = LifecycleEvent::PreTool {
        tool: "Glob".into(),
        args_preview: "**/*.rs".into(),
        sandbox_ordinal: SandboxProfile::Inherit.ordinal(),
    };
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(json.contains("\"sandbox_ordinal\":0"), "got: {json}");
}
