// SPDX-License-Identifier: Apache-2.0
//! Confidence-scored review aggregation and issue auto-triage.
//!
//! `origin` runs several review agents (bug hunter, security, type-design,
//! simplifier) plus adversarial verifiers over a diff. Each agent emits raw
//! findings; this crate is the pure decision layer that merges, scores, and
//! classifies them — no I/O, no async, no model calls (claude-code review
//! confidence scoring, kilocode's strict/balanced/lenient Review Agent with
//! auto-triage, opencode review).
//!
//! The two jobs are: (1) turn a pile of overlapping [`Finding`]s into a clean,
//! confidence-ranked list under a chosen [`Strictness`], with adversarial
//! [`vote`]ing to gate low-trust claims; and (2) [`triage`] freeform issue text
//! into an [`IssueLabel`] using a keyword classifier plus a token-Jaccard
//! [`similarity`] helper for duplicate detection.
//!
//! ```
//! use origin_review::{dedup, filter, Finding, Dimension, Strictness};
//!
//! let raw = vec![
//!     Finding::new(Dimension::Bug, "a.rs", 10, "off-by-one", "", 0.9),
//!     Finding::new(Dimension::Bug, "a.rs", 10, "off-by-one", "", 0.6),
//!     Finding::new(Dimension::Style, "a.rs", 4, "nit", "", 0.2),
//! ];
//! let merged = dedup(raw);
//! assert_eq!(merged.len(), 2); // the two off-by-one findings collapsed
//! let surfaced = filter(&merged, Strictness::Balanced);
//! assert_eq!(surfaced.len(), 1); // the 0.2 nit dropped below threshold
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The review lens a [`Finding`] was produced under.
///
/// Mirrors the specialised review agents `origin` dispatches; serialized as the
/// lowercase variant name so daemon JSON stays stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    /// A correctness defect that can produce wrong behaviour.
    Bug,
    /// A vulnerability or unsafe handling of untrusted input/secrets.
    Security,
    /// A type-design / API-shape concern (illegal states representable, etc.).
    TypeDesign,
    /// Missing, weak, or incorrect test coverage.
    Test,
    /// An opportunity to simplify or reduce code.
    Simplification,
    /// A performance regression or inefficiency.
    Performance,
    /// A purely stylistic / cosmetic nit.
    Style,
}

/// A single review observation emitted by one agent.
///
/// `confidence` is the agent's self-reported trust in the finding, clamped to
/// `[0.0, 1.0]` by [`Finding::new`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The review lens that produced this finding.
    pub dimension: Dimension,
    /// Source file the finding refers to.
    pub file: String,
    /// 1-based line number within `file`.
    pub line: u32,
    /// Short one-line summary (the merge key, with `file`/`line`).
    pub title: String,
    /// Longer human-readable explanation; may be empty.
    pub detail: String,
    /// Self-reported confidence in `[0.0, 1.0]`.
    pub confidence: f32,
}

impl Finding {
    /// Construct a finding, clamping `confidence` into `[0.0, 1.0]`.
    ///
    /// `NaN` confidence is treated as `0.0` so it sorts and thresholds safely.
    #[must_use]
    pub fn new(
        dimension: Dimension,
        file: &str,
        line: u32,
        title: &str,
        detail: &str,
        confidence: f32,
    ) -> Self {
        Self {
            dimension,
            file: file.to_string(),
            line,
            title: title.to_string(),
            detail: detail.to_string(),
            confidence: clamp_unit(confidence),
        }
    }

    /// The `(file, line, title)` identity used to deduplicate findings.
    #[must_use]
    fn key(&self) -> (String, u32, String) {
        (self.file.clone(), self.line, self.title.clone())
    }
}

/// How aggressively to surface findings (kilocode Review Agent modes).
///
/// Higher strictness raises the minimum confidence required to surface a
/// finding, trading recall for precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strictness {
    /// Only high-confidence findings (threshold `0.8`).
    Strict,
    /// A pragmatic middle ground (threshold `0.5`).
    Balanced,
    /// Surface almost everything (threshold `0.2`).
    Lenient,
}

impl Strictness {
    /// Minimum confidence a finding must meet to be surfaced under this mode.
    #[must_use]
    pub const fn threshold(self) -> f32 {
        match self {
            Self::Strict => 0.8,
            Self::Balanced => 0.5,
            Self::Lenient => 0.2,
        }
    }
}

/// Merge findings that share a `(file, line, title)`, keeping the highest
/// confidence for each.
///
/// When several agents flag the same spot, the surviving finding keeps the
/// strongest confidence and that finding's `dimension`/`detail`. Input order is
/// otherwise preserved (first occurrence of each key wins its slot).
#[must_use]
pub fn dedup(findings: Vec<Finding>) -> Vec<Finding> {
    // `order` records first-seen slot per key so output is deterministic.
    let mut order: Vec<(String, u32, String)> = Vec::new();
    let mut best: HashMap<(String, u32, String), Finding> = HashMap::new();
    for f in findings {
        let key = f.key();
        if let Some(existing) = best.get_mut(&key) {
            if f.confidence > existing.confidence {
                *existing = f;
            }
        } else {
            order.push(key.clone());
            best.insert(key, f);
        }
    }
    order
        .into_iter()
        .filter_map(|k| best.remove(&k))
        .collect()
}

/// Keep findings meeting `s`'s confidence threshold, sorted by confidence
/// descending.
///
/// Ties (equal confidence) keep their relative input order, so the result is
/// deterministic. Does not deduplicate — run [`dedup`] first if needed.
#[must_use]
pub fn filter(findings: &[Finding], s: Strictness) -> Vec<Finding> {
    let threshold = s.threshold();
    let mut kept: Vec<Finding> = findings
        .iter()
        .filter(|f| f.confidence >= threshold)
        .cloned()
        .collect();
    // Sort by confidence descending; `f32` is never NaN here (clamped on
    // construction) but fall back to Equal defensively to avoid a panic.
    kept.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    kept
}

/// Take the majority verdict of a panel of adversarial verifiers.
///
/// Returns `true` only when strictly more than half of `verdicts` are `true`.
/// An empty panel and exact ties both return `false` (fail-closed): a finding
/// is only confirmed when the panel clearly agrees.
#[must_use]
pub fn vote(verdicts: &[bool]) -> bool {
    let yes = verdicts.iter().filter(|&&v| v).count();
    yes * 2 > verdicts.len()
}

/// Coarse classification for an incoming issue (kilocode auto-triage).
///
/// Serialized as the lowercase variant name for stable daemon JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLabel {
    /// A defect report.
    Bug,
    /// A feature or enhancement request.
    Feature,
    /// A support question / how-to.
    Question,
    /// A documentation issue.
    Docs,
    /// A duplicate of an existing issue (only via [`similarity`]).
    Duplicate,
}

/// Classify an issue's `title` + `body` into an [`IssueLabel`] by keyword score.
///
/// This is a deterministic keyword classifier: each candidate label accrues a
/// score from matched signal words (title matches weigh double), and the highest
/// scorer wins. Ties and a total absence of signal resolve to
/// [`IssueLabel::Question`], the safest default for human follow-up.
///
/// [`IssueLabel::Duplicate`] is never returned here — duplicates are detected
/// separately with [`similarity`] against existing issues.
#[must_use]
pub fn triage(title: &str, body: &str) -> IssueLabel {
    let title_tokens = tokenize(title);
    let body_tokens = tokenize(body);

    let score = |keywords: &[&str]| -> u32 {
        let count = |toks: &[String]| -> u32 {
            keywords
                .iter()
                .map(|kw| u32::try_from(toks.iter().filter(|t| t.as_str() == *kw).count())
                    .unwrap_or(u32::MAX))
                .sum()
        };
        // Title signal counts double — it is the strongest intent cue.
        count(&title_tokens) * 2 + count(&body_tokens)
    };

    let candidates = [
        (IssueLabel::Bug, BUG_WORDS),
        (IssueLabel::Feature, FEATURE_WORDS),
        (IssueLabel::Docs, DOCS_WORDS),
        (IssueLabel::Question, QUESTION_WORDS),
    ];

    let mut best_label = IssueLabel::Question;
    let mut best_score = 0u32;
    for (label, words) in candidates {
        let s = score(words);
        if s > best_score {
            best_score = s;
            best_label = label;
        }
    }
    best_label
}

/// Token-set Jaccard similarity of two strings, in `[0.0, 1.0]`.
///
/// Used to spot duplicate issues: `1.0` means identical token sets, `0.0` means
/// disjoint. Two empty strings are defined as `1.0` (identical), and an empty
/// vs. non-empty string is `0.0`.
#[must_use]
pub fn similarity(a: &str, b: &str) -> f32 {
    use std::collections::HashSet;
    let sa: HashSet<String> = tokenize(a).into_iter().collect();
    let sb: HashSet<String> = tokenize(b).into_iter().collect();
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 {
        return 0.0;
    }
    ratio(intersection, union)
}

/// Keyword tables for [`triage`]. Kept module-private and intentionally small;
/// the *mechanism* (scored keyword voting) is the contribution.
const BUG_WORDS: &[&str] = &[
    "bug", "crash", "panic", "error", "broken", "fail", "fails", "failed",
    "failure", "exception", "regression", "freeze", "hang", "incorrect",
    "wrong", "unexpected", "reproduce", "stacktrace", "traceback", "segfault",
];
const FEATURE_WORDS: &[&str] = &[
    "feature", "request", "add", "support", "please", "would", "could",
    "enhancement", "improve", "improvement", "proposal", "suggestion",
    "implement", "ability", "allow", "wish", "nice",
];
const DOCS_WORDS: &[&str] = &[
    "docs", "doc", "documentation", "readme", "typo", "comment", "comments",
    "guide", "tutorial", "example", "clarify", "wording", "spelling",
];
const QUESTION_WORDS: &[&str] = &[
    "how", "why", "what", "question", "help", "confused", "unclear",
    "possible", "supported", "where", "which", "anyone",
];

/// Split text into lowercase alphanumeric tokens, dropping punctuation.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Clamp a confidence into `[0.0, 1.0]`, mapping `NaN` to `0.0`.
fn clamp_unit(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// `numerator / denominator` as an `f32` in `[0.0, 1.0]`.
#[allow(clippy::cast_precision_loss)] // token counts are tiny, well under 2^24
fn ratio(numerator: usize, denominator: usize) -> f32 {
    numerator as f32 / denominator as f32
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn dedup_merges_same_key_keeping_max_confidence() {
        let raw = vec![
            Finding::new(Dimension::Bug, "a.rs", 10, "off-by-one", "lo", 0.4),
            Finding::new(Dimension::Bug, "a.rs", 10, "off-by-one", "hi", 0.9),
            Finding::new(Dimension::Bug, "a.rs", 10, "off-by-one", "mid", 0.6),
            // Different line -> distinct finding.
            Finding::new(Dimension::Style, "a.rs", 11, "off-by-one", "", 0.3),
        ];
        let merged = dedup(raw);
        assert_eq!(merged.len(), 2);
        let off = &merged[0];
        assert_eq!(off.title, "off-by-one");
        assert_eq!(off.line, 10);
        assert_eq!(off.confidence, 0.9);
        assert_eq!(off.detail, "hi", "winning finding's detail is kept");
    }

    #[test]
    fn dedup_distinct_keys_preserve_first_seen_order() {
        let raw = vec![
            Finding::new(Dimension::Bug, "z.rs", 1, "c", "", 0.5),
            Finding::new(Dimension::Bug, "y.rs", 2, "a", "", 0.5),
            Finding::new(Dimension::Bug, "x.rs", 3, "b", "", 0.5),
        ];
        let merged = dedup(raw);
        let titles: Vec<&str> = merged.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, vec!["c", "a", "b"]);
    }

    #[test]
    fn filter_respects_thresholds_and_sorts_desc() {
        let findings = vec![
            Finding::new(Dimension::Bug, "a.rs", 1, "low", "", 0.3),
            Finding::new(Dimension::Bug, "a.rs", 2, "mid", "", 0.55),
            Finding::new(Dimension::Bug, "a.rs", 3, "high", "", 0.95),
        ];
        // Strict (0.8): only "high".
        let strict = filter(&findings, Strictness::Strict);
        assert_eq!(strict.len(), 1);
        assert_eq!(strict[0].title, "high");
        // Balanced (0.5): "high" then "mid".
        let balanced = filter(&findings, Strictness::Balanced);
        assert_eq!(
            balanced.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(),
            vec!["high", "mid"]
        );
        // Lenient (0.2): all three, sorted descending.
        let lenient = filter(&findings, Strictness::Lenient);
        assert_eq!(lenient.len(), 3);
        assert!(lenient[0].confidence >= lenient[1].confidence);
        assert!(lenient[1].confidence >= lenient[2].confidence);
    }

    #[test]
    fn strictness_thresholds_are_ordered() {
        assert!(Strictness::Strict.threshold() > Strictness::Balanced.threshold());
        assert!(Strictness::Balanced.threshold() > Strictness::Lenient.threshold());
    }

    #[test]
    fn vote_majority_and_tie_is_false() {
        assert!(vote(&[true, true, false])); // 2/3
        assert!(!vote(&[true, false, false])); // 1/3
        assert!(!vote(&[true, false])); // tie -> false
        assert!(!vote(&[])); // empty -> false
        assert!(vote(&[true])); // unanimous single
        assert!(!vote(&[false, false])); // unanimous no
        assert!(vote(&[true, true, true, false])); // 3/4
    }

    #[test]
    fn triage_classifies_bug_feature_question_docs() {
        assert_eq!(
            triage("App crashes on startup", "I get a panic every time"),
            IssueLabel::Bug
        );
        assert_eq!(
            triage("Feature request: add dark mode", "Please support theming"),
            IssueLabel::Feature
        );
        assert_eq!(
            triage("How do I configure this?", "Not sure what the right setting is"),
            IssueLabel::Question
        );
        assert_eq!(
            triage("Typo in README", "The documentation has a spelling mistake"),
            IssueLabel::Docs
        );
    }

    #[test]
    fn triage_defaults_to_question_when_no_signal() {
        assert_eq!(triage("", ""), IssueLabel::Question);
        assert_eq!(triage("hello there", "general kenobi"), IssueLabel::Question);
    }

    #[test]
    fn triage_never_returns_duplicate() {
        // Duplicate must only come from similarity, never the keyword classifier.
        let label = triage("duplicate of #5", "this is a duplicate duplicate");
        assert_ne!(label, IssueLabel::Duplicate);
    }

    #[test]
    fn similarity_identical_is_one_disjoint_is_zero() {
        assert_eq!(similarity("fix the parser bug", "fix the parser bug"), 1.0);
        assert_eq!(similarity("apple banana cherry", "dog elephant frog"), 0.0);
        // Two empty strings are identical by definition.
        assert_eq!(similarity("", ""), 1.0);
        // Empty vs non-empty is disjoint.
        assert_eq!(similarity("", "something"), 0.0);
    }

    #[test]
    fn similarity_partial_overlap_is_jaccard() {
        // tokens {a,b,c} vs {b,c,d}: intersection 2, union 4 -> 0.5.
        let s = similarity("a b c", "b c d");
        assert!((s - 0.5).abs() < 1e-6);
        // Case- and punctuation-insensitive: same token sets -> 1.0.
        assert_eq!(similarity("Hello, World!", "world hello"), 1.0);
    }

    #[test]
    fn finding_clamps_confidence() {
        assert_eq!(Finding::new(Dimension::Bug, "a", 1, "t", "", 5.0).confidence, 1.0);
        assert_eq!(Finding::new(Dimension::Bug, "a", 1, "t", "", -1.0).confidence, 0.0);
        assert_eq!(Finding::new(Dimension::Bug, "a", 1, "t", "", f32::NAN).confidence, 0.0);
    }
}
