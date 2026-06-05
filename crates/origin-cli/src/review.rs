// SPDX-License-Identifier: Apache-2.0
//! `origin review [--strictness strict|balanced|lenient]` — confidence-scored,
//! multi-dimension review of the working-tree diff vs `HEAD`.
//!
//! This is the local, in-session half of claude-code's multi-agent
//! confidence-scored review. The working-tree patch is obtained with
//! `git diff HEAD` (shelled out via [`std::process::Command`], the same approach
//! [`crate::scout`] / [`crate::vcs`] use — no new dependency), passed through a
//! fully local **static** heuristic analyzer that emits per-dimension
//! [`origin_review::Finding`]s, then merged + thresholded by the pure
//! [`origin_review`] decision layer ([`origin_review::dedup`] +
//! [`origin_review::filter`]) under the chosen [`origin_review::Strictness`].
//!
//! `origin-review` is a pure decision layer (dedup / strictness filter /
//! adversarial vote): it ranks findings agents produce, but does not itself
//! parse diffs or call a model. The deeper, semantic review dimensions (an LLM
//! bug-hunter / security agent) run in the daemon/swarm review bot; this command
//! ships the static, no-LLM dimensions that work entirely offline.

use std::process::Command;

use anyhow::Result;
use origin_review::{dedup, filter, Dimension, Finding, Strictness};

/// Parse the `--strictness` flag into a [`Strictness`].
///
/// Accepts `strict`, `balanced`, and `lenient` (case-insensitive). Any other
/// value is rejected with a friendly error rather than silently defaulting, so a
/// typo never quietly changes how aggressively findings are surfaced.
///
/// # Errors
/// Returns an error describing the accepted values when `raw` is unrecognized.
pub fn parse_strictness(raw: &str) -> Result<Strictness> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "strict" => Ok(Strictness::Strict),
        "balanced" => Ok(Strictness::Balanced),
        "lenient" => Ok(Strictness::Lenient),
        other => Err(anyhow::anyhow!(
            "unknown strictness {other:?}; expected one of strict|balanced|lenient"
        )),
    }
}

/// Run `origin review`: diff the working tree, analyze it, and print findings.
///
/// With `llm = false` (default) the analysis is the local static heuristic set
/// ([`analyze_diff`]). With `llm = true` it runs a model review pass through the
/// daemon ([`llm_review_diff`]) — the LLM bug/security/perf dimensions, now
/// available locally and not just in the CI bot — then both paths flow through
/// the same pure dedup + strictness layer ([`render`]).
///
/// On a non-git directory or an empty diff a friendly message is printed and the
/// command succeeds (no panic).
///
/// # Errors
/// Returns when `--strictness` is invalid, when `git` cannot be spawned, or
/// (in `--llm` mode) when the daemon turn fails.
pub async fn run(strictness: &str, llm: bool) -> Result<()> {
    let level = parse_strictness(strictness)?;
    match working_tree_diff()? {
        DiffOutcome::NotARepo => {
            println!("not a git repository — run `origin review` from inside a repo.");
            Ok(())
        }
        DiffOutcome::Empty => {
            println!("no working-tree changes vs HEAD — nothing to review.");
            Ok(())
        }
        DiffOutcome::Patch(patch) => {
            let findings = if llm {
                llm_review_diff(&patch).await?
            } else {
                analyze_diff(&patch)
            };
            print!("{}", render(&findings, level));
            Ok(())
        }
    }
}

/// Run an LLM review pass over `patch` through the daemon and parse its findings.
///
/// Builds a structured review prompt, runs one read-only daemon turn via
/// [`crate::headless::one_shot_text`], and parses the model's JSON findings into
/// [`Finding`]s. Parsing is lenient: malformed output yields no findings rather
/// than an error, so a stray model response degrades to an empty (clean) review.
///
/// # Errors
/// Returns when the daemon turn itself fails (unreachable daemon, etc.).
async fn llm_review_diff(patch: &str) -> Result<Vec<Finding>> {
    let model = std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-8".to_string());
    let reply = crate::headless::one_shot_text(&model, build_review_prompt(patch)).await?;
    Ok(parse_llm_findings(&reply))
}

/// The review instruction + structured-output contract sent to the model.
fn build_review_prompt(patch: &str) -> String {
    format!(
        "You are a meticulous senior code reviewer. Review the following unified diff \
         (working tree vs HEAD). Focus on real defects, not nits.\n\n\
         Output ONLY a JSON array (no prose, no markdown fences). Each element:\n\
         {{\"dimension\":\"bug|security|performance|test|style\",\"file\":\"<path>\",\
         \"line\":<new-file line number>,\"title\":\"<short>\",\"detail\":\"<why + how to fix>\",\
         \"confidence\":<0.0-1.0>}}\n\
         Use a high confidence only when you are sure. Return [] if nothing is worth flagging.\n\n\
         DIFF:\n{patch}"
    )
}

/// One finding as emitted by the model (lenient: missing fields default).
#[derive(serde::Deserialize)]
struct RawFinding {
    #[serde(default)]
    dimension: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    confidence: f32,
}

/// Map a model dimension string onto a [`Dimension`]; unknown labels are dropped.
fn parse_dimension(raw: &str) -> Option<Dimension> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "bug" | "correctness" => Some(Dimension::Bug),
        "security" | "sec" => Some(Dimension::Security),
        "performance" | "perf" => Some(Dimension::Performance),
        "test" | "tests" | "testing" => Some(Dimension::Test),
        "style" | "nit" | "docs" => Some(Dimension::Style),
        _ => None,
    }
}

/// Parse the model's reply into [`Finding`]s.
///
/// Tolerates surrounding prose / code fences by extracting the first top-level
/// `[ ... ]` JSON array; a reply with no parseable array (or only unknown
/// dimensions) yields an empty list.
fn parse_llm_findings(reply: &str) -> Vec<Finding> {
    let Some(json) = extract_json_array(reply) else {
        return Vec::new();
    };
    let Ok(raws) = serde_json::from_str::<Vec<RawFinding>>(json) else {
        return Vec::new();
    };
    raws.into_iter()
        .filter(|r| !r.file.is_empty() && !r.title.is_empty())
        .filter_map(|r| {
            parse_dimension(&r.dimension)
                .map(|d| Finding::new(d, &r.file, r.line, &r.title, &r.detail, r.confidence))
        })
        .collect()
}

/// Slice out the first balanced top-level JSON array from `s`.
///
/// Scans from the first `[` tracking bracket depth (ignoring brackets inside
/// JSON strings), so the parser tolerates a model that wraps the array in prose
/// or a fenced code block. Returns `None` when there is no balanced array.
fn extract_json_array(s: &str) -> Option<&str> {
    let start = s.find('[')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, ch) in s[start..].char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The result of trying to read the working-tree patch.
enum DiffOutcome {
    /// `git` reported this is not a repository.
    NotARepo,
    /// There is a repository but no changes vs `HEAD`.
    Empty,
    /// A non-empty unified diff.
    Patch(String),
}

/// Obtain the working-tree unified diff vs `HEAD` by shelling out to `git`.
///
/// Uses a zero-context, no-color invocation so parsing stays stable regardless
/// of the user's git config. A failure that mentions "not a git repository" maps
/// to [`DiffOutcome::NotARepo`]; other git failures bubble up as errors.
///
/// # Errors
/// Returns when the `git` binary cannot be spawned, or when `git diff` fails for
/// a reason other than the directory not being a repository.
fn working_tree_diff() -> Result<DiffOutcome> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.quotepath=false",
            "diff",
            "--no-color",
            "--unified=0",
            "HEAD",
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("spawning git: {e}"))?;

    if output.status.success() {
        let patch = String::from_utf8_lossy(&output.stdout).into_owned();
        if patch.trim().is_empty() {
            return Ok(DiffOutcome::Empty);
        }
        return Ok(DiffOutcome::Patch(patch));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.to_ascii_lowercase().contains("not a git repository") {
        return Ok(DiffOutcome::NotARepo);
    }
    Err(anyhow::anyhow!("git diff failed: {}", stderr.trim()))
}

/// One added line in the diff, tagged with the file and the new-file line number.
struct AddedLine<'a> {
    file: String,
    line: u32,
    text: &'a str,
}

/// Statically analyze a unified diff into confidence-scored [`Finding`]s.
///
/// This is the local, no-LLM dimension set: it inspects only **added** lines
/// (those prefixed `+` in the patch) and applies deterministic heuristics across
/// the [`Dimension`] axes. Each rule's confidence reflects how reliable the
/// pattern is as a true defect (e.g. an obvious hardcoded secret scores higher
/// than a stylistic nit). The caller dedups + thresholds the result.
#[must_use]
pub fn analyze_diff(patch: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for added in added_lines(patch) {
        scan_line(&added, &mut findings);
    }
    findings
}

/// Walk a unified diff and yield every added line with its new-file line number.
///
/// Tracks the current target file from `+++ b/<path>` headers and the running
/// new-file line counter from `@@ -a,b +c,d @@` hunk headers. Removed (`-`) and
/// context lines do not advance an added-line position. Lines inside a hunk that
/// start with `+` (but are not the `+++` header) are emitted.
fn added_lines(patch: &str) -> Vec<AddedLine<'_>> {
    let mut out = Vec::new();
    let mut file = String::new();
    let mut new_line: u32 = 0;
    for raw in patch.lines() {
        if let Some(path) = raw.strip_prefix("+++ ") {
            file = normalize_path(path);
        } else if raw.starts_with("@@ ") {
            new_line = parse_hunk_new_start(raw);
        } else if let Some(content) = added_payload(raw) {
            out.push(AddedLine {
                file: file.clone(),
                line: new_line,
                text: content,
            });
            new_line = new_line.saturating_add(1);
        } else if !raw.starts_with('-') && !raw.starts_with("\\ ") {
            // A context line advances the new-file cursor; `-` removals and the
            // "\ No newline at end of file" marker do not.
            new_line = new_line.saturating_add(1);
        }
    }
    out
}

/// Returns the content of an added line, or `None` if `raw` is not one.
///
/// Distinguishes a real `+` addition from the `+++` file header (which also
/// starts with `+` but is metadata, not a diff body line).
fn added_payload(raw: &str) -> Option<&str> {
    if raw.starts_with("+++") {
        return None;
    }
    raw.strip_prefix('+')
}

/// Normalize a `+++`/`---` header path into a repo-relative file name.
///
/// Strips a leading `b/` (git's destination prefix) and a trailing tab-delimited
/// timestamp if present. `/dev/null` becomes an empty string.
fn normalize_path(header: &str) -> String {
    let path = header.split('\t').next().unwrap_or(header).trim();
    if path == "/dev/null" {
        return String::new();
    }
    path.strip_prefix("b/").unwrap_or(path).to_string()
}

/// Parse the new-file starting line from a `@@ -a,b +c,d @@` hunk header.
///
/// Returns the `c` in `+c,d` (or `+c`). Defaults to `0` if the header is
/// malformed, so analysis degrades gracefully rather than panicking.
fn parse_hunk_new_start(header: &str) -> u32 {
    header
        .split('+')
        .nth(1)
        .and_then(|seg| seg.split([',', ' ']).next())
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(0)
}

/// Apply every heuristic rule to a single added line, pushing any matches.
fn scan_line(added: &AddedLine<'_>, out: &mut Vec<Finding>) {
    let body = added.text;
    let trimmed = body.trim_start();
    for rule in RULES {
        if (rule.matches)(body, trimmed) {
            out.push(Finding::new(
                rule.dimension,
                &added.file,
                added.line,
                rule.title,
                rule.detail,
                rule.confidence,
            ));
        }
    }
}

/// A single static heuristic: a predicate over an added line plus the metadata
/// of the [`Finding`] it produces on a match.
struct Rule {
    dimension: Dimension,
    title: &'static str,
    detail: &'static str,
    confidence: f32,
    /// Predicate over `(raw_added_line, leading_trimmed_line)`.
    matches: fn(&str, &str) -> bool,
}

/// The static rule table. Each rule is deterministic and offline; the *mechanism*
/// (per-dimension heuristics fed into confidence dedup) is the contribution.
const RULES: &[Rule] = &[
    Rule {
        dimension: Dimension::Security,
        title: "possible hardcoded secret",
        detail: "an added line assigns a credential-like name to a string literal",
        confidence: 0.85,
        matches: looks_like_secret,
    },
    Rule {
        dimension: Dimension::Security,
        title: "added `unsafe` block",
        detail: "review the safety invariants this `unsafe` relies on",
        confidence: 0.7,
        matches: |_raw, t| t.starts_with("unsafe ") || t == "unsafe {" || t.contains(" unsafe "),
    },
    Rule {
        dimension: Dimension::Bug,
        title: "unwrap/expect can panic",
        detail: "`.unwrap()` / `.expect(...)` aborts on `None`/`Err`; prefer `?` or a checked branch",
        confidence: 0.6,
        matches: |raw, _t| raw.contains(".unwrap()") || raw.contains(".expect("),
    },
    Rule {
        dimension: Dimension::Bug,
        title: "left-in debug print",
        detail: "a debug print / log was added; confirm it is intentional",
        confidence: 0.55,
        matches: |raw, _t| {
            raw.contains("dbg!(")
                || raw.contains("println!(")
                || raw.contains("console.log(")
        },
    },
    Rule {
        dimension: Dimension::Test,
        title: "TODO/FIXME left in code",
        detail: "an unresolved marker was introduced",
        confidence: 0.45,
        matches: |raw, _t| {
            raw.contains("TODO") || raw.contains("FIXME") || raw.contains("XXX")
        },
    },
    Rule {
        dimension: Dimension::Performance,
        title: "added `.clone()`",
        detail: "an allocation via `.clone()` was introduced; check if a borrow suffices",
        confidence: 0.3,
        matches: |raw, _t| raw.contains(".clone()"),
    },
    Rule {
        dimension: Dimension::Simplification,
        title: "very long added line",
        detail: "an added line exceeds 120 columns; consider splitting it",
        confidence: 0.25,
        matches: |raw, _t| raw.chars().count() > 120,
    },
    Rule {
        dimension: Dimension::Style,
        title: "trailing whitespace",
        detail: "an added line ends in whitespace",
        confidence: 0.2,
        matches: |raw, _t| !raw.is_empty() && raw != raw.trim_end(),
    },
];

/// Heuristic for [`Dimension::Security`]: a credential-like assignment to a
/// non-trivial string literal.
///
/// Matches when the line contains a secret-ish identifier next to `=`/`:` and a
/// quote with several characters between the quotes — empty or placeholder
/// strings (`""`) are ignored to keep false positives down.
fn looks_like_secret(raw: &str, _trimmed: &str) -> bool {
    const NAMES: &[&str] = &[
        "password", "passwd", "secret", "api_key", "apikey", "api-key",
        "access_key", "token", "private_key",
    ];
    let lower = raw.to_ascii_lowercase();
    let names_hit = NAMES.iter().any(|n| lower.contains(n));
    if !names_hit {
        return false;
    }
    let has_assign = raw.contains('=') || raw.contains(':');
    has_assign && quoted_literal_len(raw) >= 4
}

/// Length of the longest run of characters between the first pair of matching
/// single or double quotes on the line, or `0` if there is no such pair.
fn quoted_literal_len(raw: &str) -> usize {
    for quote in ['"', '\''] {
        // `inner` is the text between the first opening quote and the next quote
        // of the same kind; only counts when the line has a closing quote too.
        if raw.matches(quote).count() < 2 {
            continue;
        }
        if let Some(inner) = raw.split(quote).nth(1) {
            return inner.chars().count();
        }
    }
    0
}

/// Render a confidence-scored report for `findings` under `level`.
///
/// The findings are deduped (highest confidence per `file:line:title` wins) and
/// then thresholded + sorted by [`origin_review::filter`]. The output is plain,
/// deterministic text: a header line, then one `file:line  [dimension]  cN%
/// title` line per surfaced finding, then a summary footer. Returning a `String`
/// (rather than printing directly) keeps the renderer unit-testable.
#[must_use]
pub fn render(findings: &[Finding], level: Strictness) -> String {
    let merged = dedup(findings.to_vec());
    let analyzed = merged.len();
    let surfaced = filter(&merged, level);

    let mut out = String::new();
    out.push_str(&format!(
        "origin review — strictness {} (confidence >= {:.0}%)\n",
        strictness_name(level),
        level.threshold() * 100.0
    ));

    if surfaced.is_empty() {
        out.push_str(&format!(
            "no findings at this strictness ({analyzed} candidate(s) analyzed).\n"
        ));
        return out;
    }

    for f in &surfaced {
        let location = if f.file.is_empty() {
            "?".to_string()
        } else {
            format!("{}:{}", f.file, f.line)
        };
        out.push_str(&format!(
            "{location}  [{}]  c{:.0}%  {}\n",
            dimension_name(f.dimension),
            f.confidence * 100.0,
            f.title
        ));
        if !f.detail.is_empty() {
            out.push_str(&format!("    {}\n", f.detail));
        }
    }

    out.push_str(&format!(
        "\n{} finding(s) surfaced of {analyzed} candidate(s).\n",
        surfaced.len()
    ));
    out
}

/// Lowercase label for a [`Strictness`], for the report header.
const fn strictness_name(level: Strictness) -> &'static str {
    match level {
        Strictness::Strict => "strict",
        Strictness::Balanced => "balanced",
        Strictness::Lenient => "lenient",
    }
}

/// Short lowercase label for a [`Dimension`], for the per-finding line.
const fn dimension_name(d: Dimension) -> &'static str {
    match d {
        Dimension::Bug => "bug",
        Dimension::Security => "security",
        Dimension::TypeDesign => "type-design",
        Dimension::Test => "test",
        Dimension::Simplification => "simplify",
        Dimension::Performance => "perf",
        Dimension::Style => "style",
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_strictness_case_insensitively() {
        assert_eq!(parse_strictness("strict").unwrap(), Strictness::Strict);
        assert_eq!(parse_strictness("BALANCED").unwrap(), Strictness::Balanced);
        assert_eq!(parse_strictness("  lenient ").unwrap(), Strictness::Lenient);
    }

    #[test]
    fn extract_json_array_ignores_surrounding_prose_and_fences() {
        let s = "Here are the findings:\n```json\n[{\"a\":1}, {\"b\":[2,3]}]\n```\nDone.";
        assert_eq!(extract_json_array(s), Some("[{\"a\":1}, {\"b\":[2,3]}]"));
        // A bracket inside a string must not end the array early.
        let s2 = "[{\"title\":\"has ] bracket\"}]";
        assert_eq!(extract_json_array(s2), Some(s2));
        assert_eq!(extract_json_array("no array here"), None);
    }

    #[test]
    fn parse_dimension_maps_labels_and_drops_unknown() {
        assert_eq!(parse_dimension("Bug"), Some(Dimension::Bug));
        assert_eq!(parse_dimension("perf"), Some(Dimension::Performance));
        assert_eq!(parse_dimension("security"), Some(Dimension::Security));
        assert_eq!(parse_dimension("tests"), Some(Dimension::Test));
        assert_eq!(parse_dimension("wat"), None);
    }

    #[test]
    fn parse_llm_findings_parses_array_and_skips_invalid() {
        let reply = "```json\n[\
            {\"dimension\":\"bug\",\"file\":\"a.rs\",\"line\":12,\"title\":\"off-by-one\",\"detail\":\"d\",\"confidence\":0.9},\
            {\"dimension\":\"unknown\",\"file\":\"b.rs\",\"line\":1,\"title\":\"x\",\"detail\":\"\",\"confidence\":0.5},\
            {\"dimension\":\"security\",\"file\":\"\",\"line\":1,\"title\":\"no file\",\"detail\":\"\",\"confidence\":0.5}\
        ]\n```";
        let findings = parse_llm_findings(reply);
        // Only the well-formed, known-dimension, file+title-bearing finding survives.
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].dimension, Dimension::Bug);
        assert_eq!(findings[0].file, "a.rs");
        assert_eq!(findings[0].line, 12);
        assert!((findings[0].confidence - 0.9).abs() < 1e-6);
    }

    #[test]
    fn parse_llm_findings_empty_on_garbage() {
        assert!(parse_llm_findings("the model refused").is_empty());
        assert!(parse_llm_findings("[not valid json").is_empty());
        assert!(parse_llm_findings("[]").is_empty());
    }

    #[test]
    fn rejects_unknown_strictness() {
        let err = parse_strictness("paranoid").unwrap_err().to_string();
        assert!(err.contains("paranoid"), "error names the bad value: {err}");
        assert!(err.contains("strict|balanced|lenient"));
    }

    #[test]
    fn analyzes_added_lines_with_correct_line_numbers() {
        // A synthetic unified diff: one hunk starting at new-file line 42 adding
        // an unwrap and a debug print, plus a context line in between.
        let patch = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -41,0 +42,3 @@ fn demo() {
+    let v = thing.unwrap();
+    let _ = v;
+    println!(\"debug {}\", v);
";
        let findings = analyze_diff(patch);
        // unwrap on line 42, println on line 44.
        let unwrap = findings
            .iter()
            .find(|f| f.title == "unwrap/expect can panic")
            .expect("unwrap finding present");
        assert_eq!(unwrap.file, "src/lib.rs");
        assert_eq!(unwrap.line, 42);
        assert_eq!(unwrap.dimension, Dimension::Bug);

        let print = findings
            .iter()
            .find(|f| f.title == "left-in debug print")
            .expect("debug-print finding present");
        assert_eq!(print.line, 44);
    }

    #[test]
    fn detects_hardcoded_secret_but_ignores_empty_literal() {
        let positive = "+    let api_key = \"sk-livesecret123\";";
        let hits = analyze_diff(&format!("+++ b/cfg.rs\n@@ -0,0 +1,1 @@\n{positive}"));
        assert!(
            hits.iter().any(|f| f.dimension == Dimension::Security
                && f.title == "possible hardcoded secret"),
            "expected a security finding, got {hits:?}"
        );

        let empty = "+++ b/cfg.rs\n@@ -0,0 +1,1 @@\n+    let password = \"\";";
        assert!(
            !analyze_diff(empty)
                .iter()
                .any(|f| f.dimension == Dimension::Security),
            "empty secret literal must not trigger"
        );
    }

    #[test]
    fn render_filters_by_strictness_and_dedups() {
        // Two identical high-confidence findings (collapse to one) plus a
        // low-confidence style nit.
        let findings = vec![
            Finding::new(Dimension::Security, "a.rs", 3, "hardcoded secret", "leak", 0.85),
            Finding::new(Dimension::Security, "a.rs", 3, "hardcoded secret", "leak", 0.6),
            Finding::new(Dimension::Style, "a.rs", 9, "trailing whitespace", "ws", 0.2),
        ];

        // Strict (0.8): only the deduped secret survives.
        let strict = render(&findings, Strictness::Strict);
        assert!(strict.contains("strictness strict"));
        assert!(strict.contains("a.rs:3"));
        assert!(strict.contains("[security]"));
        assert!(strict.contains("c85%"), "keeps the higher confidence: {strict}");
        assert!(!strict.contains("trailing whitespace"));
        assert!(strict.contains("1 finding(s) surfaced of 2 candidate(s)."));

        // Lenient (0.2): both deduped findings surface, secret ranked first.
        let lenient = render(&findings, Strictness::Lenient);
        let sec = lenient.find("[security]").expect("security line");
        let sty = lenient.find("[style]").expect("style line");
        assert!(sec < sty, "higher confidence sorts first:\n{lenient}");
        assert!(lenient.contains("2 finding(s) surfaced of 2 candidate(s)."));
    }

    #[test]
    fn render_reports_clean_tree() {
        let out = render(&[], Strictness::Balanced);
        assert!(out.contains("no findings at this strictness"));
        assert!(out.contains("0 candidate(s)"));
    }

    #[test]
    fn long_line_and_clone_are_low_confidence() {
        let long = format!("+    let s = \"{}\";", "x".repeat(140));
        let patch = format!("+++ b/a.rs\n@@ -0,0 +1,2 @@\n{long}\n+    let c = v.clone();");
        let findings = analyze_diff(&patch);
        // Both exist but are dropped under balanced strictness.
        assert!(findings.iter().any(|f| f.dimension == Dimension::Simplification));
        assert!(findings.iter().any(|f| f.dimension == Dimension::Performance));
        let balanced = render(&findings, Strictness::Balanced);
        assert!(balanced.contains("no findings at this strictness"), "{balanced}");
    }
}
