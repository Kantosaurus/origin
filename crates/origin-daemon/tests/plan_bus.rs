//! `PlanBus` — daemon-wide broadcast of plan `OpEnvelope`s to subscribed
//! IPC clients. The CLI's `plan_panel_wiring` is fed from this stream.

#![allow(clippy::panic)]

use origin_daemon::plan_bus::PlanBus;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use origin_plan::{ActorId, AddStep, Lamport, LogootKey, Op, OpEnvelope, StepId};

fn sample_envelope(seq: u64) -> OpEnvelope {
    let actor = ActorId::new(1);
    let id = StepId::from_u128(u128::from(seq));
    let key = LogootKey::between(None, None, actor, seq);
    OpEnvelope::new(
        actor,
        Lamport::new(seq),
        Op::AddStep(AddStep {
            id,
            parent: None,
            body: format!("step-{seq}"),
            key,
        }),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_receives_envelopes_published_after_subscribe() {
    let bus = PlanBus::new();
    let mut rx = bus.subscribe();
    bus.publish(sample_envelope(1));
    bus.publish(sample_envelope(2));
    let one = rx.recv().await.expect("first envelope");
    let two = rx.recv().await.expect("second envelope");
    match one.op {
        Op::AddStep(step) => assert_eq!(step.body, "step-1"),
        other => panic!("expected AddStep, got {other:?}"),
    }
    match two.op {
        Op::AddStep(step) => assert_eq!(step.body, "step-2"),
        other => panic!("expected AddStep, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn subscribe_does_not_observe_pre_subscribe_envelopes() {
    let bus = PlanBus::new();
    bus.publish(sample_envelope(99));
    let mut rx = bus.subscribe();
    // No new sends — receiver should not see the pre-subscribe envelope.
    let _: tokio::sync::broadcast::error::TryRecvError = rx
        .try_recv()
        .expect_err("late subscriber should not see prior events");
}

#[test]
fn subscribe_plan_message_round_trips() {
    let m = ClientMessage::SubscribePlan;
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"kind\":\"subscribe_plan\""), "json was: {s}");
    let back: ClientMessage = serde_json::from_str(&s).expect("de");
    assert!(matches!(back, ClientMessage::SubscribePlan));
}

#[test]
fn plan_op_event_round_trips() {
    let ev = StreamEvent::PlanOp {
        envelope: sample_envelope(7),
    };
    let s = serde_json::to_string(&ev).expect("ser");
    assert!(s.contains("\"kind\":\"plan_op\""), "json was: {s}");
    let _back: StreamEvent = serde_json::from_str(&s).expect("de");
}
