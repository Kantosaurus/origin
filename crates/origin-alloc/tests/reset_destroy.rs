//! jemalloc-only — reset releases resident bytes; destroy invalidates the arena.
//!
//! These tests touch global jemalloc state, so they run on a single thread.

#![cfg(feature = "jemalloc")]

use origin_alloc::{destroy, reset, stats_snapshot, with_arena, ArenaId};
use serial_test::serial;

#[test]
#[serial]
fn reset_releases_resident_bytes() {
    // Allocate something into Cas.
    let buf: Vec<u8> = with_arena(ArenaId::Cas, |_| vec![7u8; 16 * 1024 * 1024]).expect("scope should bind");
    let before = stats_snapshot().expect("snapshot")[ArenaId::Cas.backend_index()];
    drop(buf);
    reset(ArenaId::Cas).expect("reset should succeed");
    let after = stats_snapshot().expect("snapshot")[ArenaId::Cas.backend_index()];
    assert!(
        after.resident_bytes <= before.resident_bytes,
        "reset must not grow resident: before={} after={}",
        before.resident_bytes,
        after.resident_bytes
    );
}

#[test]
#[serial]
fn destroy_invalidates_arena() {
    // Allocate then destroy. Subsequent rebind must hand back a fresh arena.
    let _drop_me: Vec<u8> =
        with_arena(ArenaId::SwarmWorker, |_| vec![0u8; 4 * 1024 * 1024]).expect("scope should bind");
    let idx_before = stats_snapshot().expect("snapshot")[ArenaId::SwarmWorker.backend_index()].jemalloc_index;
    destroy(ArenaId::SwarmWorker).expect("destroy should succeed");
    // Rebind triggers re-creation.
    let _v: Vec<u8> = with_arena(ArenaId::SwarmWorker, |_| vec![0u8; 1024]).expect("scope should bind");
    let idx_after = stats_snapshot().expect("snapshot")[ArenaId::SwarmWorker.backend_index()].jemalloc_index;
    assert_ne!(
        idx_before, idx_after,
        "destroy + rebind must allocate a new jemalloc arena index"
    );
}
