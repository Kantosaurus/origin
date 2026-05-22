//! Phase 11 N4.3 sharing contract: a `Plan` clone shares its handle→band
//! map with the original. This is the foundation that lets the daemon's
//! tool-result dispatch path register handles that the provider's wire
//! encoder will then see on subsequent turns — without an explicit
//! channel between them.

// `redundant_clone` flags the writer.clone() pattern in the share tests —
// but the entire point of those tests is to verify that a Plan clone shares
// state with the original. Hold the allow narrowly here.
#![allow(clippy::unwrap_used, clippy::redundant_clone)]

use origin_planner::{Band, Plan};

#[test]
fn handle_registered_on_clone_is_visible_on_original() {
    let writer = Plan::default();
    let reader = writer.clone();

    assert_eq!(writer.handle_count(), 0);
    assert_eq!(reader.handle_count(), 0);

    let h = [0xAB_u8; 32];
    writer.register_handle(h, Band::Sticky);

    // Both `writer` and `reader` should now see the registration: they
    // share the same inner `Arc<RwLock<HashMap<…>>>`.
    assert_eq!(writer.band_for_handle(&h), Some(Band::Sticky));
    assert_eq!(reader.band_for_handle(&h), Some(Band::Sticky));
    assert_eq!(writer.handle_count(), 1);
    assert_eq!(reader.handle_count(), 1);
}

#[test]
fn registration_on_reader_clone_is_visible_on_writer() {
    let writer = Plan::default();
    let reader = writer.clone();

    let h = [0xCD_u8; 32];
    reader.register_handle(h, Band::Volatile);

    assert_eq!(writer.band_for_handle(&h), Some(Band::Volatile));
}

#[test]
fn distinct_plans_do_not_share_state() {
    let plan_a = Plan::default();
    let plan_b = Plan::default();

    let h = [0xEF_u8; 32];
    plan_a.register_handle(h, Band::Sticky);

    assert_eq!(plan_a.band_for_handle(&h), Some(Band::Sticky));
    assert_eq!(plan_b.band_for_handle(&h), None);
}

#[test]
fn re_registration_overwrites_existing_band() {
    let plan = Plan::default();
    let h = [0x12_u8; 32];

    plan.register_handle(h, Band::Volatile);
    assert_eq!(plan.band_for_handle(&h), Some(Band::Volatile));

    plan.register_handle(h, Band::Sticky);
    assert_eq!(plan.band_for_handle(&h), Some(Band::Sticky));
    // handle_count should NOT grow on overwrite.
    assert_eq!(plan.handle_count(), 1);
}
