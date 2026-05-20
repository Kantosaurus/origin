#![allow(clippy::panic)]

use origin_hooks::{parse_hook_stdout, HookOverride, LifecycleEvent, ToolPhase};
use origin_sandbox::SandboxProfile;

#[test]
fn lifecycle_event_round_trips_json() {
    let ev = LifecycleEvent::PreTool {
        tool: "Bash".into(),
        args_preview: "ls -la".into(),
        sandbox_ordinal: SandboxProfile::Shell.ordinal(),
    };
    let json = serde_json::to_string(&ev).expect("ser");
    let back: LifecycleEvent = serde_json::from_str(&json).expect("de");
    match back {
        LifecycleEvent::PreTool {
            tool,
            args_preview,
            sandbox_ordinal,
        } => {
            assert_eq!(tool, "Bash");
            assert_eq!(args_preview, "ls -la");
            assert_eq!(sandbox_ordinal, SandboxProfile::Shell.ordinal());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn every_event_kind_serializes() {
    let evs = vec![
        LifecycleEvent::PrePrompt { text: "hi".into() },
        LifecycleEvent::PostPrompt { text: "bye".into() },
        LifecycleEvent::PreTool {
            tool: "Read".into(),
            args_preview: "/x".into(),
            sandbox_ordinal: SandboxProfile::ReadFs.ordinal(),
        },
        LifecycleEvent::PostTool {
            tool: "Read".into(),
            phase: ToolPhase::Ok,
            sandbox_ordinal: SandboxProfile::ReadFs.ordinal(),
        },
        LifecycleEvent::PreCommit {
            branch: "phase-10".into(),
        },
        LifecycleEvent::PostCommit {
            sha: "abc1234".into(),
        },
        LifecycleEvent::SessionStart,
        LifecycleEvent::SessionEnd,
    ];
    for ev in evs {
        let json = serde_json::to_string(&ev).expect("ser");
        let _back: LifecycleEvent = serde_json::from_str(&json).expect("de");
    }
}

#[test]
fn parses_allow_override() {
    let stdout = br#"{"override":{"action":"allow","reason":"trusted"}}"#;
    let parsed = parse_hook_stdout(stdout).expect("parse");
    match parsed {
        HookOverride::Allow { reason } => assert_eq!(reason, "trusted"),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn parses_deny_override() {
    let stdout = br#"{"override":{"action":"deny","reason":"blacklist"}}"#;
    let parsed = parse_hook_stdout(stdout).expect("parse");
    assert!(matches!(parsed, HookOverride::Deny { .. }));
}

#[test]
fn empty_stdout_means_passthrough() {
    let parsed = parse_hook_stdout(b"").expect("parse");
    assert!(matches!(parsed, HookOverride::Passthrough));
}

#[test]
fn rejects_malformed_json() {
    assert!(parse_hook_stdout(b"{not json}").is_err());
}
