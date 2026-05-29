// SPDX-License-Identifier: Apache-2.0
use origin_planner::{Band, PrefixLedger, SectionId};

#[test]
fn consecutive_hits_promote_section_from_volatile_to_sliding() {
    let mut ledger = PrefixLedger::new();
    let id = SectionId::new("memories");
    ledger.record_band(id, Band::Volatile);

    // Three consecutive hits across turns crosses the promotion threshold.
    ledger.record_hit(id, 100); // 100 tokens read from cache in turn 1
    ledger.record_hit(id, 100);
    ledger.record_hit(id, 100);

    assert_eq!(ledger.suggested_band(id), Some(Band::Sliding));
}

#[test]
fn missed_section_demotes_one_band() {
    let mut ledger = PrefixLedger::new();
    let id = SectionId::new("flaky-context");
    ledger.record_band(id, Band::Sticky);
    ledger.record_miss(id);
    ledger.record_miss(id);
    assert_eq!(ledger.suggested_band(id), Some(Band::Sliding));
}

use proptest::prelude::*;

proptest! {
    #[test]
    fn consecutive_hits_never_demote(hits in 1u32..32) {
        let mut ledger = PrefixLedger::new();
        let id = SectionId::new("p");
        ledger.record_band(id, Band::Volatile);
        let start = ledger.suggested_band(id).expect("seeded");
        for _ in 0..hits {
            ledger.record_hit(id, 50);
        }
        let end = ledger.suggested_band(id).expect("present");
        // Bands order Frozen=0 < Sticky=1 < Sliding=2 < Volatile=3, so the
        // ord-numeric value of `end` must be <= `start` (closer to Frozen).
        prop_assert!(end as u8 <= start as u8);
    }
}
