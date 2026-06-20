// SPDX-License-Identifier: Apache-2.0
//! Text + helpers backing the `/ponytail-*` commands.

use crate::debt::read as read_debt;

const REVIEW: &str = include_str!("../assets/review.md");
const AUDIT: &str = include_str!("../assets/audit.md");

#[must_use]
pub fn review_prompt() -> &'static str { REVIEW }

#[must_use]
pub fn audit_prompt() -> &'static str { AUDIT }

#[must_use]
pub fn gain_text() -> &'static str {
    "ponytail measured impact (Haiku 4.5, 12 agentic feature tasks vs no-skill baseline):\n\
     LOC -54%  ·  tokens -22%  ·  cost -20%  ·  time -27%  ·  safety 100%\n\
     Biggest cut where there's a real over-build trap; ~0 where code is already minimal."
}

#[must_use]
pub fn help_text() -> &'static str {
    "ponytail commands:\n\
     /ponytail [off|lite|full|ultra]  set intensity (no arg reports current)\n\
     /ponytail-review                 over-engineering review of the working diff\n\
     /ponytail-audit                  over-engineering scan of the whole repo\n\
     /ponytail-debt                   ledger of deferred ponytail: shortcuts + overrides\n\
     /ponytail-gain                   measured impact scoreboard\n\
     /ponytail-help                   this list"
}

/// Pull `ponytail:` markers out of a concatenated `file:line: text` tree dump.
#[must_use]
pub fn harvest_comments(tree: &str) -> Vec<String> {
    tree.lines()
        .filter(|l| l.contains("ponytail:"))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Render the debt ledger + a hint to harvest code markers.
#[must_use]
pub fn debt_report() -> String {
    let events = read_debt();
    if events.is_empty() {
        return "ponytail debt: ledger empty. Nothing deferred yet.".to_string();
    }
    let mut out = format!("ponytail debt ledger ({} entries):\n", events.len());
    for e in events {
        out.push_str(&format!("  {:?}  {}  → {}\n", e.action, e.dep, e.native));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_nonempty() {
        assert!(review_prompt().contains("over-engineering") || review_prompt().contains("delete"));
        assert!(!audit_prompt().is_empty());
        assert!(help_text().contains("/ponytail"));
        assert!(gain_text().contains('%'));
    }

    #[test]
    fn harvest_finds_markers() {
        let tree = "src/a.rs:12: // ponytail: global lock, per-account if throughput matters\nsrc/b.rs:3: let x = 1;\n";
        let hits = harvest_comments(tree);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("global lock"));
    }
}
