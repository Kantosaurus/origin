//! Plan side-panel widget tests (P9.9).
//!
//! Asserts:
//! - steps render in Logoot order with the right status glyph per
//!   on-disk [`origin_plan::Status`] (`Pending | InProgress | Done |
//!   Cancelled`);
//! - a leased step surfaces the holder `ActorId` on the matching line.
//!
//! The widget folds a private `Vec<OpEnvelope>` because the canonical
//! `origin_plan` surface materialises a [`origin_plan::Plan`] via
//! [`origin_plan::fold`] — there is no `Plan::apply` to call op-by-op.

use origin_plan::{
    ActorId, AddStep, Lamport, LeaseStep, LogootKey, MarkStep, Op, OpEnvelope, Status, StepId,
};
use origin_tui::widgets::plan_panel::PlanPanel;

fn add_op(actor: ActorId, lamport: u64, id: StepId, body: &str, key: LogootKey) -> OpEnvelope {
    OpEnvelope::new(
        actor,
        Lamport::new(lamport),
        Op::AddStep(AddStep {
            id,
            parent: None,
            body: body.into(),
            key,
        }),
    )
}

#[test]
fn renders_steps_in_logoot_order_with_glyphs() {
    let actor = ActorId::new(1);
    let id1 = StepId::from_u128(1);
    let id2 = StepId::from_u128(2);

    let k1 = LogootKey::between(None, None, actor, 1);
    let k2 = LogootKey::between(Some(&k1), None, actor, 2);

    let mut panel = PlanPanel::new();
    panel.apply_op(add_op(actor, 1, id1, "First", k1));
    panel.apply_op(add_op(actor, 2, id2, "Second", k2));
    panel.apply_op(OpEnvelope::new(
        actor,
        Lamport::new(3),
        Op::MarkStep(MarkStep {
            id: id1,
            status: Status::Done,
        }),
    ));

    let lines = panel.render();
    assert_eq!(lines.len(), 2, "expected two rendered lines");
    assert_eq!(lines[0].status_glyph, '●', "first step should be Done");
    assert_eq!(lines[0].content, "First");
    assert_eq!(lines[1].status_glyph, '○', "second step should be Pending");
    assert_eq!(lines[1].content, "Second");
}

#[test]
fn shows_lease_holder_when_present() {
    let a = ActorId::new(1);
    let b = ActorId::new(2);
    let id = StepId::from_u128(7);
    let k = LogootKey::between(None, None, a, 1);

    let mut panel = PlanPanel::new();
    panel.apply_op(add_op(a, 1, id, "Shared", k));
    panel.apply_op(OpEnvelope::new(
        b,
        Lamport::new(2),
        Op::LeaseStep(LeaseStep {
            step: id,
            expires_at_ms: u64::MAX,
        }),
    ));

    let lines = panel.render();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].holder, Some(b));
}
