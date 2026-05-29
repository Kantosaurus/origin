// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_goal::{parse_tag, TagOutcome};

#[test]
fn parses_met() {
    let s = "some assistant text\n<goal-status state=\"met\"><reason>tests green</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

#[test]
fn parses_in_progress_with_reason() {
    let s = "<goal-status state=\"in_progress\"><reason>still 3 tests failing</reason></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::InProgress { what_remains: "still 3 tests failing".to_string() }
    );
}

#[test]
fn parses_blocked_with_reason() {
    let s = "<goal-status state=\"blocked\"><reason>need DB password</reason></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::Blocked { why: "need DB password".to_string() }
    );
}

#[test]
fn missing_tag_yields_missing() {
    assert_eq!(parse_tag("plain assistant reply with no tag"), TagOutcome::Missing);
}

#[test]
fn malformed_tag_yields_missing() {
    let s = "<goal-status state=\"banana\"><reason>x</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Missing);
}

#[test]
fn multiple_tags_last_wins() {
    let s = "<goal-status state=\"in_progress\"><reason>a</reason></goal-status> \
             midtext \
             <goal-status state=\"met\"><reason>done</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

// A trailing tag with an unknown state must override an earlier valid tag
// (rightmost well-formed tag is authoritative), not silently fall back to it.
#[test]
fn trailing_unknown_tag_overrides_earlier_valid() {
    let s = "<goal-status state=\"met\"><reason>x</reason></goal-status> \
             later \
             <goal-status state=\"banana\"><reason>y</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Missing);
}

#[test]
fn state_attr_case_insensitive() {
    let s = "<goal-status state=\"MET\"></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

#[test]
fn whitespace_in_attributes_ok() {
    let s = "<goal-status   state = \"in_progress\" ><reason>x</reason></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::InProgress { what_remains: "x".to_string() }
    );
}

#[test]
fn empty_reason_ok() {
    let s = "<goal-status state=\"in_progress\"></goal-status>";
    assert_eq!(
        parse_tag(s),
        TagOutcome::InProgress { what_remains: String::new() }
    );
}

#[test]
fn extra_attributes_ignored() {
    let s = "<goal-status state=\"met\" extra=\"foo\"><reason>r</reason></goal-status>";
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

// Bug #2: extract_state must not match `state` as a prefix of a longer attr
// name (e.g. `state-extra`, `statemachine`). The real `state="met"` must still
// be found when it appears after such a decoy attribute.
#[test]
fn similar_attribute_prefix_does_not_block_real_state() {
    let s = r#"<goal-status state-extra="foo" state="met"><reason>r</reason></goal-status>"#;
    assert_eq!(parse_tag(s), TagOutcome::Met);
}

#[test]
fn state_prefix_in_other_attribute_name_ignored() {
    let s = r#"<goal-status statemachine="x"><reason>r</reason></goal-status>"#;
    assert_eq!(parse_tag(s), TagOutcome::Missing);
}
