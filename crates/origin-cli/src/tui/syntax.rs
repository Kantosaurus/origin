// SPDX-License-Identifier: Apache-2.0
//! Dependency-free lexical syntax tint. Pure, no I/O.
//!
//! Wave 0 stub: locks the `Lang`/`Tok`/`Span` contract and the `tint` /
//! `lang_from_label` signatures so `codeblock.rs` (Wave-1 Task B) and the Task A
//! lexer can be built in parallel against a stable surface. Bodies are filled in
//! Wave 1 (Task A).

// The stub bodies look const-foldable now; Wave 1 fills them with the real
// (non-const) lexer, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

/// A source language the lexical tint understands. Unknown languages get no
/// tint (an empty span list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Js,
    Ts,
    Py,
    Json,
    Bash,
    Go,
}

/// A lexical token class — the granularity the tint colors at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Keyword,
    Str,
    Comment,
    Num,
    Ident,
    Punct,
}

/// A tinted byte range within a single source line. `start`/`len` are byte
/// offsets (UTF-8 safe — produced from char-index boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub kind: Tok,
}

/// Resolve a fenced-code language label (e.g. `rust`, `ts`, `sh`) to a [`Lang`].
/// Unknown labels return `None` (rendered untinted).
#[must_use]
pub fn lang_from_label(_s: &str) -> Option<Lang> {
    // Wave 1 (Task A) fills this.
    None
}

/// Lexically tint one source line, returning non-overlapping in-range byte
/// spans. Pure and panic-free on partial/streaming lines.
#[must_use]
pub fn tint(_lang: Lang, _line: &str) -> Vec<Span> {
    // Wave 1 (Task A) fills this.
    Vec::new()
}
