// SPDX-License-Identifier: Apache-2.0
use origin_metrics::{Metrics, Snapshot};

#[test]
fn counter_increments_and_encodes_as_prom_text() {
    let m = Metrics::new();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    m.tool_call_total("anthropic", "Edit", "err").inc();

    let text = m.encode_text().expect("encode");
    // Prometheus's text encoder sorts label names alphabetically:
    // provider, result, tool (not the declaration order).
    assert!(
        text.contains("origin_tool_call_total{provider=\"anthropic\",result=\"ok\",tool=\"Bash\"} 2"),
        "got: {text}"
    );
    assert!(text.contains("origin_tool_call_total{provider=\"anthropic\",result=\"err\",tool=\"Edit\"} 1"));
}

#[test]
fn token_accounting_observes_per_provider() {
    let m = Metrics::new();
    m.tokens_in_total("anthropic", "claude-opus-4-7").inc_by(120);
    m.tokens_out_total("anthropic", "claude-opus-4-7").inc_by(85);
    let text = m.encode_text().expect("encode");
    // Labels are emitted alphabetically (model, provider).
    assert!(text.contains("origin_tokens_in_total{model=\"claude-opus-4-7\",provider=\"anthropic\"} 120"));
    assert!(text.contains("origin_tokens_out_total{model=\"claude-opus-4-7\",provider=\"anthropic\"} 85"));
}

#[test]
fn snapshot_returns_every_registered_metric() {
    let m = Metrics::new();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    let snap: Snapshot = m.snapshot();
    let bash_ok = snap
        .iter()
        .find(|s| {
            s.name == "origin_tool_call_total" && s.labels.iter().any(|(k, v)| k == "tool" && v == "Bash")
        })
        .expect("Bash metric in snapshot");
    assert!((bash_ok.value - 1.0).abs() < f64::EPSILON);
}

#[test]
fn fast_index_keeps_distinct_rows_for_distinct_label_values() {
    // Bug-1 regression: two registrations of the same counter family with
    // identical label *names* but different label *values* must produce
    // two distinct rows in the fast index — they encode as two distinct
    // Prometheus series. A faulty canonical key that uses only label
    // *names* (and not values) would lump them into a single row.
    let m = Metrics::new();
    m.tokens_in_total("anthropic", "model-a").inc_by(5);
    m.tokens_in_total("anthropic", "model-b").inc_by(7);
    let text = m.encode_text().expect("encode");
    assert!(
        text.contains("origin_tokens_in_total{model=\"model-a\",provider=\"anthropic\"} 5"),
        "missing model-a row: {text}"
    );
    assert!(
        text.contains("origin_tokens_in_total{model=\"model-b\",provider=\"anthropic\"} 7"),
        "missing model-b row: {text}"
    );
    // Snapshot must also reflect both distinct series.
    let snap = m.snapshot();
    let in_rows: Vec<&_> = snap
        .iter()
        .filter(|r| r.name == "origin_tokens_in_total")
        .collect();
    assert_eq!(in_rows.len(), 2, "expected two distinct rows, got {in_rows:?}");
}

#[test]
fn cardinality_is_bounded() {
    // Unknown tool names collapse into `_other_` via the label allowlist.
    let m = Metrics::new();
    for i in 0..50 {
        let tool = Box::leak(format!("UnknownTool{i}").into_boxed_str()) as &'static str;
        m.tool_call_total("anthropic", tool, "ok").inc();
    }
    let text = m.encode_text().expect("encode");
    let lines = text
        .lines()
        .filter(|l| l.contains("origin_tool_call_total{"))
        .count();
    assert!(lines <= 25, "expected <=25 distinct label tuples, got {lines}");
}
