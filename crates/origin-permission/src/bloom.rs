// SPDX-License-Identifier: Apache-2.0
//! Small growable bloom over the rule set (N9.2).
//!
//! Used as a pre-check before the rule walk: if the bloom says "absent" we
//! skip the rule walk entirely (≥95% rejection on the test mix). False
//! positives walk the rules — a few extra hashes — and never affect
//! correctness.
//!
//! Sizing: `GrowableBloom::new(0.01, target_count)` starts in the few-hundred-
//! bits range and auto-grows as items are inserted. The "4 KiB" descriptor in
//! the original spec was an upper-bound estimate for ≈1000 rules; small rule
//! sets stay well under that.

use crate::rules::Rule;
use growable_bloom_filter::GrowableBloom;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct BloomPreCheck {
    inner: GrowableBloom,
}

impl BloomPreCheck {
    /// Build a fresh bloom containing every rule's canonical key.
    #[must_use]
    pub fn build(rules: &[Rule]) -> Self {
        // 1% false-positive target, sized for the actual rule count + headroom.
        let target_fp = 0.01;
        let target_count = rules.len().max(64);
        let mut inner = GrowableBloom::new(target_fp, target_count);
        for r in rules {
            inner.insert(r.key());
        }
        Self { inner }
    }

    /// Returns `true` if the key *might* be present in the rule set.
    /// `false` means the key is definitely absent.
    #[must_use]
    pub fn maybe_contains(&self, key: &str) -> bool {
        self.inner.contains(key.to_string())
    }
}
