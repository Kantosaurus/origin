// SPDX-License-Identifier: Apache-2.0
use origin_tools::{registry_iter, SideEffects, Tier, ToolMeta, Urgency};

// Register a tiny test tool using the macro.
origin_tools::origin_tool! {
    name: "registry-test-echo",
    description: "echoes input (test only)",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: origin_tools::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

#[test]
fn registry_contains_test_tool() {
    let names: Vec<&'static str> = registry_iter().map(|m: &ToolMeta| m.name).collect();
    assert!(
        names.contains(&"registry-test-echo"),
        "registry should include the test tool, got {names:?}"
    );
}

#[test]
fn tool_meta_fields_present() {
    let meta = registry_iter()
        .find(|m| m.name == "registry-test-echo")
        .expect("test tool must be registered");
    assert_eq!(meta.tier, Tier::AutoAllowed);
    assert_eq!(meta.urgency, Urgency::Low);
    assert_eq!(meta.side_effects, SideEffects::Pure);
    assert!(!meta.input_schema.is_empty());
}

#[test]
fn every_tool_has_nonzero_token_budget() {
    for meta in origin_tools::registry_iter() {
        assert!(meta.token_budget > 0, "tool {} has zero token_budget", meta.name);
    }
}

#[test]
fn task_is_autoallowed_and_hot_for_always_on_swarm() {
    let meta = registry_iter()
        .find(|m| m.name == "Task")
        .expect("Task must be registered");
    assert_eq!(
        meta.tier,
        Tier::AutoAllowed,
        "swarm delegation must not require a permission prompt",
    );
    assert!(meta.hot, "Task must be hot so the model can call it without ToolSearch");
}

#[test]
fn hot_set_contains_exactly_the_expected_tools() {
    let hot: Vec<&str> = origin_tools::registry_iter()
        .filter(|m| m.hot)
        .map(|m| m.name)
        .collect();
    let mut expected = vec![
        "Read",
        "Edit",
        "Write",
        "Grep",
        "Glob",
        "Bash",
        "MultiEdit",
        "ApplyPatch",
        "Monitor",
        "Diagnostics",
        "ToolSearch",
        // `Recall` is hot so the model can inflate a SchemaCrush offload handle
        // in one step (no ToolSearch round-trip first).
        "Recall",
        // `Task` is hot so swarm delegation is always one call away (no
        // ToolSearch round-trip) — the fix for swarm never being invoked.
        "Task",
        // `CollectTasks` is hot so the model can gather background sub-agent
        // results in-turn (background Task dispatch is the default).
        "CollectTasks",
    ];
    let mut got: Vec<&str> = hot.clone();
    got.sort_unstable();
    expected.sort_unstable();
    assert_eq!(got, expected, "hot set drifted");
}
