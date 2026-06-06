// SPDX-License-Identifier: Apache-2.0
use origin_store::Store;

#[test]
fn migrate_creates_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("origin.db");
    let s = Store::open(&path).expect("open store");
    s.with_conn(|c| {
        let n: u32 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name IN ('sessions','messages')",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(n, 2);
        Ok(())
    })
    .expect("with_conn");
}

#[test]
fn migrate_creates_message_snapshots_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("origin.db");
    let s = Store::open(&path).expect("open store");
    s.with_conn(|c| {
        let n: u32 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name = 'message_snapshots'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(n, 1, "message_snapshots table should exist after migrations");
        Ok(())
    })
    .expect("with_conn");
}

#[test]
fn migrations_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("origin.db");
    let first = Store::open(&path).expect("first open");
    drop(first);
    let _second = Store::open(&path).expect("second open should be idempotent");
}
