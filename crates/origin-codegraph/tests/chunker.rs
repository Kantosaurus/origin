// SPDX-License-Identifier: Apache-2.0
use origin_codegraph::{chunker, Language};
use std::fmt::Write as _;

/// Generate a synthetic Rust file with `n` simple functions named `fn_0`, `fn_1`, ...
fn synth(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let _ = writeln!(s, "fn fn_{i}() {{ let _x = {i}; }}");
    }
    s
}

#[test]
fn one_function_edit_changes_at_most_two_chunks() {
    let before = synth(200); // ~5KB, plenty of fns
                             // Edit the body of fn_100 only.
    let needle = "fn_100() { let _x = 100; }";
    let replacement = "fn_100() { let _x = 100; let _y = 999; }";
    assert!(before.contains(needle), "fixture sanity");
    let after = before.replace(needle, replacement);

    let chunks_before = chunker::chunks_ast_biased(Language::Rust, before.as_bytes()).expect("before");
    let chunks_after = chunker::chunks_ast_biased(Language::Rust, after.as_bytes()).expect("after");

    let hashes_before: std::collections::HashSet<_> = chunks_before.iter().map(|c| c.hash).collect();
    let hashes_after: std::collections::HashSet<_> = chunks_after.iter().map(|c| c.hash).collect();

    // Hash-set difference: "after" chunks not present in "before".
    let novel = hashes_after.difference(&hashes_before).count();
    assert!(
        novel <= 2,
        "expected <= 2 novel chunks, got {novel} (before={}, after={})",
        chunks_before.len(),
        chunks_after.len(),
    );
}

#[test]
fn falls_back_when_parse_fails() {
    // Garbage bytes still chunk via plain FastCDC.
    let data: Vec<u8> = (0u32..10_000).map(|i| (i % 251) as u8).collect();
    let chunks = chunker::chunks_ast_biased(Language::Rust, &data).expect("chunk");
    assert!(!chunks.is_empty());
}
