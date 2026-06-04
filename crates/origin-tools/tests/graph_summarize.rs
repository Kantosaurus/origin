// SPDX-License-Identifier: Apache-2.0
//! Integration test for the real `graph_summarize` neighborhood summary.

#![allow(clippy::panic)] // test-only: an unexpected variant signals a bug
#![allow(clippy::unwrap_used)] // test-only

use origin_cas::{Store as Cas, StoreConfig};
use origin_codegraph::extract::{NodeKind, Range};
use origin_codegraph::index::{CodeGraphIndex, EntityId};
use origin_codegraph::query::QueryResult;
use origin_codegraph::record::{CodeNodeRecord, Confidence};
use origin_codegraph::Language;
use origin_tools::builtins::graph_summarize::graph_summarize_tool;
use tempfile::tempdir;

fn insert(idx: &mut CodeGraphIndex, name: &str, file: &str) -> EntityId {
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
}

/// Build `target → n1`, `target → n2` and assert the summary names both
/// neighbours and includes the target itself.
#[test]
fn summarizes_target_neighborhood() {
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

    let target = insert(&mut idx, "target", "target.rs");
    let n1 = insert(&mut idx, "neighbor_one", "n1.rs");
    let n2 = insert(&mut idx, "neighbor_two", "n2.rs");
    idx.insert_edge(target, n1, "calls", Confidence::Extracted, b"t->n1")
        .expect("edge1");
    idx.insert_edge(target, n2, "calls", Confidence::Extracted, b"t->n2")
        .expect("edge2");

    let hex = hex::encode(target.as_bytes());
    let result = graph_summarize_tool(&idx, &hex).expect("summary");

    assert!(!result.is_empty(), "summary must be a populated result, not Empty");
    let QueryResult::Nodes(nodes) = result else {
        panic!("expected Nodes summary, got {result:?}");
    };
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"target"), "summary must include the target node");
    assert!(names.contains(&"neighbor_one"), "summary must name neighbor_one");
    assert!(names.contains(&"neighbor_two"), "summary must name neighbor_two");
}

/// A node id with no edges still yields a non-empty result naming just itself.
#[test]
fn summarizes_isolated_node() {
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
    let lone = insert(&mut idx, "lone", "lone.rs");

    let result = graph_summarize_tool(&idx, &hex::encode(lone.as_bytes())).expect("summary");
    let QueryResult::Nodes(nodes) = result else {
        panic!("expected Nodes summary, got {result:?}");
    };
    assert_eq!(nodes.len(), 1, "isolated node summary is just the node itself");
    assert_eq!(nodes[0].name, "lone");
}

/// An unknown / unresolvable target hex yields `Empty` (nothing to summarize).
#[test]
fn unknown_target_is_empty() {
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

    let missing = hex::encode([0xab_u8; 32]);
    let result = graph_summarize_tool(&idx, &missing).expect("summary");
    assert!(result.is_empty(), "unknown target must summarize as Empty");
}
