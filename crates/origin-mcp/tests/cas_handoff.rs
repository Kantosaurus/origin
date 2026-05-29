// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::panic)]
use origin_cas::{Store, StoreConfig};
use origin_mcp::cas_handoff::{cas_handoff_if_large, HandoffOutcome};
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;

fn make_store() -> Arc<Store> {
    let tmp = tempdir().expect("tmp");
    Arc::new(
        Store::open(StoreConfig {
            root: tmp.path().to_path_buf(),
            hot_capacity: 16,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

#[test]
fn small_body_passes_through() {
    let store = make_store();
    let value = json!({"hello":"world"});
    let out = cas_handoff_if_large(&store, value.clone(), 1024).expect("handoff");
    match out {
        HandoffOutcome::Inline(v) => assert_eq!(v, value),
        HandoffOutcome::Cas { .. } => panic!("small body should stay inline"),
    }
}

#[test]
fn large_body_lands_in_cas() {
    let store = make_store();
    let big_string: String = "x".repeat(32 * 1024);
    let value = json!({"content": big_string});
    let out = cas_handoff_if_large(&store, value.clone(), 16 * 1024).expect("handoff");
    match out {
        HandoffOutcome::Cas { handle, byte_len } => {
            assert_eq!(byte_len, serde_json::to_vec(&value).expect("ser").len());
            // The handle should be retrievable.
            let bytes = store.get(handle).expect("get").expect("found");
            let round_trip: serde_json::Value = serde_json::from_slice(&bytes).expect("de");
            assert_eq!(round_trip, value);
        }
        HandoffOutcome::Inline(_) => panic!("large body should land in CAS"),
    }
}
