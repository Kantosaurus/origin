use origin_migrate::sink::apply_with_store;
use origin_migrate::source::{ImportedMessage, ImportedSession, ImportedSkill, MigrateBundle};
use origin_store::Store;
use tempfile::tempdir;

#[test]
fn apply_with_store_inserts_sessions_idempotently() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("sessions.db")).expect("open");

    let bundle = MigrateBundle {
        sessions: vec![ImportedSession {
            source_id: "s1".into(),
            title: Some("hello".into()),
            created_at_unix_ms: 1,
            messages: vec![ImportedMessage {
                role: "user".into(),
                body: "hi".into(),
            }],
        }],
        skills: vec![ImportedSkill {
            name: "refactor".into(),
            body: "body".into(),
        }],
        memories: vec![],
    };

    let r1 = apply_with_store(&store, &bundle).expect("apply 1");
    assert_eq!(r1.sessions_inserted, 1);
    assert_eq!(r1.skills_inserted, 1);
    assert_eq!(r1.sessions_skipped_duplicate, 0);
    assert_eq!(r1.skills_skipped_duplicate, 0);

    let r2 = apply_with_store(&store, &bundle).expect("apply 2");
    assert_eq!(r2.sessions_inserted, 0);
    assert_eq!(r2.skills_inserted, 0);
    assert_eq!(r2.sessions_skipped_duplicate, 1);
    assert_eq!(r2.skills_skipped_duplicate, 1);
}
