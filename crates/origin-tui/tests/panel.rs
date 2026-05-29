// SPDX-License-Identifier: Apache-2.0
use origin_tools::Tier;
use origin_tui::panel::{Panel, PanelEvent, PermissionOutcome};

fn ask(id: u64, tool: &str, tier: Tier) -> PanelEvent {
    PanelEvent::PermissionAsk {
        id,
        tool: tool.into(),
        tier,
        args_preview: String::new(),
    }
}

#[test]
fn permission_ask_then_y_key_decides_allow() {
    let mut p = Panel::new();
    p.push(ask(1, "Read", Tier::AutoAllowed));
    let outcome = p.handle_key('y');
    assert_eq!(outcome, Some(PermissionOutcome::Allow));
}

#[test]
fn n_key_decides_deny() {
    let mut p = Panel::new();
    p.push(ask(1, "Bash", Tier::RequiresPermission));
    let outcome = p.handle_key('n');
    assert_eq!(outcome, Some(PermissionOutcome::Deny));
}

#[test]
fn unrelated_key_returns_none() {
    let mut p = Panel::new();
    p.push(ask(1, "Edit", Tier::RequiresPermission));
    assert_eq!(p.handle_key('q'), None);
}
