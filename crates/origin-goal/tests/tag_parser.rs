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
