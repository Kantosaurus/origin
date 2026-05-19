use origin_cas::{Hash, RefTable};
use rusqlite::Connection;

fn make_table() -> (Connection, RefTable) {
    let conn = Connection::open_in_memory().expect("memdb");
    conn.execute_batch(
        "CREATE TABLE cas_refs (hash BLOB PRIMARY KEY, refcount INTEGER NOT NULL DEFAULT 0, tier INTEGER NOT NULL DEFAULT 0, last_access INTEGER NOT NULL);",
    )
    .expect("schema");
    let table = RefTable::new();
    (conn, table)
}

#[test]
fn incr_then_decr_reaches_zero() {
    let (conn, table) = make_table();
    let h = Hash::of(b"x");
    table.incr(&conn, h).expect("incr");
    assert_eq!(table.get(&conn, h).expect("get"), Some(1));
    table.decr(&conn, h).expect("decr");
    assert_eq!(table.get(&conn, h).expect("get"), Some(0));
}

#[test]
fn dead_hashes_lists_only_zero_count() {
    let (conn, table) = make_table();
    let a = Hash::of(b"a");
    let b = Hash::of(b"b");
    let c = Hash::of(b"c");
    table.incr(&conn, a).expect("incr a");
    table.incr(&conn, b).expect("incr b1");
    table.incr(&conn, b).expect("incr b2");
    table.incr(&conn, c).expect("incr c");
    table.decr(&conn, c).expect("decr c");
    let dead: Vec<Hash> = table.dead_hashes(&conn).expect("dead").collect();
    assert_eq!(dead, vec![c]);
}

#[test]
fn decr_below_zero_is_clamped_and_errors() {
    let (conn, table) = make_table();
    let h = Hash::of(b"never-incremented");
    let err = table.decr(&conn, h);
    assert!(err.is_err(), "decr below zero must error");
}
