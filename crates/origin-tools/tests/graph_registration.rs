// SPDX-License-Identifier: Apache-2.0
use origin_tools::registry_iter;

#[test]
fn graph_tools_registered() {
    let names: Vec<&str> = registry_iter().map(|m| m.name).collect();
    for expected in [
        "graph_query",
        "graph_path",
        "graph_explain",
        "graph_summarize",
        "graph_rebuild",
        "ask",
    ] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
}

#[test]
fn graph_rebuild_requires_permission() {
    let m = registry_iter()
        .find(|m| m.name == "graph_rebuild")
        .expect("graph_rebuild registered");
    assert!(matches!(m.tier, origin_tools::Tier::RequiresPermission));
}
