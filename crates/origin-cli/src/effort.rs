// SPDX-License-Identifier: Apache-2.0
//! Reasoning-effort controls: `/effort <level>` and `/fast`.
#![allow(
    clippy::must_use_candidate,
    clippy::module_name_repetitions,
    clippy::enum_variant_names
)]
//!
//! These mirror claude-code's `/effort` slider and `/fast` mode. The resolved
//! value is an [`Option<ReasoningEffort>`] that threads onto the prompt path; a
//! value of `None` means "unspecified", which is the default and leaves the
//! provider wire byte-identical to before. This module is the parser + the
//! additive value type; it is intentionally self-contained and unit-tested so
//! the deeper wire threading can adopt it without re-deriving the parsing.

/// A reasoning-effort level for a turn.
///
/// `Fast` is the dedicated low-latency mode (`/fast`); the others form the
/// `/effort` slider. The wire is only affected when a level is actually set —
/// the default prompt path passes `None` and is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// Lowest latency; minimal deliberation (`/fast`).
    Fast,
    /// Low effort.
    Low,
    /// Balanced effort (the usual middle setting).
    Medium,
    /// High effort; more deliberation.
    High,
    /// Maximum effort; deepest deliberation.
    Max,
}

impl ReasoningEffort {
    /// The canonical lowercase token for this level (round-trips with
    /// [`ReasoningEffort::parse_level`]).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// Parse a single effort level token (case-insensitive). Accepts the
    /// canonical names plus a few aliases (`min`→`low`, `mid`→`medium`,
    /// `ultra`/`maximum`→`max`).
    #[must_use]
    pub fn parse_level(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "fast" => Some(Self::Fast),
            "low" | "min" => Some(Self::Low),
            "medium" | "mid" => Some(Self::Medium),
            "high" => Some(Self::High),
            "max" | "maximum" | "ultra" => Some(Self::Max),
            _ => None,
        }
    }
}

/// Parse an `/effort <level>` or `/fast` slash command line.
///
/// Returns:
/// - `Some(Some(level))` when the line is a recognized effort command;
/// - `Some(None)` when the line is `/effort` with a missing/invalid level
///   (a usage error the caller should surface);
/// - `None` when the line is not an effort command at all (fall through).
#[must_use]
pub fn parse_effort_command(line: &str) -> Option<Option<ReasoningEffort>> {
    let trimmed = line.trim();
    if trimmed == "/fast" {
        return Some(Some(ReasoningEffort::Fast));
    }
    let rest = trimmed.strip_prefix("/effort")?;
    // Require a word boundary so `/effortfoo` is not matched.
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(ReasoningEffort::parse_level(rest.trim()))
}

#[cfg(test)]
mod tests {
    use super::{parse_effort_command, ReasoningEffort};

    #[test]
    fn fast_command_maps_to_fast() {
        assert_eq!(parse_effort_command("/fast"), Some(Some(ReasoningEffort::Fast)));
    }

    #[test]
    fn effort_levels_parse() {
        assert_eq!(
            parse_effort_command("/effort high"),
            Some(Some(ReasoningEffort::High))
        );
        assert_eq!(
            parse_effort_command("/effort ULTRA"),
            Some(Some(ReasoningEffort::Max))
        );
        assert_eq!(
            parse_effort_command("/effort min"),
            Some(Some(ReasoningEffort::Low))
        );
    }

    #[test]
    fn effort_without_valid_level_is_usage_error() {
        assert_eq!(parse_effort_command("/effort"), Some(None));
        assert_eq!(parse_effort_command("/effort bogus"), Some(None));
    }

    #[test]
    fn non_effort_lines_fall_through() {
        assert_eq!(parse_effort_command("hello world"), None);
        assert_eq!(parse_effort_command("/effortfoo"), None);
        assert_eq!(parse_effort_command("/model auto"), None);
    }

    #[test]
    fn level_token_round_trips() {
        for lvl in [
            ReasoningEffort::Fast,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ] {
            assert_eq!(ReasoningEffort::parse_level(lvl.as_str()), Some(lvl));
        }
    }
}
