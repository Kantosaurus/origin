// SPDX-License-Identifier: Apache-2.0
//! Turn-end memory extraction. Scans assistant + user messages for patterns
//! worth remembering and emits [`MemoryProposal`] candidates.
//!
//! Uses [`regex::RegexSet`] for a single-pass existence check on the full text
//! before doing per-line per-pattern capture extraction.

use regex::{Regex, RegexSet};

// ── Public types ──────────────────────────────────────────────────────────────

/// One candidate the daemon will surface for user accept/reject/edit.
#[derive(Debug, Clone)]
pub struct MemoryProposal {
    /// Stable id within the session for this proposal (counter from 1).
    pub proposal_id: u32,
    pub body: String,
    pub suggested_tags: Vec<String>,
    /// Reason this was extracted (regex name or constant).
    pub source_hint: &'static str,
}

// ── Pattern table ─────────────────────────────────────────────────────────────

/// Metadata for one extraction rule.
struct PatternDef {
    pattern: &'static str,
    in_user: bool,
    in_assistant: bool,
    tag: &'static str,
    source_hint: &'static str,
}

const PATTERNS: &[PatternDef] = &[
    PatternDef {
        // "remember: ..." or "remember that ..."
        pattern: r"(?i)\bremember(?: that)?[: ]+(.+)",
        in_user: true,
        in_assistant: false,
        tag: "user-statement",
        source_hint: "remember-directive",
    },
    PatternDef {
        // "I prefer / I like / I always / I never ..."
        pattern: r"(?i)\bi (?:prefer|like|always|never)\b.{0,140}",
        in_user: true,
        in_assistant: false,
        tag: "feedback",
        source_hint: "preference-phrase",
    },
    PatternDef {
        // "I'll remember / I'll note that ..."
        pattern: r"(?i)\bi'll (?:remember|note) that (.+?)(?:\.|$)",
        in_user: false,
        in_assistant: true,
        tag: "assistant-note",
        source_hint: "assistant-note",
    },
    PatternDef {
        // "TODO: ..."
        pattern: r"(?i)\bTODO\b: (.+)",
        in_user: true,
        in_assistant: true,
        tag: "todo",
        source_hint: "todo-marker",
    },
];

// ── Proposer ──────────────────────────────────────────────────────────────────

/// Scans conversation turns for patterns worth persisting as memories.
///
/// One [`RegexSet`] per side enables a single-pass existence check; per-pattern
/// [`Regex`] values are used only when the set fires to extract capture groups.
pub struct Proposer {
    /// One-pass set matching all user-side patterns; used as a fast pre-filter.
    user_set: RegexSet,
    /// One-pass set matching all assistant-side patterns; used as a fast pre-filter.
    asst_set: RegexSet,
    /// Per-pattern compiled regexes for capture extraction (same order as [`PATTERNS`]).
    captures: Vec<Regex>,
}

impl Proposer {
    /// Build compiled regex sets.
    ///
    /// # Panics
    /// Panics if a regex constant in [`PATTERNS`] is malformed — a build-time
    /// invariant, not a runtime condition (all patterns are string literals).
    #[must_use]
    pub fn new() -> Self {
        let user_patterns: Vec<&str> = PATTERNS.iter().filter(|p| p.in_user).map(|p| p.pattern).collect();
        let asst_patterns: Vec<&str> = PATTERNS
            .iter()
            .filter(|p| p.in_assistant)
            .map(|p| p.pattern)
            .collect();

        let user_set = RegexSet::new(&user_patterns).expect("user regex set compile");
        let asst_set = RegexSet::new(&asst_patterns).expect("asst regex set compile");

        let captures: Vec<Regex> = PATTERNS
            .iter()
            .map(|p| Regex::new(p.pattern).expect("capture regex compile"))
            .collect();

        Self {
            user_set,
            asst_set,
            captures,
        }
    }

    /// Scan `user` and `assistant` messages at turn end.
    ///
    /// Returns 0..N [`MemoryProposal`] candidates. Each match increments
    /// `*next_id` and uses the pre-increment value as the proposal id.
    ///
    /// The `RegexSet` provides a cheap all-patterns-in-one-pass check before
    /// the per-line per-pattern extraction loop.
    #[must_use]
    pub fn scan(&self, user: &str, assistant: &str, next_id: &mut u32) -> Vec<MemoryProposal> {
        let mut out: Vec<MemoryProposal> = Vec::new();
        // Dedup by body so e.g. "remember: i prefer X" doesn't fire two near-identical
        // proposals (one from the remember pattern, one from the preference pattern).
        let mut seen_bodies: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Fast-path: skip per-line work if no user-side pattern fires at all.
        if self.user_set.is_match(user) {
            for (idx, def) in PATTERNS.iter().enumerate() {
                if !def.in_user {
                    continue;
                }
                for line in user.lines() {
                    if let Some(proposal) = self.extract(idx, def, line, next_id) {
                        if seen_bodies.insert(proposal.body.clone()) {
                            out.push(proposal);
                        } else {
                            // Roll back the id we burned on the duplicate so the
                            // counter stays packed.
                            *next_id -= 1;
                        }
                    }
                }
            }
        }

        // Fast-path: skip per-line work if no assistant-side pattern fires at all.
        if self.asst_set.is_match(assistant) {
            for (idx, def) in PATTERNS.iter().enumerate() {
                if !def.in_assistant {
                    continue;
                }
                for line in assistant.lines() {
                    if let Some(proposal) = self.extract(idx, def, line, next_id) {
                        if seen_bodies.insert(proposal.body.clone()) {
                            out.push(proposal);
                        } else {
                            *next_id -= 1;
                        }
                    }
                }
            }
        }

        out
    }

    /// Extract a proposal from `text` using pattern at `idx`. Returns `None`
    /// if the pattern does not match.
    fn extract(&self, idx: usize, def: &PatternDef, text: &str, next_id: &mut u32) -> Option<MemoryProposal> {
        let caps = self.captures[idx].captures(text)?;

        // If the pattern has a named/numbered capture group (len > 1), use
        // group 1 as the body; otherwise use the whole match.
        let body = if caps.len() > 1 {
            caps.get(1).map_or("", |m| m.as_str())
        } else {
            caps.get(0).map_or("", |m| m.as_str())
        }
        .trim()
        .to_string();

        if body.is_empty() {
            return None;
        }

        let proposal_id = *next_id;
        *next_id += 1;

        Some(MemoryProposal {
            proposal_id,
            body,
            suggested_tags: vec![def.tag.to_string()],
            source_hint: def.source_hint,
        })
    }
}

impl Default for Proposer {
    fn default() -> Self {
        Self::new()
    }
}
