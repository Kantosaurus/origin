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
