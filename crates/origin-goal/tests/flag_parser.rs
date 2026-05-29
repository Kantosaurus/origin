// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_goal::{parse_goal_args, FlagParseError};

#[test]
fn condition_only() {
    let g = parse_goal_args("fix the failing tests").unwrap();
    assert_eq!(g.condition, "fix the failing tests");
    assert_eq!(g.max_iter, None);
    assert_eq!(g.token_budget, None);
}

#[test]
fn max_iter_then_cond() {
    let g = parse_goal_args("--max-iter=50 fix tests").unwrap();
    assert_eq!(g.condition, "fix tests");
    assert_eq!(g.max_iter, Some(50));
}

#[test]
fn budget_with_k_suffix() {
    let g = parse_goal_args("--budget=200k fix tests").unwrap();
    assert_eq!(g.token_budget, Some(200_000));
}

#[test]
fn budget_with_m_suffix() {
    let g = parse_goal_args("--budget=1m fix tests").unwrap();
    assert_eq!(g.token_budget, Some(1_000_000));
}

#[test]
fn budget_plain_number() {
    let g = parse_goal_args("--budget=12345 fix tests").unwrap();
    assert_eq!(g.token_budget, Some(12_345));
}

#[test]
fn both_flags() {
    let g = parse_goal_args("--max-iter=5 --budget=50k fix tests").unwrap();
    assert_eq!(g.max_iter, Some(5));
    assert_eq!(g.token_budget, Some(50_000));
    assert_eq!(g.condition, "fix tests");
}

#[test]
fn flags_after_condition_text_are_part_of_condition() {
    let g = parse_goal_args("fix tests --max-iter=5").unwrap();
    assert_eq!(g.condition, "fix tests --max-iter=5");
    assert_eq!(g.max_iter, None);
}

#[test]
fn unknown_flag_rejected() {
    let err = parse_goal_args("--bogus=1 fix tests").unwrap_err();
    matches!(err, FlagParseError::UnknownFlag(_));
}

#[test]
fn empty_condition_rejected() {
    let err = parse_goal_args("--max-iter=5").unwrap_err();
    matches!(err, FlagParseError::EmptyCondition);
}

#[test]
fn condition_with_embedded_double_dash_kept() {
    let g = parse_goal_args("rewrite the --foo flag handler").unwrap();
    assert_eq!(g.condition, "rewrite the --foo flag handler");
}

#[test]
fn max_iter_non_numeric_rejected() {
    let err = parse_goal_args("--max-iter=abc fix").unwrap_err();
    matches!(err, FlagParseError::InvalidValue { .. });
}

#[test]
fn condition_exceeding_4000_chars_rejected() {
    let big = "x".repeat(4001);
    let err = parse_goal_args(&big).unwrap_err();
    matches!(err, FlagParseError::ConditionTooLong);
}

// Bug #1: parse_budget must not panic on values whose last UTF-8 byte happens
// to coincide with an ASCII suffix marker (k/K/m/M). In practice, valid UTF-8
// continuation bytes are 0x80..=0xBF, so a multi-byte char can never *end* in
// 0x6B/0x4B/0x6D/0x4D — the panic is unreachable for valid UTF-8 input. But
// the code was fragile-looking (byte-level slicing keyed on byte equality);
// after refactoring to `chars().last()`, a non-ASCII suffix must surface as a
// clean InvalidValue, never a panic.
#[test]
fn budget_with_multibyte_suffix_returns_error_not_panic() {
    // 'ä' is U+00E4 = 0xC3 0xA4. The last byte (0xA4) is a continuation byte,
    // never matches an ASCII suffix marker.
    let err = parse_goal_args("--budget=10ä do thing").unwrap_err();
    assert!(matches!(err, FlagParseError::InvalidValue { .. }));
}

// Bug #13: --max-iter=0 must be rejected at parse time. A goal with 0 max-iter
// hits cap_check immediately and drops the user prompt.
#[test]
fn max_iter_zero_rejected() {
    let err = parse_goal_args("--max-iter=0 do thing").unwrap_err();
    assert!(matches!(err, FlagParseError::InvalidValue { .. }));
}

// Bug #13: --budget=0 must be rejected at parse time. Same reasoning — a
// 0-budget goal triggers BudgetExhausted on first iteration.
#[test]
fn budget_zero_rejected() {
    let err = parse_goal_args("--budget=0 do thing").unwrap_err();
    assert!(matches!(err, FlagParseError::InvalidValue { .. }));
    let err2 = parse_goal_args("--budget=0k do thing").unwrap_err();
    assert!(matches!(err2, FlagParseError::InvalidValue { .. }));
}

// Bug #19: a duplicate flag (e.g. --budget=1k --budget=2k) must be rejected
// rather than silently last-wins.
#[test]
fn duplicate_budget_flag_rejected() {
    let err = parse_goal_args("--budget=1k --budget=2k cond").unwrap_err();
    assert!(matches!(err, FlagParseError::DuplicateFlag(_)));
}

#[test]
fn duplicate_max_iter_flag_rejected() {
    let err = parse_goal_args("--max-iter=3 --max-iter=5 cond").unwrap_err();
    assert!(matches!(err, FlagParseError::DuplicateFlag(_)));
}
