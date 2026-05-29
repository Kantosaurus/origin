// SPDX-License-Identifier: Apache-2.0
//! `MS_PER_DAY` is exported as a crate-public constant so daemon/admin
//! code can reference the same value `Consolidator`/`Injector` use for
//! age-based scoring.

use origin_mem::{MS_PER_DAY, SECS_PER_DAY};

#[test]
fn ms_per_day_matches_seconds_per_day_times_thousand() {
    // 24 * 60 * 60 = 86_400
    assert_eq!(SECS_PER_DAY, 86_400);
    // f32 round-trip should be exact for this value.
    #[allow(clippy::cast_precision_loss)]
    let expected = (SECS_PER_DAY as f32) * 1000.0;
    assert!((MS_PER_DAY - expected).abs() < f32::EPSILON);
}

#[test]
fn ms_per_day_is_canonical_constant() {
    assert!((MS_PER_DAY - 86_400_000.0_f32).abs() < f32::EPSILON);
}
