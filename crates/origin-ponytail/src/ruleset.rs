// SPDX-License-Identifier: Apache-2.0
//! The injected ponytail ruleset, filtered to the active intensity. Mirrors
//! ponytail's hooks/ponytail-instructions.js::filterSkillBodyForMode.

use crate::mode::PonytailMode;

const RULESET: &str = include_str!("../assets/ponytail-ruleset.md");

fn line_label_mode(line: &str) -> Option<PonytailMode> {
    // Intensity table row: `| **lite** | ... |`
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("| **") {
        if let Some(end) = rest.find("**") {
            return PonytailMode::parse_level(&rest[..end]);
        }
    }
    // Worked-example bullet: `- lite: ...`
    if let Some(rest) = t.strip_prefix("- ") {
        if let Some((label, _)) = rest.split_once(':') {
            return PonytailMode::parse_level(label.trim());
        }
    }
    None
}

/// Build the `<origin-ponytail>` system block for the mode, keeping only the
/// intensity-specific lines that match. `Off` ⇒ empty string.
#[must_use]
pub fn system_block(mode: PonytailMode) -> String {
    if mode == PonytailMode::Off {
        return String::new();
    }
    let body: String = RULESET
        .lines()
        .filter(|line| match line_label_mode(line) {
            Some(m) => m == mode, // mode-keyed line: keep only the active mode's
            None => true,         // ordinary rule line: always keep
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("<origin-ponytail level=\"{}\">\n{}\n</origin-ponytail>", mode.as_str(), body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_empty() {
        assert!(system_block(PonytailMode::Off).is_empty());
    }

    #[test]
    fn block_is_wrapped_and_carries_level() {
        let b = system_block(PonytailMode::Full);
        assert!(b.starts_with("<origin-ponytail level=\"full\">"));
        assert!(b.trim_end().ends_with("</origin-ponytail>"));
        assert!(b.contains("lazy senior developer"));
    }

    #[test]
    fn intensity_rows_are_filtered_by_mode() {
        // The ultra worked-example bullet appears only in ultra.
        let full = system_block(PonytailMode::Full);
        let ultra = system_block(PonytailMode::Ultra);
        assert!(!full.contains("YAGNI extremist"));
        assert!(ultra.contains("YAGNI extremist"));
    }
}
