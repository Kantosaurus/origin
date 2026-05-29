// SPDX-License-Identifier: Apache-2.0
use origin_metrics::Metrics;
use origin_tui::widgets::metrics::MetricsWidget;
use origin_tui::{Panel, PanelEvent, PanelState};

#[test]
fn snapshot_contains_every_registered_metric() {
    let m = Metrics::new();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    m.tokens_in_total("anthropic", "claude-opus-4-7").inc_by(10);
    let widget = MetricsWidget::new(&m);
    let lines = widget.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("origin_tool_call_total") && l.contains("Bash")),
        "missing tool_call_total Bash row in: {lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("origin_tokens_in_total") && l.contains("claude-opus-4-7")),
        "missing tokens_in_total row in: {lines:#?}"
    );
}

#[test]
fn question_mark_toggles_metrics_state() {
    let mut p = Panel::new();
    assert_eq!(p.state(), PanelState::PermissionQueue);
    let consumed = p.handle_key('?');
    assert!(consumed.is_none());
    assert_eq!(p.state(), PanelState::Metrics);
    // Toggles back.
    p.handle_key('?');
    assert_eq!(p.state(), PanelState::PermissionQueue);
}

#[test]
fn show_metrics_event_does_not_pollute_queue() {
    let mut p = Panel::new();
    p.push(PanelEvent::ShowMetrics);
    assert_eq!(p.state(), PanelState::Metrics);
    // A subsequent permission ask still gets queued normally.
    p.push(PanelEvent::PermissionAsk {
        id: 1,
        tool: "Bash".into(),
        tier: origin_tools::Tier::RequiresPermission,
        args_preview: "ls".into(),
    });
    // The y key still resolves to Allow.
    let outcome = p.handle_key('y');
    assert!(matches!(outcome, Some(origin_tui::PermissionOutcome::Allow)));
}
