use origin_tools::{registry_iter, SideEffects, Tier, ToolMeta, Urgency};

// Register a tiny test tool using the macro.
origin_tools::origin_tool! {
    name: "registry-test-echo",
    description: "echoes input (test only)",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
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
