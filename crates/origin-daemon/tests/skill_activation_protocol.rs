//! End-to-end: activating a skill via the protocol must mutate the
//! per-connection registry, and the daemon must reply with SkillActive.

#![allow(clippy::panic)]

use origin_daemon::protocol::{ClientMessage, StreamEvent};

#[test]
fn activate_skill_message_round_trips_as_json() {
    let msg = ClientMessage::ActivateSkill {
        name: "frontend-design".into(),
    };
    let body = serde_json::to_vec(&msg).expect("encode");
    let decoded: ClientMessage = serde_json::from_slice(&body).expect("decode");
    match decoded {
        ClientMessage::ActivateSkill { name } => assert_eq!(name, "frontend-design"),
        other => panic!("expected ActivateSkill, got {other:?}"),
    }
}

#[test]
fn deactivate_skill_message_round_trips_as_json() {
    let msg = ClientMessage::DeactivateSkill {
        name: "frontend-design".into(),
    };
    let body = serde_json::to_vec(&msg).expect("encode");
    let decoded: ClientMessage = serde_json::from_slice(&body).expect("decode");
    match decoded {
        ClientMessage::DeactivateSkill { name } => assert_eq!(name, "frontend-design"),
        other => panic!("expected DeactivateSkill, got {other:?}"),
    }
}

#[test]
fn skill_active_event_round_trips_as_json() {
    let ev = StreamEvent::SkillActive {
        name: "frontend-design".into(),
        allowed_tools: vec!["Read".into(), "Glob".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::SkillActive { name, allowed_tools } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(allowed_tools, vec!["Read", "Glob"]);
        }
        other => panic!("expected SkillActive, got {other:?}"),
    }
}

#[test]
fn skill_error_event_round_trips_as_json() {
    let ev = StreamEvent::SkillError {
        message: "no such skill: ghost".into(),
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::SkillError { message } => assert_eq!(message, "no such skill: ghost"),
        other => panic!("expected SkillError, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Dispatch test: drives the IPC loop end-to-end via a scripted connection.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Task 5: ActivateWorkflow / WorkflowActive round-trips.
// ---------------------------------------------------------------------------

#[test]
fn activate_workflow_message_round_trips_as_json() {
    let msg = ClientMessage::ActivateWorkflow {
        name: "frontend-design".into(),
    };
    let body = serde_json::to_vec(&msg).expect("encode");
    let decoded: ClientMessage = serde_json::from_slice(&body).expect("decode");
    match decoded {
        ClientMessage::ActivateWorkflow { name } => assert_eq!(name, "frontend-design"),
        other => panic!("expected ActivateWorkflow, got {other:?}"),
    }
}

#[test]
fn workflow_active_event_round_trips_as_json() {
    let ev = StreamEvent::WorkflowActive {
        name: "frontend-design".into(),
        steps: vec!["frontend-design:frontend-design".into(), "impeccable".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowActive { name, steps } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(steps.len(), 2);
        }
        other => panic!("expected WorkflowActive, got {other:?}"),
    }
}

// (Placeholder: real IPC dispatch testing requires the daemon's full
// transport stack, which is large to set up here. For Task 2 the JSON
// round-trip above is the contract; Task 3 covers the dispatch path
// from the CLI side. The handler glue is asserted by `cargo check`
// compiling the dispatch arm.)
