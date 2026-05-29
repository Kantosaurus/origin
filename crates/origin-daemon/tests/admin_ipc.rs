// SPDX-License-Identifier: Apache-2.0
//! P13.4.2 — IPC envelope additions for the admin surface:
//! `ListSessions`, `RemoveSession`, `ResumeSession`, `GetUsage`,
//! `KeyringAdd` / `KeyringList` / `KeyringRemove`, and the matching
//! `SessionsListed`, `UsageReport`, `KeyringAccounts`, `AdminOk`,
//! `AdminError` `StreamEvent`s.

use origin_daemon::protocol::{ClientMessage, SessionSummaryWire, StreamEvent};

#[test]
fn list_sessions_message_round_trips() {
    let m = ClientMessage::ListSessions;
    let json = serde_json::to_vec(&m).expect("ser");
    let back: ClientMessage = serde_json::from_slice(&json).expect("de");
    assert!(matches!(back, ClientMessage::ListSessions));
}

#[test]
fn sessions_listed_event_carries_summaries() {
    let ev = StreamEvent::SessionsListed {
        summaries: vec![SessionSummaryWire {
            id: "s1".into(),
            created_at: 1,
            title: None,
            model: "m".into(),
            message_count: 0,
        }],
    };
    let s = serde_json::to_string(&ev).expect("ser");
    assert!(s.contains("\"kind\":\"sessions_listed\""), "json was: {s}");
}

#[test]
fn keyring_add_serializes() {
    let m = ClientMessage::KeyringAdd {
        provider: "anthropic".into(),
        account: "default".into(),
        secret: "sk-...".into(),
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"kind\":\"keyring_add\""), "json was: {s}");
}

#[test]
fn remove_session_round_trips() {
    let m = ClientMessage::RemoveSession {
        session_id: "abc".into(),
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"kind\":\"remove_session\""), "json was: {s}");
    let back: ClientMessage = serde_json::from_str(&s).expect("de");
    assert!(matches!(back, ClientMessage::RemoveSession { .. }));
}

#[test]
fn get_usage_round_trips() {
    let m = ClientMessage::GetUsage;
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"kind\":\"get_usage\""), "json was: {s}");
    let back: ClientMessage = serde_json::from_str(&s).expect("de");
    assert!(matches!(back, ClientMessage::GetUsage));
}

#[test]
fn admin_ok_and_error_events_serialize() {
    let ok = StreamEvent::AdminOk;
    let s = serde_json::to_string(&ok).expect("ser");
    assert!(s.contains("\"kind\":\"admin_ok\""), "json was: {s}");

    let err = StreamEvent::AdminError {
        message: "boom".into(),
    };
    let s = serde_json::to_string(&err).expect("ser");
    assert!(s.contains("\"kind\":\"admin_error\""), "json was: {s}");
}

#[test]
fn keyring_accounts_event_carries_provider_list() {
    let ev = StreamEvent::KeyringAccounts {
        provider: "anthropic".into(),
        accounts: vec!["default".into(), "work".into()],
    };
    let s = serde_json::to_string(&ev).expect("ser");
    assert!(s.contains("\"kind\":\"keyring_accounts\""), "json was: {s}");
    assert!(s.contains("\"provider\":\"anthropic\""), "json was: {s}");
}

#[test]
fn usage_report_event_carries_rows() {
    let ev = StreamEvent::UsageReport {
        rows: vec![origin_daemon::protocol::UsageRow {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            tokens_in: 100,
            tokens_out: 50,
        }],
    };
    let s = serde_json::to_string(&ev).expect("ser");
    assert!(s.contains("\"kind\":\"usage_report\""), "json was: {s}");
    assert!(s.contains("\"tokens_in\":100"), "json was: {s}");
}
