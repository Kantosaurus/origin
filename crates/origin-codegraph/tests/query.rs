// SPDX-License-Identifier: Apache-2.0
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

/// Seed two disjoint call clusters (`a→b→c` and `d→e→f`) and assert
/// `Communities` returns exactly two partitions.
#[allow(clippy::many_single_char_names)] // canonical 6-node fixture
fn make_two_cluster_idx() -> (tempfile::TempDir, CodeGraphIndex) {
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

    let mut insert = |name: &str, file: &str| {
        idx.insert_node(&CodeNodeRecord {
            kind: NodeKind::Function,
            name: name.into(),
            language: Language::Rust,
            file_path: file.into(),
            range: Range { start: 0, end: 1 },
            signature: format!("fn {name}()").into_bytes(),
            body: b"".to_vec(),
        })
        .expect("insert")
    };
    let a = insert("a", "a.rs");
    let b = insert("b", "b.rs");
    let c = insert("c", "c.rs");
    let d = insert("d", "d.rs");
    let e = insert("e", "e.rs");
    let f = insert("f", "f.rs");

    // Cluster 1: a → b, b → c, a → c (triangle).
    idx.insert_edge(a, b, "calls", Confidence::Extracted, b"ab")
        .expect("ab");
    idx.insert_edge(b, c, "calls", Confidence::Extracted, b"bc")
        .expect("bc");
    idx.insert_edge(a, c, "calls", Confidence::Extracted, b"ac")
        .expect("ac");
    // Cluster 2: d → e, e → f, d → f (triangle).
    idx.insert_edge(d, e, "calls", Confidence::Extracted, b"de")
        .expect("de");
    idx.insert_edge(e, f, "calls", Confidence::Extracted, b"ef")
        .expect("ef");
    idx.insert_edge(d, f, "calls", Confidence::Extracted, b"df")
        .expect("df");
    (dir, idx)
}

#[test]
fn query_communities_partitions_disjoint_clusters() {
    let (_dir, idx) = make_two_cluster_idx();
    let r = dispatch(&idx, Query::Communities).expect("q");
    match r {
        QueryResult::Partitions(parts) => {
            assert_eq!(parts.len(), 2, "expected two clusters, got {}", parts.len());
            let total: usize = parts.iter().map(Vec::len).sum();
            assert_eq!(total, 6, "all 6 nodes should land in some partition");
            for p in &parts {
                assert_eq!(p.len(), 3, "each triangle has 3 nodes");
            }
        }
        other => panic!("expected Partitions, got {other:?}"),
    }
}

#[test]
fn query_communities_empty_graph_returns_no_partitions() {
    let dir = tempdir().expect("tempdir");
    let cas = Cas::open(StoreConfig {
        root: dir.path().join("cas"),
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 16,
        cold_zstd_level: 3,
    })
    .expect("cas");
    let store = origin_store::Store::open(dir.path().join("s.db")).expect("store");
    let idx = CodeGraphIndex::new(cas, store);
    let r = dispatch(&idx, Query::Communities).expect("q");
    match r {
        QueryResult::Partitions(parts) => assert!(parts.is_empty()),
        other => panic!("expected empty Partitions, got {other:?}"),
    }
}

#[test]
fn query_god_nodes_caps_per_partition_by_indegree() {
    let (_dir, idx) = make_two_cluster_idx();
    // In each triangle, the "sink" (c, f) has in-degree 2; the others have 1.
    let r = dispatch(&idx, Query::GodNodes { top_per_partition: 1 }).expect("q");
    match r {
        QueryResult::Partitions(parts) => {
            assert_eq!(parts.len(), 2);
            for p in &parts {
                assert_eq!(p.len(), 1, "top_per_partition=1 must cap each cluster");
            }
            let names: Vec<&str> = parts
                .iter()
                .flat_map(|p| p.iter().map(|n| n.name.as_str()))
                .collect();
            assert!(names.contains(&"c"), "highest in-degree of cluster 1 is c");
            assert!(names.contains(&"f"), "highest in-degree of cluster 2 is f");
        }
        other => panic!("expected Partitions, got {other:?}"),
    }
}
