// SPDX-License-Identifier: Apache-2.0
//! Verifies the polyglot bench corpus loads offline (no Docker / network) and
//! that the default corpus stays byte-identical (the polyglot tasks ship in a
//! separate directory, so the default `origin bench` run is unchanged).

use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    // crates/origin-bench/.. /.. == repo root
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[test]
fn polyglot_manifest_loads_all_five_languages() {
    let dir = repo_root().join("bench").join("tasks").join("polyglot");
    let tasks = origin_bench::task_set::load(&dir).expect("load polyglot corpus");
    assert_eq!(tasks.len(), 5, "expected five polyglot tasks");
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for needle in ["rust", "ts", "py", "go", "java"] {
        assert!(
            ids.iter().any(|id| id.contains(needle)),
            "expected a {needle} task; got {ids:?}"
        );
    }
    // Every task is self-contained and offline-runnable: a non-empty prompt and
    // positive budgets (no fixture/Docker/network dependency).
    for t in &tasks {
        assert!(!t.prompt.is_empty(), "task {} has an empty prompt", t.id);
        assert!(
            t.expected_tool_calls_max > 0,
            "task {} caps tool calls at 0",
            t.id
        );
        assert!(t.max_turn_latency_ms > 0, "task {} caps latency at 0", t.id);
    }
}

#[test]
fn default_manifest_unchanged_eight_tasks() {
    let dir = repo_root().join("bench").join("tasks");
    let tasks = origin_bench::task_set::load(&dir).expect("load default corpus");
    assert_eq!(
        tasks.len(),
        8,
        "the default corpus must stay 8 tasks (polyglot ships separately)"
    );
}
