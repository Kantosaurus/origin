// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use tempfile::tempdir;

#[test]
fn update_summary_writes_column() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("origin.db");
    let store = SessionStore::open(&db).expect("open");
    let s = Session::new("anthropic".to_string(), "claude-opus-4-7".to_string());
    store.persist_session(&s).expect("persist session");
    let m = Message {
        role: Role::Assistant,
        blocks: vec![Block::Text {
            text: "hi".into(),
            cache_marker: None,
        }],
    };
    store.persist_message(&s.id, 0, &m).expect("persist message");
    store.update_summary(&s.id, 0, "first-summary").expect("update");
    drop(store);
    let conn = rusqlite::Connection::open(&db).expect("re-open");
    let got: String = conn
        .query_row(
            "SELECT summary FROM messages WHERE session_id = ?1 AND turn_index = ?2",
            rusqlite::params![&s.id, 0],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(got, "first-summary");
}
