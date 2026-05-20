use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use tempfile::TempDir;

#[test]
fn list_summaries_returns_persisted_sessions() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("origin.db");
    let store = SessionStore::open(&path).unwrap();

    let s1 = Session::new_with_id("sess-a".into(), "claude-opus-4-7".into());
    store.persist_session(&s1).unwrap();
    let s2 = Session::new_with_id("sess-b".into(), "claude-haiku".into());
    store.persist_session(&s2).unwrap();

    let mut summaries = store.list_summaries().unwrap();
    summaries.sort_by_key(|s| s.id.clone());
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id, "sess-a");
    assert_eq!(summaries[1].id, "sess-b");
    assert_eq!(summaries[0].model, "claude-opus-4-7");
}

#[test]
fn delete_removes_session_and_messages() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("origin.db");
    let store = SessionStore::open(&path).unwrap();
    let s = Session::new_with_id("sess-x".into(), "m".into());
    store.persist_session(&s).unwrap();

    store.delete("sess-x").unwrap();
    let summaries = store.list_summaries().unwrap();
    assert!(summaries.iter().all(|s| s.id != "sess-x"));
}
