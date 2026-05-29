// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_goal::verifier::{parse_verdict, Verdict, VerifierError};

#[test]
fn parses_met() {
    assert_eq!(parse_verdict("VERDICT: met").unwrap(), Verdict::Met);
}

#[test]
fn parses_not_met_with_em_dash() {
    let v = parse_verdict("VERDICT: not_met — tests still failing").unwrap();
    assert_eq!(
        v,
        Verdict::NotMet {
            reason: "tests still failing".into()
        }
    );
}

#[test]
fn parses_not_met_with_ascii_dash() {
    let v = parse_verdict("VERDICT: not_met - missing migration").unwrap();
    assert_eq!(
        v,
        Verdict::NotMet {
            reason: "missing migration".into()
        }
    );
}

#[test]
fn parses_not_met_no_separator() {
    let v = parse_verdict("VERDICT: not_met tests red").unwrap();
    assert_eq!(
        v,
        Verdict::NotMet {
            reason: "tests red".into()
        }
    );
}

#[test]
fn ignores_preamble_lines() {
    let raw = "Some preamble Haiku felt like emitting.\nVERDICT: met";
    assert_eq!(parse_verdict(raw).unwrap(), Verdict::Met);
}

#[test]
fn malformed_when_no_verdict_line() {
    matches!(
        parse_verdict("VERDICT_OF_THE_PEOPLE"),
        Err(VerifierError::Malformed(_))
    );
}

#[test]
fn malformed_when_unknown_verdict_word() {
    matches!(parse_verdict("VERDICT: maybe"), Err(VerifierError::Malformed(_)));
}
