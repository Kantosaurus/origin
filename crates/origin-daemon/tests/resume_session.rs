// SPDX-License-Identifier: Apache-2.0
//! `ClientMessage::ResumeSession` no longer returns a stub `AdminOk`. It
//! reads the persisted message log + any supervisor-checkpointed
//! [`ResumeToken`] and replies with [`StreamEvent::SessionResumed`].

use origin_core::types::{Block, Message, Role};
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_resume_token::ResumeToken;
use tempfile::TempDir;

#[test]
fn resume_session_message_round_trips() {
    let m = ClientMessage::ResumeSession {
        session_id: "sess-x".into(),
    };
    let s = serde_json::to_string(&m).expect("ser");
    assert!(s.contains("\"kind\":\"resume_session\""), "json was: {s}");
    let back: ClientMessage = serde_json::from_str(&s).expect("de");
    assert!(matches!(back, ClientMessage::ResumeSession { .. }));
}

#[test]
fn session_resumed_event_serializes() {
    let ev = StreamEvent::SessionResumed {
        session_id: "sess-x".into(),
        messages_loaded: 4,
        restored_to_turn: 3,
        had_resume_token: true,
    };
    let s = serde_json::to_string(&ev).expect("ser");
    assert!(s.contains("\"kind\":\"session_resumed\""), "json was: {s}");
    assert!(s.contains("\"messages_loaded\":4"), "json was: {s}");
    assert!(s.contains("\"had_resume_token\":true"), "json was: {s}");
}

#[test]
fn session_store_round_trips_messages_and_resume_token() {
    let dir = TempDir::new().expect("tempdir");
    let store = SessionStore::open(dir.path().join("origin.db")).expect("open");

    let session = Session::new_with_id("sess-y".into(), "claude-opus-4-7".into());
    store.persist_session(&session).expect("persist session");
    for (i, body) in ["hello", "hi", "follow-up", "ok"].iter().enumerate() {
        let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
        let m = Message::new(role).with_block(Block::text((*body).to_string()));
        store
            .persist_message("sess-y", u32::try_from(i).expect("turn fits u32"), &m)
            .expect("persist message");
    }

    // The supervisor checkpointed turn 2 (third user/assistant exchange).
    let token = ResumeToken {
        session_id: "sess-y".into(),
        last_turn: 2,
        cas_handle_root: [0u8; 32],
        pending_tool_calls: Vec::new(),
        plan_seq: 0,
        goal: None,
    };
    store.save_resume_token(&token).expect("save token");

    let loaded = store
        .load_resume_token("sess-y")
        .expect("load")
        .expect("token present");
    assert_eq!(loaded.last_turn, 2);

    let messages = store.load_messages("sess-y").expect("load messages");
    assert_eq!(messages.len(), 4);

    // Unknown session: no rows on disk.
    let empty = store.load_messages("does-not-exist").expect("ok empty");
    assert!(empty.is_empty());
}
