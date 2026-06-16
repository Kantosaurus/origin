// SPDX-License-Identifier: Apache-2.0
//! Durability of offloaded content across a daemon restart.
//!
//! Root cause of the "cas miss for tool result handle" loop error: a tool result
//! offloaded to CAS lives only in the in-memory Hot LRU until it is LRU-evicted.
//! Neither the old shutdown flush (`flush_warm_pending`, which seals only the
//! already-evicted warm batch) nor a SIGKILL restart persisted the Hot tier, so a
//! handle persisted in the transcript dangled after the next daemon restart.
//! `flush_all` fixes this by persisting Hot too.

use origin_cas::{Store, StoreConfig};

fn cfg(root: std::path::PathBuf) -> StoreConfig {
    StoreConfig {
        root,
        hot_capacity: 128, // big enough that a single put() stays resident in Hot
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    }
}

#[test]
fn flush_all_persists_hot_tier_across_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = cfg(dir.path().to_path_buf());

    let store = Store::open(c.clone()).expect("open");
    let payload = b"a large offloaded tool result payload";
    let h = store.put(payload).expect("put");
    assert_eq!(
        store.get(h).expect("get").as_deref(),
        Some(&payload[..]),
        "payload should be readable from Hot before any flush"
    );

    // Durable checkpoint, then simulate a daemon restart: drop + reopen the
    // same root (the new daemon rebuilds its indices from the on-disk packs).
    store.flush_all().expect("flush_all");
    drop(store);
    let store2 = Store::open(c).expect("reopen");

    assert_eq!(
        store2.get(h).expect("get after restart").as_deref(),
        Some(&payload[..]),
        "Hot-tier payload MUST survive flush_all + restart (no cas miss)"
    );
}

#[test]
fn flush_warm_pending_alone_leaves_hot_unpersisted() {
    // Documents the bug + guards the call sites: sealing only warm_pending does
    // NOT persist a Hot-resident entry, so it is lost across a restart. The
    // shutdown/checkpoint paths must call flush_all, not flush_warm_pending.
    let dir = tempfile::tempdir().expect("tempdir");
    let c = cfg(dir.path().to_path_buf());

    let store = Store::open(c.clone()).expect("open");
    let h = store.put(b"hot only").expect("put");
    store.flush_warm_pending().expect("flush_warm_pending");
    drop(store);
    let store2 = Store::open(c).expect("reopen");

    assert_eq!(
        store2.get(h).expect("get").as_deref(),
        None,
        "warm-pending flush must NOT have persisted the Hot entry (the bug)"
    );
}

#[test]
fn flush_all_is_idempotent_and_keeps_hot_readable() {
    // Repeated checkpoints must not corrupt or duplicate-fault, and the entry
    // stays readable in-process (Hot is not evicted by the flush).
    let dir = tempfile::tempdir().expect("tempdir");
    let c = cfg(dir.path().to_path_buf());

    let store = Store::open(c).expect("open");
    let h = store.put(b"checkpoint me").expect("put");
    store.flush_all().expect("flush_all 1");
    store.flush_all().expect("flush_all 2 (idempotent)");
    assert_eq!(
        store.get(h).expect("get").as_deref(),
        Some(&b"checkpoint me"[..]),
        "entry stays readable from Hot after checkpoints"
    );
}
