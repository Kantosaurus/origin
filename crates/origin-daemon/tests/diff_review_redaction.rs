// SPDX-License-Identifier: Apache-2.0
//! C6 M1: the review diff is built from redacted content, so a secret that
//! spans lines (and straddles the 60,000-byte cut) never leaks. Its own test
//! binary: it installs the process-wide redactor.

use origin_daemon::diff_review::{run_diff, snapshot, Run, MAX_DIFF_BYTES};

#[test]
fn a_multi_line_secret_straddling_the_cut_never_leaks() {
    let d = tempfile::tempdir().expect("tempdir");
    let first = "c6-first-half-of-a-secret-0123456789";
    let second = format!("c6-second-half-of-the-secret-{}", "9".repeat(40));
    let secret = format!("{first}\n{second}");
    let p = d.path().join("cut.txt").to_string_lossy().into_owned();
    let mut run = Run::new();
    let _ = snapshot(vec![p.clone()], &mut run);
    let body = |n: usize| format!("{}{secret}\n", format!("{}\n", "f".repeat(38)).repeat(n));
    // A filler size where the cut keeps the secret's first line and drops the
    // second: redacting the diff after building (or cutting) it would leak it.
    let n = (1..1_900)
        .find(|&n| {
            let raw = similar::TextDiff::from_lines("", &body(n))
                .unified_diff()
                .header(&format!("a/{p}"), &format!("b/{p}"))
                .to_string();
            raw.find(first).is_some_and(|at| {
                let end = at + first.len() + 1;
                end < MAX_DIFF_BYTES && end + 1 + second.len() > MAX_DIFF_BYTES
            })
        })
        .expect("a filler size that straddles the cut");
    std::fs::write(&p, body(n)).expect("write");
    origin_daemon::redact::install(origin_daemon::redact::Redactor::new([(
        "C6_TEST_TOKEN".to_string(),
        secret,
    )]));
    let diff = run_diff(&run).expect("changed");
    assert!(
        !diff.contains("c6-first-half"),
        "half a secret leaked past the cut"
    );
}
