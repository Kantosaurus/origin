// SPDX-License-Identifier: Apache-2.0
//! Task 11: the ponytail ruleset block appears in the assembled system prompt
//! for `Full` and is absent for `Off`. The daemon adds `&ponytail_block` to the
//! system-prompt `parts` array as a pure concatenation, so asserting on
//! `system_block` directly locks the wiring contract (an empty block is dropped
//! by the array's non-empty filter).

#[test]
fn ponytail_block_present_for_full_absent_for_off() {
    assert!(origin_ponytail::system_block(origin_ponytail::PonytailMode::Full)
        .contains("origin-ponytail"));
    assert!(origin_ponytail::system_block(origin_ponytail::PonytailMode::Off).is_empty());
}
