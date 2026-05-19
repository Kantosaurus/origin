//! Integration tests for the typed query DSL (P7.6 N6.10).

#![allow(clippy::panic)] // test-only: unexpected variant signals a bug

use origin_cas::{Store as Cas, StoreConfig};
use origin_codegraph::extract::{NodeKind, Range};
use origin_codegraph::index::{CodeGraphIndex, EntityId};
use origin_codegraph::query::{dispatch, Query, QueryResult};
use origin_codegraph::record::{CodeNodeRecord, Confidence};
use origin_codegraph::Language;
use tempfile::tempdir;

fn make_idx() -> (tempfile::TempDir, CodeGraphIndex, EntityId, EntityId, EntityId) {
    let dir = tempdir().expect("tempdir");
    let cas = Cas::open(StoreConfig {
        root: dir.path().join("cas"),
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 16,
        cold_zstd_level: 3,
    })
    .expect("cas");
    let store = origin_store::Store::open(dir.path().join("s.db")).expect("store");
    let mut idx = CodeGraphIndex::new(cas, store);
    let a = idx
        .insert_node(&CodeNodeRecord {
            kind: NodeKind::Function,
            name: "alpha".into(),
            language: Language::Rust,
            file_path: "a.rs".into(),
            range: Range { start: 0, end: 1 },
            signature: b"fn alpha()".to_vec(),
            body: b"".to_vec(),
        })
        .expect("a");
    let b = idx
        .insert_node(&CodeNodeRecord {
            kind: NodeKind::Function,
            name: "beta".into(),
            language: Language::Rust,
            file_path: "b.rs".into(),
            range: Range { start: 0, end: 1 },
            signature: b"fn beta()".to_vec(),
            body: b"".to_vec(),
        })
        .expect("b");
    let c = idx
        .insert_node(&CodeNodeRecord {
            kind: NodeKind::Function,
            name: "gamma".into(),
            language: Language::Rust,
            file_path: "c.rs".into(),
            range: Range { start: 0, end: 1 },
            signature: b"fn gamma()".to_vec(),
            body: b"".to_vec(),
        })
        .expect("c");
    idx.insert_edge(a, b, "calls", Confidence::Extracted, b"a->b")
        .expect("ab");
    idx.insert_edge(b, c, "calls", Confidence::Extracted, b"b->c")
        .expect("bc");
    (dir, idx, a, b, c)
}

#[test]
fn query_neighbors() {
    let (_dir, idx, a, b, _c) = make_idx();
    let r = dispatch(&idx, Query::Neighbors { node: a, depth: 1 }).expect("q");
    match r {
        QueryResult::Nodes(ns) => assert!(ns.iter().any(|n| n.entity_id == b)),
        other => panic!("expected Nodes, got {other:?}"),
    }
}

#[test]
fn query_path() {
    let (_dir, idx, a, _b, c) = make_idx();
    let r = dispatch(
        &idx,
        Query::Path {
            from: a,
            to: c,
            max_hops: 3,
        },
    )
    .expect("q");
    match r {
        QueryResult::Path(hops) => assert_eq!(hops.len(), 3, "a->b->c is 3 entities"),
        other => panic!("expected Path, got {other:?}"),
    }
}

#[test]
fn query_recent_changes() {
    let (_dir, idx, _a, _b, _c) = make_idx();
    let r = dispatch(&idx, Query::RecentChanges { since_ms: 0 }).expect("q");
    match r {
        QueryResult::Nodes(ns) => assert_eq!(ns.len(), 3),
        other => panic!("expected Nodes, got {other:?}"),
    }
}
