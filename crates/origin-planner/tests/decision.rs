// SPDX-License-Identifier: Apache-2.0
use origin_planner::{Band, WireDecision};

#[test]
fn small_volatile_inlines() {
    let d = WireDecision::for_block(Band::Volatile, 128);
    assert_eq!(d, WireDecision::Inline);
}

#[test]
fn large_volatile_references() {
    let d = WireDecision::for_block(Band::Volatile, 10_000);
    assert_eq!(d, WireDecision::Reference);
}

#[test]
fn anything_in_frozen_inlines() {
    let d = WireDecision::for_block(Band::Frozen, 10_000);
    assert_eq!(d, WireDecision::Inline);
}
