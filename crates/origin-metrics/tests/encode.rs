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
