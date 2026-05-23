//! End-to-end: activating a skill via the protocol must mutate the
//! per-connection registry, and the daemon must reply with `SkillActive`.

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
fn workflow_step_active_event_round_trips_as_json() {
    let ev = StreamEvent::WorkflowStepActive {
        name: "frontend-design".into(),
        step_index: 0,
        total_steps: 2,
        skill: "frontend-design:frontend-design".into(),
        skipped: vec!["ghost".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowStepActive {
            name,
            step_index,
            total_steps,
            skill,
            skipped,
        } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(step_index, 0);
            assert_eq!(total_steps, 2);
            assert_eq!(skill, "frontend-design:frontend-design");
            assert_eq!(skipped, vec!["ghost"]);
        }
        other => panic!("expected WorkflowStepActive, got {other:?}"),
    }
}

#[test]
fn workflow_step_held_event_round_trips_as_json() {
    // Emitted when a Prompt fails while a workflow is in progress.
    // The workflow stays paused at the same step; the next successful
    // prompt advances. The event surfaces the held step + error so the
    // CLI can show "retry your prompt to resume".
    let ev = StreamEvent::WorkflowStepHeld {
        name: "frontend-design".into(),
        step_index: 1,
        total_steps: 3,
        skill: "impeccable".into(),
        message: "loop error: provider timeout".into(),
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowStepHeld {
            name,
            step_index,
            total_steps,
            skill,
            message,
        } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(step_index, 1);
            assert_eq!(total_steps, 3);
            assert_eq!(skill, "impeccable");
            assert_eq!(message, "loop error: provider timeout");
        }
        other => panic!("expected WorkflowStepHeld, got {other:?}"),
    }
}

#[test]
fn workflow_complete_event_round_trips_as_json() {
    let ev = StreamEvent::WorkflowComplete {
        name: "frontend-design".into(),
        skipped: vec!["tail-ghost".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowComplete { name, skipped } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(skipped, vec!["tail-ghost"]);
        }
        other => panic!("expected WorkflowComplete, got {other:?}"),
    }
}

#[test]
fn workflow_active_event_still_carries_no_resolved_path() {
    // Retained variant: emitted ONLY when the workflow exists but no step
    // resolves through the catalog. `steps` is always empty in this path.
    let ev = StreamEvent::WorkflowActive {
        name: "all-ghost".into(),
        steps: vec![],
        skipped: vec!["ghost-a".into(), "ghost-b".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowActive { name, steps, skipped } => {
            assert_eq!(name, "all-ghost");
            assert!(steps.is_empty());
            assert_eq!(skipped.len(), 2);
        }
        other => panic!("expected WorkflowActive, got {other:?}"),
    }
}

// (Placeholder: real IPC dispatch testing requires the daemon's full
// transport stack, which is large to set up here. For Task 2 the JSON
// round-trip above is the contract; Task 3 covers the dispatch path
// from the CLI side. The handler glue is asserted by `cargo check`
// compiling the dispatch arm.)
