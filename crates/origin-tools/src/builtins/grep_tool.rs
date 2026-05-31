// SPDX-License-Identifier: Apache-2.0
//! `Grep` v2 — `files_with_matches` default, `head_limit`, type/glob, context lines.

use crate::error::{ErrClass, ToolError};
use crate::{SideEffects, Tier, Urgency};
use grep_regex::RegexMatcher;
use grep_searcher::{sinks::UTF8, SearcherBuilder};
use ignore::types::TypesBuilder;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy)]
pub enum OutputMode {
    FilesWithMatches,
    Content,
    Count,
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Default)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    pub r#type: Option<String>,
    pub output_mode: Option<OutputMode>,
    pub head_limit: Option<u32>,
    pub before: u32,
    pub after: u32,
    pub line_numbers: bool,
    pub multiline: bool,
    /// Already-seen line regions to elide from `content`-mode results.
    ///
    /// Defaults to `None` (absent). Only consulted when the
    /// `ORIGIN_AGENTGREP_TRUNCATE=1` env gate is set; otherwise it is ignored
    /// and the result is byte-identical to a run without this field. Populated
    /// by the daemon from session history, not by the tool input schema.
    pub exposure: Option<Vec<ExposureWindow>>,
}

/// A contiguous, already-exposed region of a file.
///
/// Lines are 1-based and the range is inclusive on both ends
/// (`start_line..=end_line`), matching the 1-based `line` numbers carried by
/// `content`-mode match objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureWindow {
    /// File path this window applies to. Compared verbatim against the `file`
    /// string carried by `content`-mode match objects.
    pub file: String,
    /// First exposed line (1-based, inclusive).
    pub start_line: u64,
    /// Last exposed line (1-based, inclusive).
    pub end_line: u64,
}

/// Opt-in sentinel: a `pattern` beginning with this prefix selects the agentgrep
/// relations/regions query DSL instead of a raw regex search. A real regex never
/// starts with this literal unless the caller intends the DSL, so the default
/// `Grep` contract (no prefix) is byte-identical to before.
const DSL_PREFIX: &str = "agentgrep:";

/// A parsed agentgrep query.
///
/// The grammar is intentionally tiny (one verb + one operand):
/// - `outline:<path>` — enumerate the definitions/symbols in a single file.
/// - `refs:<symbol>` — find references to a bare identifier, each annotated with
///   its nearest enclosing definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DslQuery {
    /// List the definitions/symbols declared in `path`.
    Outline { path: String },
    /// Find references to the bare identifier `symbol`.
    Refs { symbol: String },
}

/// Parse a raw `pattern` into a [`DslQuery`], or `None` when it is not a DSL
/// query (i.e. it lacks the [`DSL_PREFIX`] sentinel, so it is an ordinary regex).
///
/// Recognized verbs: `outline:<path>` and `refs:<symbol>`. An empty operand or
/// an unknown verb yields `Some(Err(..))` so the caller can surface a clear
/// validation error rather than silently degrading to a regex search.
///
/// # Errors
/// `validation.dsl` when the prefix is present but the verb/operand is invalid.
#[must_use]
pub fn parse_dsl(pattern: &str) -> Option<Result<DslQuery, ToolError>> {
    let body = pattern.strip_prefix(DSL_PREFIX)?;
    let err = |msg: &str| ToolError::new(ErrClass::Validation, "dsl", msg.to_string());
    let parsed = body.split_once(':').map_or_else(
        || Err(err("agentgrep DSL expects `<verb>:<operand>` (e.g. outline:src/lib.rs)")),
        |(verb, operand)| {
            let operand = operand.trim();
            if operand.is_empty() {
                return Err(err("agentgrep DSL operand must not be empty"));
            }
            match verb.trim() {
                "outline" => Ok(DslQuery::Outline { path: operand.to_string() }),
                "refs" => Ok(DslQuery::Refs { symbol: operand.to_string() }),
                other => Err(err(&format!("unknown agentgrep verb `{other}` (expected outline|refs)"))),
            }
        },
    );
    Some(parsed)
}

/// Enumerate the definition/symbol leaders declared in an in-memory source.
///
/// Reuses the enclosing-symbol scanner's [`is_definition_line`] recognizer so the
/// outline matches the same ~18-language definition set as the agentgrep fold.
/// Each returned entry is `(1-based line, trimmed definition text)`.
#[must_use]
pub fn outline_symbols(lines: &[String]) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = Vec::new();
    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        if is_definition_line(trimmed) {
            let lnum = u64::try_from(idx).unwrap_or(u64::MAX).saturating_add(1);
            let sym: String = trimmed.trim_end().chars().take(120).collect();
            out.push((lnum, sym));
        }
    }
    out
}

/// Find references to a bare identifier `symbol` in an in-memory source.
///
/// A reference is a whole-word occurrence of `symbol` (delimited by non-identifier
/// characters on both sides), so `symbol` matches `symbol(x)` but not `symbolic`.
/// Each hit is annotated with its nearest enclosing definition via
/// [`enclosing_symbol`]. Returns `(1-based line, line text, enclosing symbol)`.
#[must_use]
pub fn refs_in_source(lines: &[String], symbol: &str) -> Vec<(u64, String, Option<String>)> {
    let mut out: Vec<(u64, String, Option<String>)> = Vec::new();
    if symbol.is_empty() {
        return out;
    }
    for (idx, raw) in lines.iter().enumerate() {
        if !line_has_word(raw, symbol) {
            continue;
        }
        let lnum = u64::try_from(idx).unwrap_or(u64::MAX).saturating_add(1);
        let sym = enclosing_symbol(lines, lnum);
        out.push((lnum, raw.trim_end().to_string(), sym));
    }
    out
}

/// `true` when `haystack` contains `word` as a whole identifier — bounded on both
/// sides by a non-identifier character (or string edge). Empty `word` is `false`.
fn line_has_word(haystack: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let boundary_before = |s: &str| s.chars().last().is_none_or(|c| !is_ident(c));
    let boundary_after = |s: &str| s.chars().next().is_none_or(|c| !is_ident(c));
    let mut rest = haystack;
    while let Some(pos) = rest.find(word) {
        let before_ok = boundary_before(&rest[..pos]);
        let after_ok = boundary_after(&rest[pos + word.len()..]);
        if before_ok && after_ok {
            return true;
        }
        // Advance past this candidate by one char to keep searching for a
        // properly bounded occurrence later in the line.
        let advance = rest[pos..].chars().next().map_or(word.len(), char::len_utf8);
        rest = &rest[pos + advance..];
    }
    false
}

/// Read a file into trimmed lines, or `Err(io.read)` when it cannot be read.
fn read_source_lines(path: &str) -> Result<Vec<String>, ToolError> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .map_err(|e| ToolError::new(ErrClass::Io, "read", e.to_string()))
}

/// Execute an `outline:<path>` query against the real filesystem.
///
/// # Errors
/// `io.read` when the file cannot be read.
fn run_outline(path: &str) -> Result<Value, ToolError> {
    let lines = read_source_lines(path)?;
    let symbols: Vec<Value> = outline_symbols(&lines)
        .into_iter()
        .map(|(line, text)| json!({"line": line, "symbol": text}))
        .collect();
    Ok(json!({"outline": {"file": path, "symbols": symbols}}))
}

/// Execute a `refs:<symbol>` query by walking `args` like a normal `Grep`, but
/// matching the bare identifier and annotating each hit with its enclosing def.
///
/// # Errors
/// `regex.invalid` when the identifier cannot be escaped into a valid matcher.
fn run_refs(symbol: &str, args: &GrepArgs) -> Result<Value, ToolError> {
    let head_limit = args.head_limit.unwrap_or(250) as usize;
    let root = args.path.clone().unwrap_or_else(|| ".".to_string());
    // A word-boundary matcher gives a cheap pre-filter; `line_has_word` then
    // confirms identifier boundaries precisely before a hit is recorded.
    let pattern = format!(r"\b{}\b", regex_escape(symbol));
    let matcher = RegexMatcher::new(&pattern)
        .map_err(|e| ToolError::new(ErrClass::Regex, "invalid", e.to_string()))?;
    let mut walker = WalkBuilder::new(&root);
    walker.follow_links(false).standard_filters(true);
    if let Some(t) = &args.r#type {
        let mut tb = TypesBuilder::new();
        tb.add_defaults();
        tb.select(t);
        let types = tb
            .build()
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_type", e.to_string()))?;
        walker.types(types);
    }
    if let Some(g) = &args.glob {
        let mut ob = ignore::overrides::OverrideBuilder::new(&root);
        ob.add(g)
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob", e.to_string()))?;
        walker.overrides(
            ob.build()
                .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob_build", e.to_string()))?,
        );
    }
    let mut searcher = SearcherBuilder::new().build();
    let mut refs: Vec<Value> = Vec::new();
    'walk: for raw_entry in walker.build() {
        let Ok(entry) = raw_entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let mut hit = false;
        let search_outcome = searcher.search_path(
            &matcher,
            &path,
            UTF8(|_lnum, _line| {
                hit = true;
                Ok(false)
            }),
        );
        if search_outcome.is_err() || !hit {
            continue;
        }
        let path_display = path.display().to_string();
        let Ok(lines) = read_source_lines(&path_display) else {
            continue;
        };
        for (lnum, text, sym) in refs_in_source(&lines, symbol) {
            let mut obj = json!({"file": path_display, "line": lnum, "text": text});
            if let (Some(s), Some(map)) = (sym, obj.as_object_mut()) {
                map.insert("symbol".to_string(), Value::String(s));
            }
            refs.push(obj);
            if refs.len() >= head_limit {
                break 'walk;
            }
        }
    }
    Ok(json!({"refs": {"symbol": symbol, "matches": refs}}))
}

/// Escape regex metacharacters in `s` so it matches as a literal identifier.
///
/// Identifiers are typically `[A-Za-z0-9_]`, but a caller may pass something
/// containing metacharacters; escaping keeps the generated matcher safe.
fn regex_escape(s: &str) -> String {
    const META: &[char] =
        &['.', '^', '$', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\'];
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if META.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Elide `content`-mode matches that fall inside an already-exposed region.
///
/// Removes, in place, any match whose `(file, line)` lands inside one of the
/// supplied [`ExposureWindow`]s. A window matches when its `file` equals the
/// match's `file` string verbatim and the match's 1-based `line` is within the
/// inclusive range `start_line..=end_line`. Matches lacking a readable
/// `file`/`line` are never elided. Returns the number of matches removed; an
/// empty `exposure` removes nothing and returns `0`.
fn truncate_by_exposure(matches: &mut Vec<Value>, exposure: &[ExposureWindow]) -> u32 {
    if exposure.is_empty() {
        return 0;
    }
    let before = matches.len();
    matches.retain(|m| {
        let file = m.get("file").and_then(Value::as_str);
        let line = m.get("line").and_then(Value::as_u64);
        let (Some(file), Some(line)) = (file, line) else {
            return true;
        };
        let elide = exposure
            .iter()
            .any(|w| w.file == file && line >= w.start_line && line <= w.end_line);
        !elide
    });
    u32::try_from(before - matches.len()).unwrap_or(u32::MAX)
}

/// # Errors
/// `regex.invalid` on bad pattern, `io.*` on walk failures.
#[allow(clippy::module_name_repetitions, clippy::too_many_lines)]
pub fn grep_v2(args: GrepArgs) -> Result<Value, ToolError> {
    // Opt-in agentgrep relations/regions DSL: a `pattern` carrying the
    // `agentgrep:` sentinel selects a structural query (outline/refs) instead of
    // a raw regex search. Without the sentinel the path below is unchanged.
    if let Some(parsed) = parse_dsl(&args.pattern) {
        return match parsed? {
            DslQuery::Outline { path } => run_outline(&path),
            DslQuery::Refs { symbol } => run_refs(&symbol, &args),
        };
    }
    let matcher = RegexMatcher::new(&args.pattern)
        .map_err(|e| ToolError::new(ErrClass::Regex, "invalid", e.to_string()))?;
    let mode = args.output_mode.unwrap_or(OutputMode::FilesWithMatches);
    let head_limit = args.head_limit.unwrap_or(250) as usize;
    let root = args.path.unwrap_or_else(|| ".".to_string());

    let mut walker = WalkBuilder::new(&root);
    walker.follow_links(false).standard_filters(true);
    if let Some(t) = &args.r#type {
        let mut tb = TypesBuilder::new();
        tb.add_defaults();
        tb.select(t);
        let types = tb
            .build()
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_type", e.to_string()))?;
        walker.types(types);
    }
    if let Some(g) = &args.glob {
        let mut ob = ignore::overrides::OverrideBuilder::new(&root);
        ob.add(g)
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob", e.to_string()))?;
        walker.overrides(
            ob.build()
                .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob_build", e.to_string()))?,
        );
    }

    let mut searcher = SearcherBuilder::new()
        .before_context(args.before as usize)
        .after_context(args.after as usize)
        .multi_line(args.multiline)
        .build();

    // Code-structure-aware grep (jcode agentgrep): when `ORIGIN_AGENTGREP=1`,
    // each `content`-mode match is annotated with the nearest enclosing
    // definition (`fn`/`def`/`class`/…) so the model gets structure, not just a
    // raw line. Default-off ⇒ the result shape is byte-identical to before.
    let agentgrep = std::env::var("ORIGIN_AGENTGREP").as_deref() == Ok("1");

    let mut match_results: Vec<Value> = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut files: BTreeSet<String> = BTreeSet::new();
    'walk: for raw_entry in walker.build() {
        let Ok(entry) = raw_entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let path_display = path.display().to_string();
        let mut local_count: u64 = 0;
        let mut local_lines: Vec<(u64, String)> = Vec::new();
        let res = searcher.search_path(
            &matcher,
            &path,
            UTF8(|lnum, line| {
                local_count += 1;
                local_lines.push((lnum, line.trim_end_matches('\n').to_string()));
                Ok(true)
            }),
        );
        if res.is_err() {
            continue;
        }
        if local_count == 0 {
            continue;
        }
        files.insert(path_display.clone());
        counts.insert(path_display.clone(), local_count);
        if matches!(mode, OutputMode::Content) {
            // Read the whole file once (only when agentgrep is on) so each
            // match can resolve its enclosing definition by a cheap backward scan.
            let file_lines: Vec<String> = if agentgrep {
                std::fs::read_to_string(&path)
                    .map(|s| s.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            for (lnum, line) in local_lines {
                let mut obj = json!({"file": path_display, "line": lnum, "text": line});
                if agentgrep {
                    if let Some(sym) = enclosing_symbol(&file_lines, lnum) {
                        if let Some(map) = obj.as_object_mut() {
                            map.insert("symbol".to_string(), Value::String(sym));
                        }
                    }
                }
                match_results.push(obj);
                if match_results.len() >= head_limit {
                    break 'walk;
                }
            }
        }
    }

    // Exposure-aware truncation (opt-in): when `ORIGIN_AGENTGREP_TRUNCATE=1`,
    // drop `content`-mode matches whose (file, line) was already shown to the
    // model in an earlier turn, so re-running a search doesn't re-spend tokens
    // on already-seen regions. Default-off (gate unset) OR no `exposure` ⇒
    // `match_results` is untouched, so the result is byte-identical to before.
    // `files_with_matches` / `count` modes are never affected.
    if std::env::var("ORIGIN_AGENTGREP_TRUNCATE").as_deref() == Ok("1")
        && matches!(mode, OutputMode::Content)
    {
        if let Some(exposure) = args.exposure.as_ref() {
            // Elided matches are simply dropped; there is no metadata slot
            // in the result shape, and `{"matches": …}` reflects the
            // reduced set directly.
            let _elided = truncate_by_exposure(&mut match_results, exposure);
        }
    }

    let out = match mode {
        OutputMode::FilesWithMatches => {
            let arr: Vec<String> = files.into_iter().take(head_limit).collect();
            json!({"files": arr})
        }
        OutputMode::Content => json!({"matches": match_results}),
        OutputMode::Count => {
            let arr: Vec<Value> = counts
                .into_iter()
                .take(head_limit)
                .map(|(f, c)| json!({"file": f, "count": c}))
                .collect();
            json!({"counts": arr})
        }
    };
    Ok(out)
}

/// Definition leaders recognised by the agentgrep enclosing-symbol scan.
///
/// Deliberately language-agnostic and broad: the scan has only the file's lines
/// (no path/extension), so it keys off the *definition keyword* that opens a
/// declaration. The set spans the same ~18 language families `origin-repomap`'s
/// lightweight scanner handles — Rust, TS/JS, Python, Go, Java, C/C++/C#, Ruby,
/// PHP, Swift, Kotlin, Scala, Zig, Haskell, Lua, Elixir, shell — covering each
/// family's leaders (`fn`/`def`/`class`/`struct`/`impl`/`trait`/`func`/`fun`/
/// `object`/`interface`/`record`/`module`/`defmodule`/`defp`/`function`/`sub`/
/// `data`/`newtype`/`protocol`/`actor`/…). It is a heuristic, not tree-sitter.
///
/// Each entry is matched against the trimmed line via [`leader_matches`], which
/// enforces a word boundary so `fn ` matches `fn foo` but `fnord(` is rejected.
/// Multi-word leaders (`case class`, `data class`, `export default function`)
/// are matched literally and must precede their generic single-word forms so the
/// longest leader wins. The set is intentionally ordered longest-first per family.
const DEF_LEADERS: &[&str] = &[
    // --- multi-word / qualified leaders (must precede their single-word forms) ---
    "export default function",
    "export abstract class",
    "export function",
    "export class",
    "export const",
    "export interface",
    "export enum",
    "export type",
    "pub async fn",
    "pub const fn",
    "async fn",
    "const fn",
    "data class", // Kotlin
    "enum class", // Kotlin / C++
    "sealed class",
    "value class",
    "case class", // Scala
    "case object",
    "abstract class",
    "final class",
    "static func", // Swift
    "class func",
    "suspend fun", // Kotlin
    "local function", // Lua
    "public static",
    "private static",
    "protected static",
    // --- single-word leaders, by family ---
    "fn",        // Rust / Zig
    "func",      // Go / Swift
    "fun",       // Kotlin
    "function",  // TS/JS / PHP / Lua / shell `function name`
    "def",       // Python / Ruby / Scala / Elixir / Groovy
    "defp",      // Elixir private
    "defmodule", // Elixir
    "defmacro",  // Elixir
    "defprotocol",
    "defstruct",
    "defimpl",
    "sub",   // Perl
    "class", // many
    "struct",
    "enum",
    "union",
    "trait", // Rust / Scala / PHP
    "impl",  // Rust
    "interface",
    "protocol", // Swift
    "extension",
    "actor",     // Swift
    "object",    // Scala / Kotlin
    "module",    // Ruby / TS / Haskell-ish
    "namespace", // C++ / C# / TS
    "record",    // Java / C#
    "delegate",  // C#
    "type",      // Rust / TS / Go / Haskell
    "data",      // Haskell
    "newtype",   // Haskell
    "package",   // Java / Kotlin / Scala top-level
];

/// Visibility-led member declarations (Java / C# / C++ / Swift). Matched only
/// when followed by *whitespace* (never `public:` C++ access labels), so they
/// don't shadow the real enclosing type for a member-body match.
const VISIBILITY_LEADERS: &[&str] = &["public ", "private ", "protected ", "internal "];

/// Trailing keywords that look like a definition leader but are control-flow or
/// loop constructs — a line such as `if (cond) {` must NOT be mistaken for a
/// definition by the structural `name(...)`/`name() {` heuristics below.
const CALL_NOT_DEF_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "return", "catch", "do", "else", "case",
    "throw", "match", "with", "when", "elif", "until",
];

/// Resolve the nearest enclosing definition for a 1-based match line by scanning
/// backwards for the first line whose trimmed text looks like a definition.
///
/// Recognition is a cheap heuristic union of (a) a known keyword leader from
/// [`DEF_LEADERS`], (b) a Haskell-style top-level type signature `name :: …`,
/// and (c) a C-family / POSIX-shell header `… name(args) {` or `name() {`.
/// Returns the trimmed definition line (bounded length), or `None` when no
/// definition precedes the match.
fn enclosing_symbol(lines: &[String], match_line_1based: u64) -> Option<String> {
    if lines.is_empty() || match_line_1based == 0 {
        return None;
    }
    // Convert to a 0-based index, clamped to the file length.
    let match_idx = usize::try_from(match_line_1based).unwrap_or(usize::MAX);
    let start = match_idx.min(lines.len()).saturating_sub(1);
    for idx in (0..=start).rev() {
        let trimmed = lines[idx].trim_start();
        if is_definition_line(trimmed) {
            let sym: String = trimmed.trim_end().chars().take(120).collect();
            return Some(sym);
        }
    }
    None
}

/// `true` when `trimmed` (already left-trimmed) opens a definition under any of
/// the supported language heuristics.
fn is_definition_line(trimmed: &str) -> bool {
    if DEF_LEADERS.iter().any(|lead| leader_matches(trimmed, lead)) {
        return true;
    }
    if VISIBILITY_LEADERS.iter().any(|lead| trimmed.starts_with(lead)) {
        return true;
    }
    // Re-test the keyword leaders after peeling shared visibility/modifier words
    // (`pub`, `pub(crate)`, `static`, `final`, …) so `pub fn`, `static func`,
    // `open class`, etc. resolve to their definition keyword.
    let stripped = strip_leading_modifiers(trimmed);
    if stripped.len() != trimmed.len()
        && DEF_LEADERS.iter().any(|lead| leader_matches(stripped, lead))
    {
        return true;
    }
    haskell_signature(trimmed) || zig_type_binding(trimmed) || brace_header(trimmed)
}

/// Shared visibility / modifier keywords peeled from the front of a line so the
/// real definition keyword underneath can be recognized. Mirrors the modifier
/// set used by `origin-repomap`'s scanner; keywords that are *themselves*
/// leaders (`const`, `static`, `data`) are still peeled here because the leader
/// re-test runs on the original line first, so `const fn` / `static func` are
/// already covered before this peel applies.
const MODIFIERS: &[&str] = &[
    "pub", "public", "private", "protected", "internal", "export", "default",
    "final", "abstract", "sealed", "open", "override", "async", "static",
    "inline", "extern", "unsafe", "virtual", "partial", "suspend", "lateinit",
    "readonly", "declare", "implicit", "lazy", "annotation",
];

/// Peel leading [`MODIFIERS`] (and Rust `pub(crate)` scopes) from a trimmed line,
/// returning the remainder. Returns the input unchanged when nothing is peeled.
fn strip_leading_modifiers(line: &str) -> &str {
    let mut cur = line;
    loop {
        let trimmed = cur.trim_start();
        // Rust `pub(crate)` / `pub(super)` visibility scope.
        if let Some(rest) = trimmed.strip_prefix("pub(").and_then(|a| a.find(')').map(|c| &a[c + 1..])) {
            cur = rest;
            continue;
        }
        let next = MODIFIERS.iter().find_map(|m| {
            leader_matches(trimmed, m)
                .then(|| trimmed.strip_prefix(m))
                .flatten()
        });
        match next {
            Some(after) => cur = after,
            None => return trimmed,
        }
    }
}

/// Zig type-definition idiom: `const Name = struct {` / `= enum {` / `= union {`
/// / `= opaque {`. These bind a type to a `const`/`var` with no keyword leader of
/// their own, so they are matched structurally on the `= <container>` shape.
fn zig_type_binding(trimmed: &str) -> bool {
    let after_binder = trimmed
        .strip_prefix("const ")
        .or_else(|| trimmed.strip_prefix("var "))
        .or_else(|| trimmed.strip_prefix("pub const "))
        .or_else(|| trimmed.strip_prefix("pub var "));
    let Some(rest) = after_binder else {
        return false;
    };
    let Some(eq) = rest.find('=') else {
        return false;
    };
    let value = rest[eq + 1..].trim_start();
    ["struct", "enum", "union", "opaque", "packed struct"]
        .iter()
        .any(|kw| leader_matches(value, kw))
}

/// `true` when `trimmed` begins with `leader` as a whole word — i.e. the next
/// character is end-of-line, whitespace, or a non-identifier delimiter (`(`, `<`,
/// `{`, `:`). This stops `fn` from matching `fnord` or `class` from matching
/// `classify`, while still matching `fn(` / `class<T>`-style headers.
fn leader_matches(trimmed: &str, leader: &str) -> bool {
    let Some(after) = trimmed.strip_prefix(leader) else {
        return false;
    };
    after
        .chars()
        .next()
        .map_or(true, |c| !(c.is_ascii_alphanumeric() || c == '_'))
}

/// Haskell top-level type signature: `name :: Type`. Requires the ` :: `
/// separator to be *space-delimited* (the Haskell convention), which excludes
/// C++ scope-resolution (`std::vector`) and similar `Foo::bar` operators that
/// pack `::` tightly against identifiers. Each comma-separated binding to the
/// left must be a bare identifier, so `a, b :: Type` group signatures match too.
fn haskell_signature(trimmed: &str) -> bool {
    // Space-delimited `::` only; `Foo::bar` / `std::vector` are not signatures.
    let Some(idx) = trimmed.find(" :: ").or_else(|| {
        // Also accept a line ending exactly in ` ::` (type wraps to next line).
        trimmed.strip_suffix(" ::").map(str::len)
    }) else {
        return false;
    };
    let head = trimmed[..idx].trim();
    if head.is_empty() {
        return false;
    }
    head.split(',').all(|part| {
        let p = part.trim();
        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
    })
}

/// C-family / POSIX-shell definition header: a line shaped like `… name(args)`
/// whose tail opens a body (`{`), is a bare prototype (`;`), carries a C++
/// trailing specifier (`const`/`noexcept`/`override`), or is a POSIX `name() {`.
///
/// Guards against false positives: the identifier immediately before `(` must
/// not be a control-flow keyword; a return-type / modifier token must precede
/// the name (ruling out plain calls like `do_thing(x);`); the lead must not
/// contain `=` (an initializer is a call expression, not a decl); and the text
/// after the matching `)` must look like a body/prototype rather than a chained
/// call. This mirrors `origin-repomap`'s `method_signature` heuristic.
fn brace_header(trimmed: &str) -> bool {
    let Some(paren) = trimmed.find('(') else {
        return false;
    };
    let head = trimmed[..paren].trim_end();
    // The name is the trailing identifier run before `(`.
    let name_start = head
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_ascii_alphanumeric() || c == '_')
        .last()
        .map(|(i, _)| i);
    let Some(nstart) = name_start else {
        return false;
    };
    let name = &head[nstart..];
    if name.is_empty() || CALL_NOT_DEF_KEYWORDS.contains(&name) {
        return false;
    }
    let lead = head[..nstart].trim();
    let Some(tail) = paren_tail(&trimmed[paren..]) else {
        // Unbalanced parens ⇒ the argument list wraps onto the next line. Only a
        // real declaration (a lead return-type / modifier precedes the name) is
        // assumed to span lines like this; a bare wrapped call is rejected.
        return !lead.is_empty() && !lead.contains('=');
    };
    let after_paren = tail.trim_start();
    // POSIX shell `name()` / `name ()` with nothing before the name: the body
    // brace must follow the (balanced) empty argument list.
    if lead.is_empty() {
        return after_paren.starts_with('{');
    }
    // C-family: an `=` in the lead means an assignment/initializer (a call
    // expression), not a declaration.
    if lead.contains('=') {
        return false;
    }
    // A statement keyword immediately before the name (`return foo()`,
    // `throw err()`, `await go()`) means this is a call inside a statement, not a
    // declaration. Reject when the lead's trailing token is such a keyword.
    let last = lead.rsplit([' ', '\t']).next().unwrap_or(lead);
    if CALL_NOT_DEF_KEYWORDS.contains(&last) || last == "await" || last == "yield" {
        return false;
    }
    // The tail must open a body, terminate a prototype, or carry a trailing C++
    // specifier. A balanced `name()` with an *empty* tail is a bare statement
    // (e.g. `defer cleanup()`), which is a call — not a definition.
    let trailing_specifier = ["const", "noexcept", "override", "->"]
        .iter()
        .any(|kw| after_paren.starts_with(kw));
    after_paren.starts_with('{') || after_paren.starts_with(';') || trailing_specifier
}

/// Given a slice beginning at `(`, return `Some(tail)` — the slice immediately
/// after its balanced matching `)` — or `None` when the parens never balance
/// (the argument list spills onto later lines).
fn paren_tail(from_open: &str) -> Option<&str> {
    let mut depth: usize = 0;
    for (i, c) in from_open.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&from_open[i + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

crate::origin_tool! {
    name: "Grep",
    description: "Recursive regex search. Modes: files_with_matches (default), content, count. Supports glob/type filters and context lines. Opt-in agentgrep DSL via the pattern: `agentgrep:outline:<path>` lists a file's definitions/symbols; `agentgrep:refs:<symbol>` finds references to a bare identifier, each annotated with its enclosing definition. Without the `agentgrep:` prefix the pattern is an ordinary regex.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern":     { "type": "string" },
            "path":        { "type": "string" },
            "glob":        { "type": "string" },
            "type":        { "type": "string" },
            "output_mode": { "type": "string", "enum": ["files_with_matches", "content", "count"] },
            "head_limit":  { "type": "integer", "minimum": 1 },
            "before":      { "type": "integer", "minimum": 0 },
            "after":       { "type": "integer", "minimum": 0 },
            "line_numbers":{ "type": "boolean" },
            "multiline":   { "type": "boolean" }
        },
        "required": ["pattern"]
    }"#,
}

#[cfg(test)]
mod agentgrep_tests {
    use super::{enclosing_symbol, is_definition_line};

    fn lines(src: &str) -> Vec<String> {
        src.lines().map(str::to_string).collect()
    }

    // ---- relations/regions query DSL ---------------------------------------

    mod dsl {
        use super::lines;
        use crate::builtins::grep_tool::{
            outline_symbols, parse_dsl, refs_in_source, DslQuery,
        };

        #[test]
        fn parse_outline_query() {
            let q = parse_dsl("agentgrep:outline:src/lib.rs")
                .expect("recognized")
                .expect("ok");
            assert_eq!(q, DslQuery::Outline { path: "src/lib.rs".into() });
        }

        #[test]
        fn parse_refs_query() {
            let q = parse_dsl("agentgrep:refs:do_thing")
                .expect("recognized")
                .expect("ok");
            assert_eq!(q, DslQuery::Refs { symbol: "do_thing".into() });
        }

        #[test]
        fn parse_trims_operand_whitespace() {
            let q = parse_dsl("agentgrep:outline:  a/b.rs  ")
                .expect("recognized")
                .expect("ok");
            assert_eq!(q, DslQuery::Outline { path: "a/b.rs".into() });
        }

        #[test]
        fn non_dsl_pattern_is_none() {
            // No sentinel ⇒ ordinary regex ⇒ DSL not engaged (byte-identical path).
            assert!(parse_dsl("fn foo").is_none());
            assert!(parse_dsl("outline:src/lib.rs").is_none());
            assert!(parse_dsl(r"\bfoo\b").is_none());
        }

        #[test]
        fn unknown_verb_is_error() {
            let res = parse_dsl("agentgrep:wat:thing").expect("recognized");
            assert!(res.is_err());
        }

        #[test]
        fn empty_operand_is_error() {
            assert!(parse_dsl("agentgrep:outline:").expect("recognized").is_err());
            assert!(parse_dsl("agentgrep:refs:   ").expect("recognized").is_err());
        }

        #[test]
        fn missing_operand_separator_is_error() {
            assert!(parse_dsl("agentgrep:outline").expect("recognized").is_err());
        }

        #[test]
        fn outline_enumerates_multi_def_source() {
            // Line 1 (`let header`) is not a definition; the three defs that
            // follow (across two languages' leaders + a struct) are enumerated.
            let src = "let header = 1;\n    pub fn first() {}\n    fn second(x: u8) {\n        do_thing(x);\n    }\nstruct Point { x: u8 }\n";
            let syms = outline_symbols(&lines(src));
            let names: Vec<&str> = syms.iter().map(|(_, s)| s.as_str()).collect();
            assert_eq!(
                names,
                vec!["pub fn first() {}", "fn second(x: u8) {", "struct Point { x: u8 }"]
            );
            // Line numbers are 1-based and track the source.
            assert_eq!(syms[0], (2u64, "pub fn first() {}".to_string()));
        }

        #[test]
        fn outline_empty_source_is_empty() {
            assert!(outline_symbols(&[]).is_empty());
            assert!(outline_symbols(&lines("let x = 1;\nlet y = 2;\n")).is_empty());
        }

        #[test]
        fn refs_returns_lines_with_enclosing_symbol() {
            let src = "fn caller() {\n    let r = target();\n    target();\n}\nfn other() {\n    noop();\n}\n";
            let hits = refs_in_source(&lines(src), "target");
            assert_eq!(hits.len(), 2);
            // Both references resolve to their enclosing `fn caller`.
            for (_, _, sym) in &hits {
                assert_eq!(sym.as_deref(), Some("fn caller() {"));
            }
            assert_eq!(hits[0].0, 2);
            assert_eq!(hits[1].0, 3);
        }

        #[test]
        fn refs_word_boundary_rejects_substring() {
            // `target` must not match `targeting` or `mytarget`.
            let src = "fn f() {\n    let targeting = 1;\n    mytarget();\n    target();\n}\n";
            let hits = refs_in_source(&lines(src), "target");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].0, 4);
        }

        #[test]
        fn refs_empty_symbol_is_empty() {
            let src = "fn f() {}\n";
            assert!(refs_in_source(&lines(src), "").is_empty());
        }

        #[test]
        fn refs_without_enclosing_def_has_none_symbol() {
            let src = "let a = target();\nlet b = 2;\n";
            let hits = refs_in_source(&lines(src), "target");
            assert_eq!(hits.len(), 1);
            assert!(hits[0].2.is_none());
        }
    }

    #[test]
    fn finds_nearest_preceding_rust_fn() {
        let src = "mod a {\n    pub fn target() {\n        let x = 1;\n        do_thing(x);\n    }\n}\n";
        // The match for `do_thing` is on line 4 (1-based).
        let sym = enclosing_symbol(&lines(src), 4).expect("symbol");
        assert_eq!(sym, "pub fn target() {");
    }

    #[test]
    fn finds_python_def() {
        let src = "class C:\n    def method(self):\n        return compute()\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "def method(self):");
    }

    #[test]
    fn none_when_no_definition_precedes() {
        let src = "let x = 1;\nlet y = 2;\n";
        assert!(enclosing_symbol(&lines(src), 2).is_none());
    }

    #[test]
    fn out_of_range_line_is_safe() {
        let src = "fn a() {}\n";
        // Asking past the end of the file must not panic.
        assert!(enclosing_symbol(&lines(src), 999).is_some());
        assert!(enclosing_symbol(&[], 1).is_none());
        assert!(enclosing_symbol(&lines(src), 0).is_none());
    }

    // ---- broadened language coverage ---------------------------------------

    #[test]
    fn finds_go_method() {
        // A receiver method: the match inside the body resolves to the `func` header.
        let src = "func (r *Repo) Save(x int) error {\n    return persist(x)\n}\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "func (r *Repo) Save(x int) error {");
    }

    #[test]
    fn finds_ruby_def_inside_class() {
        // The nearest enclosing definition of the `compute` call is the `def`,
        // not the surrounding `class`.
        let src = "class Cat\n  def meow(loudness)\n    compute(loudness)\n  end\nend\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "def meow(loudness)");
    }

    #[test]
    fn finds_c_function() {
        // A C function header recognised purely by the `name(args) {` shape, with
        // a preceding return type — no keyword leader involved.
        let src = "int add(int a, int b) {\n    return a + b;\n}\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "int add(int a, int b) {");
    }

    #[test]
    fn c_bare_call_is_not_a_definition() {
        // A bare call with no return type / leader must not be treated as a def.
        let src = "    do_thing(x, y);\n    if (cond) {\n        foo();\n    }\n";
        assert!(enclosing_symbol(&lines(src), 3).is_none());
    }

    #[test]
    fn finds_elixir_def() {
        let src = "defmodule Calc do\n  def add(a, b) do\n    sum(a, b)\n  end\nend\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "def add(a, b) do");
    }

    #[test]
    fn finds_elixir_defp_private() {
        // `defp` (private) is a distinct leader from `def`.
        let src = "defmodule M do\n  defp secret() do\n    helper()\n  end\nend\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "defp secret() do");
    }

    #[test]
    fn finds_kotlin_fun() {
        let src = "class App {\n    fun run(arg: Int) {\n        process(arg)\n    }\n}\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "fun run(arg: Int) {");
    }

    #[test]
    fn finds_scala_case_class_and_object() {
        let src = "object App {\n  case class Item(id: Int)\n  val x = Item(use_it())\n}\n";
        // The nearest preceding definition to line 3 is the `case class`.
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "case class Item(id: Int)");
    }

    #[test]
    fn finds_swift_func() {
        let src = "struct Vec {\n    func dot(o: Vec) -> Int {\n        return compute(o)\n    }\n}\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "func dot(o: Vec) -> Int {");
    }

    #[test]
    fn finds_php_function() {
        let src = "<?php\nclass User {\n    public function name() {\n        return fetch();\n    }\n}\n";
        let sym = enclosing_symbol(&lines(src), 4).expect("symbol");
        assert_eq!(sym, "public function name() {");
    }

    #[test]
    fn finds_haskell_signature() {
        // A top-level type signature `name :: …` is treated as the definition.
        let src = "module M where\nfib :: Int -> Int\nfib n = go n\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "fib :: Int -> Int");
    }

    #[test]
    fn finds_haskell_data_decl() {
        let src = "data Tree = Leaf | Node Tree Tree\n  deriving (Show)\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "data Tree = Leaf | Node Tree Tree");
    }

    #[test]
    fn finds_zig_struct_binding() {
        // Zig type definition idiom `const Name = struct {`.
        let src = "const Point = struct {\n    x: u8,\n    y: u8,\n};\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "const Point = struct {");
    }

    #[test]
    fn finds_lua_function() {
        let src = "function M.attach(o)\n    register(o)\nend\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "function M.attach(o)");
    }

    #[test]
    fn finds_lua_local_function() {
        let src = "local function helper(x)\n    return tweak(x)\nend\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "local function helper(x)");
    }

    #[test]
    fn finds_posix_shell_function() {
        // POSIX `name() {` form, recognised structurally with no leader keyword.
        let src = "build() {\n    run_make\n}\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "build() {");
    }

    #[test]
    fn finds_bash_function_keyword() {
        let src = "function deploy {\n    push_it\n}\n";
        let sym = enclosing_symbol(&lines(src), 2).expect("symbol");
        assert_eq!(sym, "function deploy {");
    }

    #[test]
    fn finds_csharp_record_and_interface() {
        let src = "namespace App {\n    public record Money(decimal Amount);\n    var m = use(Money);\n}\n";
        let sym = enclosing_symbol(&lines(src), 3).expect("symbol");
        assert_eq!(sym, "public record Money(decimal Amount);");
    }

    #[test]
    fn leader_word_boundary_rejects_lookalikes() {
        // `fnord`/`classify`/`defer` must not be mistaken for `fn`/`class`/`def`.
        assert!(!is_definition_line("fnord = 1"));
        assert!(!is_definition_line("classify(x)"));
        assert!(!is_definition_line("defer cleanup()"));
        // …but the real keywords still match.
        assert!(is_definition_line("fn foo() {}"));
        assert!(is_definition_line("class Foo {"));
        assert!(is_definition_line("def foo():"));
    }
}

#[cfg(test)]
mod truncation_tests {
    use super::{truncate_by_exposure, ExposureWindow, GrepArgs};
    use serde_json::json;

    #[test]
    fn elides_inside_window_and_is_file_selective() {
        let mut matches = vec![
            json!({ "file": "src/a.rs", "line": 5, "text": "hit a5" }),
            json!({ "file": "src/a.rs", "line": 50, "text": "hit a50" }),
            json!({ "file": "src/b.rs", "line": 5, "text": "hit b5" }),
        ];
        let exposure = vec![ExposureWindow {
            file: "src/a.rs".to_string(),
            start_line: 1,
            end_line: 10,
        }];
        let elided = truncate_by_exposure(&mut matches, &exposure);
        // a5 is inside the window on the right file -> elided.
        // a50 is outside the window -> kept.
        // b5 is in a different file -> kept (multi-file selectivity).
        assert_eq!(elided, 1);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["file"], "src/a.rs");
        assert_eq!(matches[0]["line"], 50);
        assert_eq!(matches[1]["file"], "src/b.rs");
        assert_eq!(matches[1]["line"], 5);
    }

    #[test]
    fn boundaries_are_inclusive() {
        let mut matches = vec![
            json!({ "file": "src/a.rs", "line": 9, "text": "before" }),
            json!({ "file": "src/a.rs", "line": 10, "text": "start boundary" }),
            json!({ "file": "src/a.rs", "line": 20, "text": "end boundary" }),
            json!({ "file": "src/a.rs", "line": 21, "text": "after" }),
        ];
        let exposure = vec![ExposureWindow {
            file: "src/a.rs".to_string(),
            start_line: 10,
            end_line: 20,
        }];
        let elided = truncate_by_exposure(&mut matches, &exposure);
        // Lines 10 and 20 (both boundaries, inclusive) are elided; the lines
        // one outside the window on each side (9 and 21) are kept.
        assert_eq!(elided, 2);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["line"], 9);
        assert_eq!(matches[1]["line"], 21);
    }

    #[test]
    fn empty_exposure_is_noop() {
        let mut matches = vec![
            json!({ "file": "src/a.rs", "line": 5, "text": "hit" }),
            json!({ "file": "src/b.rs", "line": 7, "text": "hit" }),
        ];
        let exposure: Vec<ExposureWindow> = Vec::new();
        let elided = truncate_by_exposure(&mut matches, &exposure);
        assert_eq!(elided, 0);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["line"], 5);
        assert_eq!(matches[1]["line"], 7);
    }

    #[test]
    fn default_grep_args_has_no_exposure() {
        // The exposure field is absent (`None`) by default, so a `GrepArgs`
        // built without it — the daemon-free path — keeps the legacy behavior.
        let args = GrepArgs::default();
        assert_eq!(args.exposure, None);
    }
}
