// SPDX-License-Identifier: Apache-2.0
//! Integration tests for the CAS-backed code-graph index (P7.3).

use origin_cas::{Store as CasStore, StoreConfig};
use origin_codegraph::extract::{NodeKind, Range};
use origin_codegraph::index::CodeGraphIndex;
use origin_codegraph::lang::Language;
use origin_codegraph::record::{CodeNodeRecord, Confidence};
use origin_store::Store as SqlStore;
use tempfile::tempdir;

fn open_index() -> (CodeGraphIndex, tempfile::TempDir) {
    let tmp = tempdir().expect("tempdir");
    let cas_root = tmp.path().join("cas");
    let db_path = tmp.path().join("origin.db");

    let cas = CasStore::open(StoreConfig {
        root: cas_root,
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    })
    .expect("open cas store");
    let sql = SqlStore::open(&db_path).expect("open sqlite store");
    (CodeGraphIndex::new(cas, sql), tmp)
}

#[test]
fn insert_two_files_with_identical_signature_dedup() {
    let (mut idx, _tmp) = open_index();
    let signature = b"fn handle() -> Result<()>";
    let body = b"fn handle() -> Result<()> { Ok(()) }";

    let rec_a = CodeNodeRecord {
        kind: NodeKind::Function,
        name: "handle".to_owned(),
        language: Language::Rust,
        file_path: "a.rs".to_owned(),
        range: Range { start: 0, end: 36 },
        signature: signature.to_vec(),
        body: body.to_vec(),
    };
    let rec_b = CodeNodeRecord {
        kind: NodeKind::Function,
        name: "handle".to_owned(),
        language: Language::Rust,
        file_path: "b.rs".to_owned(),
        range: Range { start: 0, end: 36 },
        signature: signature.to_vec(),
        body: body.to_vec(),
    };

    let id_a = idx.insert_node(&rec_a).expect("insert a");
    let id_b = idx.insert_node(&rec_b).expect("insert b");
    assert_ne!(
        id_a, id_b,
        "different file paths must produce different entity ids"
    );

    let rows = idx.nodes_by_signature(signature).expect("query by signature");
    assert_eq!(rows.len(), 2, "both nodes should share the signature handle");
    assert_eq!(
        rows[0].signature_handle, rows[1].signature_handle,
        "signature handle must be identical across files (CAS dedup)"
    );
}

#[test]
fn insert_edge_round_trip() {
    let (mut idx, _tmp) = open_index();
    let rec_a = CodeNodeRecord {
        kind: NodeKind::Function,
        name: "caller".to_owned(),
        language: Language::Rust,
        file_path: "lib.rs".to_owned(),
        range: Range { start: 0, end: 20 },
        signature: b"fn caller()".to_vec(),
        body: b"fn caller() { callee(); }".to_vec(),
    };
    let rec_b = CodeNodeRecord {
        kind: NodeKind::Function,
        name: "callee".to_owned(),
        language: Language::Rust,
        file_path: "lib.rs".to_owned(),
        range: Range { start: 21, end: 40 },
        signature: b"fn callee()".to_vec(),
        body: b"fn callee() {}".to_vec(),
    };
    let id_a = idx.insert_node(&rec_a).expect("insert caller");
    let id_b = idx.insert_node(&rec_b).expect("insert callee");

    idx.insert_edge(
        id_a,
        id_b,
        "calls",
        Confidence::Extracted,
        b"call site at lib.rs:1",
    )
    .expect("insert edge");

    let edges = idx.edges_from(id_a).expect("query edges_from");
    assert_eq!(edges.len(), 1, "should observe exactly one outgoing edge");
    assert_eq!(edges[0].from, id_a);
    assert_eq!(edges[0].to, id_b);
    assert_eq!(edges[0].kind, "calls");
    assert_eq!(edges[0].confidence, Confidence::Extracted);
}
