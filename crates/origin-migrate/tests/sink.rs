// SPDX-License-Identifier: Apache-2.0
use origin_migrate::sink::{apply_with_store, summarize};
use origin_migrate::source::{
    ImportedMemory, ImportedMessage, ImportedSession, ImportedSkill, MigrateBundle,
};
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

#[test]
fn apply_persists_memories_and_count_is_not_phantom() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("mem.db")).expect("open");

    let bundle = MigrateBundle {
        sessions: vec![],
        skills: vec![],
        memories: vec![
            ImportedMemory {
                kind: "preference".into(),
                body: "user prefers dark mode".into(),
                tags: vec!["ui".into()],
            },
            ImportedMemory {
                kind: "fact".into(),
                body: "the repo uses Rust 2021".into(),
                tags: vec!["lang".into(), "rust".into()],
            },
        ],
    };

    // The dry-run summary claims it will insert two memories.
    let dry = summarize(&bundle);
    assert_eq!(dry.memories_inserted, 2);

    // Apply must persist exactly that many — the reported count must match what
    // is actually queryable, not a phantom count over a dropped field.
    let r1 = apply_with_store(&store, &bundle).expect("apply 1");
    assert_eq!(r1.memories_inserted, 2);
    assert_eq!(r1.memories_skipped_duplicate, 0);
    assert_eq!(
        store.count_migrated_memories().expect("count"),
        2,
        "reported memories_inserted must equal persisted rows"
    );

    // Re-applying the same bundle is idempotent: nothing new, both deduped.
    let r2 = apply_with_store(&store, &bundle).expect("apply 2");
    assert_eq!(r2.memories_inserted, 0);
    assert_eq!(r2.memories_skipped_duplicate, 2);
    assert_eq!(
        store.count_migrated_memories().expect("count after re-apply"),
        2,
        "re-apply must not duplicate memories"
    );
}

#[test]
fn distinct_memories_with_same_body_are_not_deduped() {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("mem2.db")).expect("open");

    // Same body, different kind/tags — these are distinct and both must persist.
    let bundle = MigrateBundle {
        sessions: vec![],
        skills: vec![],
        memories: vec![
            ImportedMemory {
                kind: "a".into(),
                body: "same".into(),
                tags: vec!["x".into()],
            },
            ImportedMemory {
                kind: "b".into(),
                body: "same".into(),
                tags: vec!["x".into()],
            },
            ImportedMemory {
                kind: "a".into(),
                body: "same".into(),
                tags: vec!["y".into()],
            },
        ],
    };

    let r = apply_with_store(&store, &bundle).expect("apply");
    assert_eq!(r.memories_inserted, 3, "all three distinct memories persist");
    assert_eq!(store.count_migrated_memories().expect("count"), 3);
}
