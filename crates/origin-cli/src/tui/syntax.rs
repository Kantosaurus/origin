// SPDX-License-Identifier: Apache-2.0
//! Dependency-free lexical syntax tint. Pure, no I/O.
//!
//! A tiny hand-rolled lexer that produces per-line, non-overlapping byte-range
//! [`Span`]s classified into a small [`Tok`] vocabulary
//! (keyword / string / comment / number / ident / punct). It is intentionally
//! *lexical only* — it scans one line at a time, knows nothing about the lines
//! around it, and never panics on a partial / streaming / truncated line. That
//! makes it cheap enough to run on every visible code row each frame and safe to
//! call while the model is still emitting a code block.
//!
//! Color is **not** decided here: [`tint`] only classifies ranges; `codeblock.rs`
//! maps each [`Tok`] to a `Tokens` color. Keeping the lexer color-free is what
//! lets it stay dependency-free and pure.
//!
//! ## Design notes
//! - **UTF-8 safety.** All scanning advances by whole `char`s and records
//!   *byte* offsets, so a multi-byte char (e.g. `é`, `你`, an emoji) never
//!   produces a span boundary in the middle of a code point. Spans are always on
//!   `char` boundaries and always within `line.len()`.
//! - **Non-overlapping, ordered.** Spans are emitted left to right and never
//!   overlap (the scanner consumes each region exactly once).
//! - **Streaming tolerance.** An unterminated string / unterminated block
//!   comment simply runs to end-of-line; there is no look-ahead past the line.

// `tint`/`Lang`/`Tok`/`Span` are consumed live by `mod.rs::render_code_tint`
// (INT-2); `lang_from_label` is used only by `codeblock::layout_code`, which the
// flat-model draw path does not call yet (see codeblock.rs), so a sliver stays
// dead until the structured-render path lands.
#![allow(dead_code)] // lang_from_label only used by the (deferred) codeblock path

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
///
/// The label is lowercased and any trailing fence info (e.g. `rust,ignore` or
/// `python title=foo`) is dropped — only the leading token is matched. Common
/// aliases (`rs`, `py`, `sh`, `golang`, …) map through.
#[must_use]
pub fn lang_from_label(s: &str) -> Option<Lang> {
    // Take only the first whitespace/comma/colon-delimited token, lowercased.
    let head = s
        .trim()
        .split(|c: char| c.is_whitespace() || c == ',' || c == ':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    Some(match head.as_str() {
        "rust" | "rs" => Lang::Rust,
        "js" | "javascript" | "jsx" | "mjs" | "cjs" | "node" => Lang::Js,
        "ts" | "typescript" | "tsx" => Lang::Ts,
        "py" | "python" | "python3" => Lang::Py,
        "json" | "jsonc" | "json5" => Lang::Json,
        "bash" | "sh" | "shell" | "zsh" | "console" | "shell-session" => Lang::Bash,
        "go" | "golang" => Lang::Go,
        _ => return None,
    })
}

/// Lexically tint one source line, returning non-overlapping in-range byte
/// spans. Pure and panic-free on partial/streaming lines.
#[must_use]
pub fn tint(lang: Lang, line: &str) -> Vec<Span> {
    match lang {
        Lang::Rust => Lexer::new(line, Syntax::rust()).run(),
        Lang::Js => Lexer::new(line, Syntax::js()).run(),
        Lang::Ts => Lexer::new(line, Syntax::ts()).run(),
        Lang::Py => Lexer::new(line, Syntax::py()).run(),
        Lang::Json => Lexer::new(line, Syntax::json()).run(),
        Lang::Bash => Lexer::new(line, Syntax::bash()).run(),
        Lang::Go => Lexer::new(line, Syntax::go()).run(),
    }
}

// ---------------------------------------------------------------------------
// Per-language syntax description
// ---------------------------------------------------------------------------

/// A compact, table-driven description of one language's lexical surface — the
/// knobs the shared [`Lexer`] reads. Keeping the per-language data here (and the
/// scanning generic) means adding a language is a small data change, not a new
/// scanner.
struct Syntax {
    /// Reserved words tinted as [`Tok::Keyword`]. Compared against a scanned
    /// identifier exactly (case-sensitive).
    keywords: &'static [&'static str],
    /// Line-comment lead-ins (e.g. `//`, `#`). The first match runs the rest of
    /// the line as a comment.
    line_comments: &'static [&'static str],
    /// Whether `/* … */` block comments exist (scanned to the `*/` or, if
    /// unterminated on this line, to end-of-line).
    block_comments: bool,
    /// String delimiters. Each entry is an opening (== closing) quote char. The
    /// scanner reads to the matching unescaped close or end-of-line.
    string_delims: &'static [char],
    /// Whether a backslash escapes the next char inside a string (true for most
    /// C-family / JSON; false for shells where we keep it simple).
    backslash_escapes: bool,
    /// Whether a `$name` / `${name}` sigil should be tinted as a variable
    /// reference ([`Tok::Ident`]) — used by Bash outside strings.
    dollar_vars: bool,
}

impl Syntax {
    const fn base() -> Self {
        Self {
            keywords: &[],
            line_comments: &[],
            block_comments: false,
            string_delims: &['"'],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }

    const fn rust() -> Self {
        Self {
            keywords: &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
                "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
                "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
                "type", "unsafe", "use", "where", "while",
            ],
            line_comments: &["//"],
            block_comments: true,
            string_delims: &['"'],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }

    const fn js() -> Self {
        Self {
            keywords: &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "default",
                "delete",
                "do",
                "else",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "let",
                "new",
                "null",
                "of",
                "return",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "with",
                "yield",
            ],
            line_comments: &["//"],
            block_comments: true,
            string_delims: &['"', '\'', '`'],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }

    const fn ts() -> Self {
        Self {
            keywords: &[
                "abstract",
                "any",
                "as",
                "async",
                "await",
                "boolean",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "declare",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "implements",
                "import",
                "in",
                "instanceof",
                "interface",
                "is",
                "keyof",
                "let",
                "namespace",
                "never",
                "new",
                "null",
                "number",
                "object",
                "of",
                "private",
                "protected",
                "public",
                "readonly",
                "return",
                "string",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "type",
                "typeof",
                "undefined",
                "unknown",
                "var",
                "void",
                "while",
                "yield",
            ],
            line_comments: &["//"],
            block_comments: true,
            string_delims: &['"', '\'', '`'],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }

    const fn py() -> Self {
        Self {
            keywords: &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
                "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
                "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "False", "try", "while",
                "with", "yield", "match", "case",
            ],
            line_comments: &["#"],
            block_comments: false,
            string_delims: &['"', '\''],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }

    const fn json() -> Self {
        Self {
            keywords: &["true", "false", "null"],
            line_comments: &[],
            block_comments: false,
            string_delims: &['"'],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }

    const fn bash() -> Self {
        Self {
            keywords: &[
                "if", "then", "elif", "else", "fi", "case", "esac", "for", "select", "while", "until", "do",
                "done", "in", "function", "time", "coproc", "return", "break", "continue", "local", "export",
                "readonly", "declare", "unset", "echo", "cd", "exit", "source", "alias",
            ],
            line_comments: &["#"],
            block_comments: false,
            string_delims: &['"', '\''],
            backslash_escapes: false,
            dollar_vars: true,
        }
    }

    const fn go() -> Self {
        Self {
            keywords: &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "type",
                "var",
                "nil",
                "true",
                "false",
                "iota",
            ],
            line_comments: &["//"],
            block_comments: true,
            string_delims: &['"', '`'],
            backslash_escapes: true,
            dollar_vars: false,
        }
    }
}

// ---------------------------------------------------------------------------
// The shared scanner
// ---------------------------------------------------------------------------

/// A streaming, single-line lexer over `char`s, recording byte offsets.
///
/// Holds the line as a `Vec<(byte_offset, char)>` so every emitted [`Span`]
/// boundary lands on a real char boundary regardless of multi-byte content.
struct Lexer<'a> {
    /// Source line.
    src: &'a str,
    /// `(byte_offset, char)` for each char in `src`, in order. A terminating
    /// sentinel of `(src.len(), '\0')` is appended so end-of-token byte offsets
    /// can be read uniformly as `chars[i].0`.
    chars: Vec<(usize, char)>,
    /// Cursor into `chars` (a *char* index, not a byte offset).
    i: usize,
    /// The active language description.
    syn: Syntax,
    /// Accumulated spans.
    out: Vec<Span>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, syn: Syntax) -> Self {
        let mut chars: Vec<(usize, char)> = src.char_indices().collect();
        chars.push((src.len(), '\0')); // sentinel for end-byte lookups
        Self {
            src,
            chars,
            i: 0,
            syn,
            out: Vec::new(),
        }
    }

    /// Number of real chars (excluding the sentinel).
    fn len(&self) -> usize {
        self.chars.len() - 1
    }

    /// The char at char-index `i` (sentinel `'\0'` when at/over the end).
    fn ch(&self, i: usize) -> char {
        self.chars[i.min(self.len())].1
    }

    /// The byte offset at char-index `i` (the sentinel yields `src.len()`).
    fn byte(&self, i: usize) -> usize {
        self.chars[i.min(self.len())].0
    }

    /// Push a span covering char-indices `[from, to)`. Skips empties; clamps to
    /// the line so it can never exceed `src.len()`.
    fn push(&mut self, from: usize, to: usize, kind: Tok) {
        if to <= from {
            return;
        }
        let start = self.byte(from);
        let end = self.byte(to);
        if end <= start {
            return;
        }
        self.out.push(Span {
            start,
            len: end - start,
            kind,
        });
    }

    /// Does the source, starting at char-index `i`, begin with `pat`?
    fn starts_with_at(&self, i: usize, pat: &str) -> bool {
        let b = self.byte(i);
        self.src[b..].starts_with(pat)
    }

    fn run(mut self) -> Vec<Span> {
        let n = self.len();
        while self.i < n {
            let c = self.ch(self.i);

            // 1. Line comments (longest lead-in wins; e.g. would matter if a
            //    language had both `#` and `#!`).
            if let Some(lead) = self
                .syn
                .line_comments
                .iter()
                .filter(|p| self.starts_with_at(self.i, p))
                .max_by_key(|p| p.len())
            {
                let len = lead.len();
                // Advance char cursor past the lead-in, then to end-of-line.
                let start = self.i;
                self.advance_bytes(len);
                self.i = n;
                self.push(start, n, Tok::Comment);
                break;
            }

            // 2. Block comments `/* ... */`.
            if self.syn.block_comments && self.starts_with_at(self.i, "/*") {
                self.scan_block_comment();
                continue;
            }

            // 3. Strings.
            if self.syn.string_delims.contains(&c) {
                self.scan_string(c);
                continue;
            }

            // 4. Bash `$VAR` / `${VAR}` / `$1` variable references.
            if self.syn.dollar_vars && c == '$' {
                self.scan_dollar_var();
                continue;
            }

            // 5. Numbers (must not start mid-identifier; a leading digit, or a
            //    `.` immediately followed by a digit).
            if c.is_ascii_digit() || (c == '.' && self.ch(self.i + 1).is_ascii_digit()) {
                self.scan_number();
                continue;
            }

            // 6. Identifiers / keywords.
            if is_ident_start(c) {
                self.scan_ident();
                continue;
            }

            // 7. Punctuation (single char). Whitespace is left untinted.
            if !c.is_whitespace() {
                self.push(self.i, self.i + 1, Tok::Punct);
            }
            self.i += 1;
        }
        self.out
    }

    /// Advance the char cursor so it has consumed at least `bytes` source bytes
    /// from the current position (used to step over a multi-byte-safe lead-in).
    fn advance_bytes(&mut self, bytes: usize) {
        let target = self.byte(self.i) + bytes;
        while self.i < self.len() && self.byte(self.i) < target {
            self.i += 1;
        }
    }

    fn scan_block_comment(&mut self) {
        let start = self.i;
        let n = self.len();
        // Consume the opening `/*`.
        self.advance_bytes(2);
        while self.i < n {
            if self.starts_with_at(self.i, "*/") {
                self.advance_bytes(2);
                break;
            }
            self.i += 1;
        }
        self.push(start, self.i, Tok::Comment);
    }

    fn scan_string(&mut self, quote: char) {
        let start = self.i;
        let n = self.len();
        self.i += 1; // opening quote
        while self.i < n {
            let c = self.ch(self.i);
            if self.syn.backslash_escapes && c == '\\' {
                // Escape consumes the next char too (if any). Safe at end-of-line.
                self.i += 2;
                continue;
            }
            if c == quote {
                self.i += 1; // closing quote
                break;
            }
            self.i += 1;
        }
        // Unterminated string ⇒ runs to end-of-line (streaming tolerance).
        let end = self.i.min(n);
        self.push(start, end, Tok::Str);
    }

    fn scan_dollar_var(&mut self) {
        let start = self.i;
        let n = self.len();
        self.i += 1; // the `$`
        match self.ch(self.i) {
            '{' => {
                // `${...}` — consume to the closing brace (or end-of-line).
                self.i += 1;
                while self.i < n && self.ch(self.i) != '}' {
                    self.i += 1;
                }
                if self.i < n {
                    self.i += 1; // closing brace
                }
            }
            c if is_ident_start(c) || c.is_ascii_digit() => {
                while self.i < n {
                    let cc = self.ch(self.i);
                    if is_ident_continue(cc) {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
            }
            // `$` alone (e.g. `$?`, `$#`, `$$`) — take one following sigil char.
            c if !c.is_whitespace() && c != '\0' => {
                self.i += 1;
            }
            _ => {}
        }
        self.push(start, self.i, Tok::Ident);
    }

    fn scan_number(&mut self) {
        let start = self.i;
        let n = self.len();
        // Optional radix prefix: 0x / 0o / 0b (and Go/Rust style).
        if self.ch(self.i) == '0' {
            let p = self.ch(self.i + 1).to_ascii_lowercase();
            if matches!(p, 'x' | 'o' | 'b') {
                self.i += 2;
            }
        }
        while self.i < n {
            let c = self.ch(self.i);
            // Digits, hex letters, separators (`_`), a single decimal point, and
            // exponent markers — kept permissive on purpose (lexical only).
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                self.i += 1;
            } else if (c == '+' || c == '-')
                && matches!(self.ch(self.i.wrapping_sub(1)).to_ascii_lowercase(), 'e')
            {
                // Signed exponent: `1e-9`.
                self.i += 1;
            } else {
                break;
            }
        }
        self.push(start, self.i, Tok::Num);
    }

    fn scan_ident(&mut self) {
        let start = self.i;
        let n = self.len();
        while self.i < n && is_ident_continue(self.ch(self.i)) {
            self.i += 1;
        }
        let b0 = self.byte(start);
        let b1 = self.byte(self.i);
        let word = &self.src[b0..b1];
        let kind = if self.syn.keywords.contains(&word) {
            Tok::Keyword
        } else {
            Tok::Ident
        };
        self.push(start, self.i, kind);
    }
}

/// Identifier-start: a Unicode alphabetic char or `_`. (Permissive — the tint is
/// cosmetic, not a real parser.)
fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

/// Identifier-continue: start chars plus digits.
fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Extract the substring a span covers (asserts the span is in-range and on
    /// char boundaries — `str` indexing panics otherwise, which would fail the
    /// test loudly rather than silently).
    fn slice<'a>(line: &'a str, s: &Span) -> &'a str {
        &line[s.start..s.start + s.len]
    }

    /// All spans must be ordered, non-overlapping, and within the line length.
    fn assert_well_formed(line: &str, spans: &[Span]) {
        let mut prev_end = 0usize;
        for s in spans {
            assert!(
                s.start >= prev_end,
                "spans overlap or are unordered: {spans:?} in {line:?}"
            );
            assert!(
                s.start + s.len <= line.len(),
                "span {s:?} exceeds line len {} in {line:?}",
                line.len()
            );
            // Must land on char boundaries (this indexing panics otherwise).
            let _ = slice(line, s);
            prev_end = s.start + s.len;
        }
    }

    fn first_of(spans: &[Span], kind: Tok) -> Option<Span> {
        spans.iter().copied().find(|s| s.kind == kind)
    }

    // ---- lang_from_label ----

    #[test]
    fn lang_from_label_known_and_aliases() {
        assert_eq!(lang_from_label("rust"), Some(Lang::Rust));
        assert_eq!(lang_from_label("rs"), Some(Lang::Rust));
        assert_eq!(lang_from_label("JavaScript"), Some(Lang::Js));
        assert_eq!(lang_from_label("ts"), Some(Lang::Ts));
        assert_eq!(lang_from_label("py"), Some(Lang::Py));
        assert_eq!(lang_from_label("python3"), Some(Lang::Py));
        assert_eq!(lang_from_label("json"), Some(Lang::Json));
        assert_eq!(lang_from_label("sh"), Some(Lang::Bash));
        assert_eq!(lang_from_label("bash"), Some(Lang::Bash));
        assert_eq!(lang_from_label("golang"), Some(Lang::Go));
    }

    #[test]
    fn lang_from_label_strips_fence_info_and_trims() {
        assert_eq!(lang_from_label("  rust "), Some(Lang::Rust));
        assert_eq!(lang_from_label("rust,ignore"), Some(Lang::Rust));
        assert_eq!(lang_from_label("python title=foo"), Some(Lang::Py));
    }

    #[test]
    fn lang_from_label_unknown_is_none() {
        assert_eq!(lang_from_label(""), None);
        assert_eq!(lang_from_label("cobol"), None);
        assert_eq!(lang_from_label("plaintext"), None);
    }

    // ---- unknown language path is impossible to construct, but assert empties ----

    #[test]
    fn empty_line_yields_no_spans() {
        for lang in [
            Lang::Rust,
            Lang::Js,
            Lang::Ts,
            Lang::Py,
            Lang::Json,
            Lang::Bash,
            Lang::Go,
        ] {
            assert!(tint(lang, "").is_empty(), "{lang:?} on empty line");
            assert!(tint(lang, "   \t  ").is_empty(), "{lang:?} on whitespace");
        }
    }

    /// Per the contract, a label that does not resolve to a `Lang` is never
    /// tinted — the caller skips `tint` entirely. We model "unknown ⇒ empty" at
    /// the label boundary.
    #[test]
    fn unknown_label_means_no_tint() {
        assert!(lang_from_label("nope").is_none());
        // The renderer renders untinted (no span list) when this is None.
    }

    // ---- Rust ----

    #[test]
    fn rust_fn_let_comment_string_number() {
        let line = r#"let x = foo(42, "hi"); // tail"#;
        let spans = tint(Lang::Rust, line);
        assert_well_formed(line, &spans);

        // `let` is a keyword.
        let kw = first_of(&spans, Tok::Keyword).expect("a keyword span");
        assert_eq!(slice(line, &kw), "let");

        // Number 42.
        let num = first_of(&spans, Tok::Num).expect("a number span");
        assert_eq!(slice(line, &num), "42");

        // String "hi" (with quotes).
        let s = first_of(&spans, Tok::Str).expect("a string span");
        assert_eq!(slice(line, &s), "\"hi\"");

        // Trailing line comment.
        let c = first_of(&spans, Tok::Comment).expect("a comment span");
        assert_eq!(slice(line, &c), "// tail");

        // `foo` is an identifier, not a keyword.
        assert!(spans
            .iter()
            .any(|s| s.kind == Tok::Ident && slice(line, s) == "foo"));
    }

    #[test]
    fn rust_fn_keyword_and_block_comment() {
        let line = "fn main() { /* note */ }";
        let spans = tint(Lang::Rust, line);
        assert_well_formed(line, &spans);
        let kw = first_of(&spans, Tok::Keyword).unwrap();
        assert_eq!(slice(line, &kw), "fn");
        let c = first_of(&spans, Tok::Comment).unwrap();
        assert_eq!(slice(line, &c), "/* note */");
    }

    #[test]
    fn rust_unterminated_string_runs_to_eol() {
        let line = r#"let s = "unterminated"#;
        let spans = tint(Lang::Rust, line);
        assert_well_formed(line, &spans);
        let s = first_of(&spans, Tok::Str).unwrap();
        assert_eq!(slice(line, &s), "\"unterminated");
    }

    #[test]
    fn rust_escaped_quote_inside_string() {
        let line = r#""a\"b" rest"#;
        let spans = tint(Lang::Rust, line);
        assert_well_formed(line, &spans);
        let s = first_of(&spans, Tok::Str).unwrap();
        assert_eq!(slice(line, &s), r#""a\"b""#);
        // `rest` is a separate ident, proving the string closed correctly.
        assert!(spans
            .iter()
            .any(|sp| sp.kind == Tok::Ident && slice(line, sp) == "rest"));
    }

    // ---- JSON ----

    #[test]
    fn json_keys_strings_numbers() {
        let line = r#"{ "name": "origin", "n": 42, "ok": true }"#;
        let spans = tint(Lang::Json, line);
        assert_well_formed(line, &spans);

        // Keys and string values both lex as strings.
        let strings: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == Tok::Str)
            .map(|s| slice(line, s))
            .collect();
        assert!(strings.contains(&"\"name\""), "key tinted: {strings:?}");
        assert!(strings.contains(&"\"origin\""), "value tinted: {strings:?}");
        assert!(strings.contains(&"\"n\""));

        // Number.
        let num = first_of(&spans, Tok::Num).unwrap();
        assert_eq!(slice(line, &num), "42");

        // `true` is a JSON keyword.
        let kw = first_of(&spans, Tok::Keyword).unwrap();
        assert_eq!(slice(line, &kw), "true");
    }

    #[test]
    fn json_negative_and_float() {
        let line = r#"{ "x": -3.14e2 }"#;
        let spans = tint(Lang::Json, line);
        assert_well_formed(line, &spans);
        // `3.14e2` is captured as a number (the leading `-` is punct, which is
        // fine — lexical tint, not a value parser).
        assert!(spans
            .iter()
            .any(|s| s.kind == Tok::Num && slice(line, s) == "3.14e2"));
    }

    // ---- Bash ----

    #[test]
    fn bash_comment_and_dollar_var() {
        let line = r#"echo "$HOME/bin" $1 ${VAR} # a comment"#;
        let spans = tint(Lang::Bash, line);
        assert_well_formed(line, &spans);

        // `echo` keyword.
        let kw = first_of(&spans, Tok::Keyword).unwrap();
        assert_eq!(slice(line, &kw), "echo");

        // Trailing `#` comment.
        let c = first_of(&spans, Tok::Comment).unwrap();
        assert_eq!(slice(line, &c), "# a comment");

        // `$1` and `${VAR}` are variable refs (Ident). (`$HOME` is inside a
        // string so it is part of the string span — that's fine for a lexer.)
        let idents: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == Tok::Ident)
            .map(|s| slice(line, s))
            .collect();
        assert!(idents.contains(&"$1"), "got {idents:?}");
        assert!(idents.contains(&"${VAR}"), "got {idents:?}");
    }

    #[test]
    fn bash_full_line_comment() {
        let line = "# just a comment with $not_a_var";
        let spans = tint(Lang::Bash, line);
        assert_well_formed(line, &spans);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, Tok::Comment);
        assert_eq!(slice(line, &spans[0]), line);
    }

    // ---- Python ----

    #[test]
    fn python_def_comment_fstring() {
        let line = r"def greet(n):  # f-string demo";
        let spans = tint(Lang::Py, line);
        assert_well_formed(line, &spans);
        let kw = first_of(&spans, Tok::Keyword).unwrap();
        assert_eq!(slice(line, &kw), "def");
        let c = first_of(&spans, Tok::Comment).unwrap();
        assert_eq!(slice(line, &c), "# f-string demo");
    }

    #[test]
    fn python_fstring_body_is_a_string() {
        // The `f` prefix lexes as an ident; the quoted body is the string span.
        let line = r#"msg = f"hello {name}!""#;
        let spans = tint(Lang::Py, line);
        assert_well_formed(line, &spans);
        let s = first_of(&spans, Tok::Str).unwrap();
        assert_eq!(slice(line, &s), r#""hello {name}!""#);
        // `f` prefix present as an ident immediately before the string.
        assert!(spans
            .iter()
            .any(|sp| sp.kind == Tok::Ident && slice(line, sp) == "f"));
    }

    #[test]
    fn python_single_quote_string() {
        let line = "x = 'abc'";
        let spans = tint(Lang::Py, line);
        assert_well_formed(line, &spans);
        let s = first_of(&spans, Tok::Str).unwrap();
        assert_eq!(slice(line, &s), "'abc'");
    }

    // ---- JS / TS ----

    #[test]
    fn js_template_literal_and_keywords() {
        let line = "const s = `t`; let n = 0xFF;";
        let spans = tint(Lang::Js, line);
        assert_well_formed(line, &spans);
        let kws: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == Tok::Keyword)
            .map(|s| slice(line, s))
            .collect();
        assert!(kws.contains(&"const"));
        assert!(kws.contains(&"let"));
        // Backtick template literal lexes as a string.
        assert!(spans
            .iter()
            .any(|s| s.kind == Tok::Str && slice(line, s) == "`t`"));
        // Hex number.
        assert!(spans
            .iter()
            .any(|s| s.kind == Tok::Num && slice(line, s) == "0xFF"));
    }

    #[test]
    fn ts_type_keywords() {
        let line = "interface X { name: string }";
        let spans = tint(Lang::Ts, line);
        assert_well_formed(line, &spans);
        let kws: Vec<_> = spans
            .iter()
            .filter(|s| s.kind == Tok::Keyword)
            .map(|s| slice(line, s))
            .collect();
        assert!(kws.contains(&"interface"));
        assert!(kws.contains(&"string"));
    }

    // ---- Go ----

    #[test]
    fn go_func_and_raw_string() {
        let line = "func main() { s := `raw`; _ = 1.5 }";
        let spans = tint(Lang::Go, line);
        assert_well_formed(line, &spans);
        let kw = first_of(&spans, Tok::Keyword).unwrap();
        assert_eq!(slice(line, &kw), "func");
        assert!(spans
            .iter()
            .any(|s| s.kind == Tok::Str && slice(line, s) == "`raw`"));
        assert!(spans
            .iter()
            .any(|s| s.kind == Tok::Num && slice(line, s) == "1.5"));
    }

    // ---- UTF-8 safety ----

    #[test]
    fn utf8_multibyte_spans_stay_on_char_boundaries() {
        // Multi-byte content before/inside a string + an emoji must never split
        // a code point (assert_well_formed would panic via str indexing).
        let line = r#"let café = "naïve 🚀"; // café"#;
        let spans = tint(Lang::Rust, line);
        assert_well_formed(line, &spans);

        // The identifier `café` (with a 2-byte é) must be captured whole.
        assert!(
            spans
                .iter()
                .any(|s| s.kind == Tok::Ident && slice(line, s) == "café"),
            "café ident: {spans:?}"
        );
        // The string spans the full quoted region including the emoji.
        let s = first_of(&spans, Tok::Str).unwrap();
        assert_eq!(slice(line, &s), "\"naïve 🚀\"");
        // The comment includes the multi-byte é.
        let c = first_of(&spans, Tok::Comment).unwrap();
        assert_eq!(slice(line, &c), "// café");
    }

    #[test]
    fn no_panic_on_dangling_escape_and_lone_dollar() {
        // Backslash at end-of-line and a lone `$` must not panic or over-read.
        let l1 = r#"x = "abc\"#;
        let s1 = tint(Lang::Rust, l1);
        assert_well_formed(l1, &s1);

        let l2 = "echo $";
        let s2 = tint(Lang::Bash, l2);
        assert_well_formed(l2, &s2);

        let l3 = "/* unterminated block";
        let s3 = tint(Lang::Rust, l3);
        assert_well_formed(l3, &s3);
        assert_eq!(s3.len(), 1);
        assert_eq!(s3[0].kind, Tok::Comment);
    }
}
