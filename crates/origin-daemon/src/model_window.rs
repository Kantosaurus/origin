// SPDX-License-Identifier: Apache-2.0
//! The single, shared per-model context-window resolver.
//!
//! Used by the live compaction soft-cap ([`crate::agent`]), the input-bar `ctx %`
//! meter (`origin-cli`'s TUI), and the onboarding model picker. Resolution order
//! is fixed:
//!
//! 1. An explicit trailing marker `[<digits><k|m>]` (case-insensitive) on the id
//!    wins — `claude-opus-4-8[1m]` ⇒ `1_000_000`, `foo[200k]` ⇒ `200_000`
//!    (`k` ⇒ `×1_000`, `m` ⇒ `×1_000_000`). This lets a model id carry its own
//!    window even when the static table below does not know it.
//! 2. Otherwise a lowercased-substring match against a small accurate table of
//!    known families (substring so version suffixes like `-20250101` still
//!    resolve).
//! 3. Otherwise a conservative `200_000` fallback.

/// The conservative fallback context window (tokens) for an unrecognized model.
const FALLBACK_WINDOW: u32 = 200_000;

/// Resolve a model id to its context window in tokens.
///
/// See the module docs for the resolution order. Always returns a concrete value
/// (never `None`); the `200_000` fallback keeps callers simple.
#[must_use]
pub fn model_context_window(model: &str) -> u32 {
    if let Some(window) = parse_window_marker(model) {
        return window;
    }

    let m = model.to_ascii_lowercase();

    // Opus 4.8 is the one Claude family with a 1M window, alongside Gemini —
    // match both before the generic 200K Claude branch below.
    if m.contains("claude-opus-4-8") || m.contains("opus-4-8") || m.contains("gemini") {
        1_000_000
    } else if m.contains("claude")
        || m.contains("opus")
        || m.contains("sonnet")
        || m.contains("haiku")
        || m.contains("fable")
    {
        200_000
    } else if m.contains("gpt-4")
        || m.contains("gpt-5")
        || m.contains("o1")
        || m.contains("o3")
    {
        128_000
    } else {
        FALLBACK_WINDOW
    }
}

/// Parse a trailing `[<digits><k|m>]` marker (case-insensitive) into a token
/// count, e.g. `…[1m]` ⇒ `Some(1_000_000)`, `…[200k]` ⇒ `Some(200_000)`.
/// Returns `None` when the id has no such well-formed marker.
fn parse_window_marker(model: &str) -> Option<u32> {
    let model = model.trim_end();
    let inner = model.strip_suffix(']')?;
    let open = inner.rfind('[')?;
    let body = &inner[open + 1..];
    let (digits, unit) = body.split_at(body.char_indices().last()?.0);
    let mult: u32 = match unit.to_ascii_lowercase().as_str() {
        "k" => 1_000,
        "m" => 1_000_000,
        _ => return None,
    };
    if digits.is_empty() {
        return None;
    }
    let value: u32 = digits.parse().ok()?;
    Some(value.saturating_mul(mult))
}

#[cfg(test)]
mod tests {
    use super::model_context_window;

    #[test]
    fn marker_takes_precedence() {
        assert_eq!(model_context_window("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(model_context_window("foo[200k]"), 200_000);
    }

    #[test]
    fn marker_is_case_insensitive() {
        assert_eq!(model_context_window("foo[1M]"), 1_000_000);
        assert_eq!(model_context_window("foo[200K]"), 200_000);
    }

    #[test]
    fn opus_4_8_resolves_to_one_million() {
        assert_eq!(model_context_window("claude-opus-4-8"), 1_000_000);
    }

    #[test]
    fn version_suffix_still_resolves() {
        assert_eq!(model_context_window("claude-opus-4-8-20250101"), 1_000_000);
    }

    #[test]
    fn other_claude_families_stay_at_200k() {
        assert_eq!(model_context_window("claude-sonnet-4-6"), 200_000);
        assert_eq!(model_context_window("claude-haiku-4-5"), 200_000);
    }

    #[test]
    fn ported_legacy_family_asserts() {
        // Ported from the old `agent.rs::compaction_cap_tests` so coverage of the
        // known-family substring matches is not lost.
        assert_eq!(model_context_window("claude-opus-4-7-20250115"), 200_000);
        assert_eq!(model_context_window("claude-fable-5"), 200_000);
        assert_eq!(model_context_window("gpt-4o-mini"), 128_000);
    }

    #[test]
    fn gemini_is_one_million() {
        assert_eq!(model_context_window("gemini-2.5-pro"), 1_000_000);
    }

    #[test]
    fn gpt_is_128k() {
        assert_eq!(model_context_window("gpt-4o"), 128_000);
    }

    #[test]
    fn unknown_falls_back_to_200k() {
        assert_eq!(model_context_window("totally-unknown"), 200_000);
        assert_eq!(model_context_window("some-unknown-local-model"), 200_000);
    }
}
