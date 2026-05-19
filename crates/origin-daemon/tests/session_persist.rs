#![allow(clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;

#[test]
fn round_trip_persists_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("origin.db");
    let store = SessionStore::open(&db).expect("open store");

    let mut s = Session::new("anthropic", "claude-opus-4-7");
    let sid = s.id.to_string();
    s.push(Message::new(Role::User).with_block(Block::text("hello")));
    s.push(Message::new(Role::Assistant).with_block(Block::text("hi")));

    store.persist_session(&s).expect("persist meta");
    for (i, m) in s.messages.iter().enumerate() {
        store
            .persist_message(&sid, u32::try_from(i).expect("u32 fits"), m)
            .expect("persist message");
    }

    let loaded = store.load_messages(&sid).expect("load");
    assert_eq!(loaded.len(), 2);
    let first_text = match &loaded[0].blocks[0] {
        Block::Text { text, .. } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert_eq!(first_text, "hello");
    assert_eq!(loaded[1].role, Role::Assistant);
}
