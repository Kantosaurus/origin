//! Routing test — every `ArenaId` resolves to a distinct backend arena handle,
//! and `with_arena(id, |scope| …)` returns the same `id` back via `scope.id()`.

#![allow(clippy::redundant_closure_for_method_calls)]

use origin_alloc::{with_arena, ArenaId};

#[test]
fn every_arena_id_is_distinct() {
    let ids = [
        ArenaId::Agent,
        ArenaId::Cas,
        ArenaId::Sidecar,
        ArenaId::SwarmCoord,
        ArenaId::SwarmWorker,
        ArenaId::Ipc,
        ArenaId::MetricsHttp,
        ArenaId::CodeGraph,
        ArenaId::Mem,
        ArenaId::Other,
    ];
    let mut indices = ids.iter().map(|id| id.backend_index()).collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        ids.len(),
        "every ArenaId must map to a distinct backend index"
    );
}

#[test]
fn with_arena_returns_scope_with_same_id() {
    let observed = with_arena(ArenaId::Cas, |scope| scope.id()).expect("scope should bind");
    assert_eq!(observed, ArenaId::Cas);
}
