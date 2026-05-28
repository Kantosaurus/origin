//! Pure Tab-completion logic for the TUI input buffer.
//!
//! Detects the shape of the partial token in the buffer and rewrites the
//! buffer in-place to the completed form. No I/O — the caller passes in a
//! [`CompletionSources`] snapshot (skill + workflow names read once at
//! startup, or refreshed on demand).
//!
//! Three shapes are recognized:
//! - `/<partial>` and `/<plugin>:<partial>` — match against skills.
//! - `/-<partial>` — match against skills (deactivate form).
//! - `{workflow:<partial>` (with or without closing `}`) — match against workflows.
//!
//! Anything else returns [`CompletionResult::NoMatch`] so the caller can
//! choose not to consume the Tab.

#[derive(Debug, Clone, Default)]
pub struct CompletionSources {
    /// Skill names as they appear in the `name:` frontmatter field.
    pub skills: Vec<String>,
    /// Workflow names from `~/.origin/workflows.toml`.
    pub workflows: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompletionResult {
    /// The buffer did not match any completable shape; the caller should
    /// pass the Tab through to its default handler (or ignore it).
    NoMatch,
    /// Exactly one candidate matched; the buffer has been rewritten in
    /// full and the caller should re-render.
    UniqueCompletion,
    /// Multiple candidates matched; the buffer has been extended to the
    /// longest common prefix, and `candidates` lists the matches so the
    /// caller can display them as a status line.
    MultipleCandidates { candidates: Vec<String> },
}

/// Rewrite `buffer` to the completed token, if one of the three shapes
/// applies. Returns a [`CompletionResult`] describing what happened.
pub fn complete(buffer: &mut String, sources: &CompletionSources) -> CompletionResult {
    // Workflow shape — must start with `{workflow:` (the `}` is optional
    // because the user is mid-type).
    if let Some(partial) = buffer.strip_prefix("{workflow:") {
        // Strip trailing `}` if present so we match against bare names.
        let partial = partial.strip_suffix('}').unwrap_or(partial);
        let partial = partial.to_string();
        return complete_with(buffer, &partial, &sources.workflows, |full| {
            format!("{{workflow:{full}}}")
        });
    }
    // Skill shapes — `/-<name>` (deactivate) or `/<name>` (activate).
    if let Some(partial) = buffer.strip_prefix("/-") {
        let partial = partial.to_string();
        return complete_with(buffer, &partial, &sources.skills, |full| format!("/-{full}"));
    }
    if let Some(partial) = buffer.strip_prefix('/') {
        // Whitespace inside the partial means it's not a slash command.
        if partial.chars().any(char::is_whitespace) {
            return CompletionResult::NoMatch;
        }
        let partial = partial.to_string();
        return complete_with(buffer, &partial, &sources.skills, |full| format!("/{full}"));
    }
    CompletionResult::NoMatch
}

/// Inner helper: find matches of `partial` in `candidates`, rewrite
/// `buffer` to the longest common prefix (or full single match), and
/// return the appropriate result. `wrap` reconstructs the surrounding
/// syntax (slash, dash, braces).
fn complete_with(
    buffer: &mut String,
    partial: &str,
    candidates: &[String],
    wrap: impl Fn(&str) -> String,
) -> CompletionResult {
    let matches: Vec<&String> = candidates.iter().filter(|c| c.starts_with(partial)).collect();
    match matches.len() {
        0 => CompletionResult::NoMatch,
        1 => {
            *buffer = wrap(matches[0]);
            CompletionResult::UniqueCompletion
        }
        _ => {
            let lcp = longest_common_prefix(&matches);
            if lcp.len() > partial.len() {
                *buffer = wrap(&lcp);
            }
            CompletionResult::MultipleCandidates {
                candidates: matches.iter().map(|s| (*s).clone()).collect(),
            }
        }
    }
}

/// Longest common prefix of a non-empty slice of strings.
fn longest_common_prefix(matches: &[&String]) -> String {
    let first = matches[0].as_str();
    let mut end = first.len();
    for s in &matches[1..] {
        end = end.min(common_prefix_len(first, s));
    }
    first[..end].to_string()
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(c, _)| c.len_utf8())
        .sum()
}

// ---------------------------------------------------------------------------
// Loader helpers — read the live catalog from disk.
// ---------------------------------------------------------------------------

/// Build a [`CompletionSources`] by reading the embedded `superpowers/`
/// skill catalog merged with any user overrides in `~/.origin/skills/`
/// (every `<dir>/SKILL.md`), plus `~/.origin/workflows.toml` for workflows.
///
/// Failures degrade to empty lists so a missing directory or corrupt file
/// doesn't break Tab.
#[must_use]
pub fn load_sources() -> CompletionSources {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let skills_dir = home.join(".origin").join("skills");
    let workflows_path = home.join(".origin").join("workflows.toml");

    // Use `load_all` (embedded + user overrides) rather than `load_skills_dir`
    // (user-only). Otherwise the suggestion popup is empty on a fresh install
    // because `~/.origin/skills/` exists but is empty after onboarding — the
    // bundled `superpowers` skills (systematic-debugging, impeccable, etc.)
    // never become discoverable through `/`.
    let skills: Vec<String> = origin_skills::load_all(&skills_dir)
        .map(|v| v.into_iter().map(|s| s.front.name).collect())
        .unwrap_or_default();
    let workflows: Vec<String> = crate::workflows::load_from(&workflows_path)
        .ok()
        .flatten()
        .map(|f| f.workflows.into_iter().map(|w| w.name).collect())
        .unwrap_or_default();
    CompletionSources { skills, workflows }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::useless_vec)] // unit-test ergonomics
mod tests {
    use super::*;

    fn srcs() -> CompletionSources {
        CompletionSources {
            skills: vec![
                "frontend-design".into(),
                "frontend-design:frontend-design".into(),
                "impeccable".into(),
                "polish".into(),
            ],
            workflows: vec!["frontend-design".into(), "polish-pass".into()],
        }
    }

    #[test]
    fn slash_unique_completion() {
        let mut buf = "/impe".to_string();
        let r = complete(&mut buf, &srcs());
        assert_eq!(r, CompletionResult::UniqueCompletion);
        assert_eq!(buf, "/impeccable");
    }

    #[test]
    fn slash_multiple_lcp_completion() {
        let mut buf = "/fro".to_string();
        let r = complete(&mut buf, &srcs());
        match r {
            CompletionResult::MultipleCandidates { candidates } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected MultipleCandidates, got {other:?}"),
        }
        // LCP of "frontend-design" and "frontend-design:frontend-design"
        // is the full first name.
        assert_eq!(buf, "/frontend-design");
    }

    #[test]
    fn slash_no_match() {
        let mut buf = "/xyz".to_string();
        assert_eq!(complete(&mut buf, &srcs()), CompletionResult::NoMatch);
        assert_eq!(buf, "/xyz");
    }

    #[test]
    fn dash_deactivate_completion() {
        let mut buf = "/-impe".to_string();
        let r = complete(&mut buf, &srcs());
        assert_eq!(r, CompletionResult::UniqueCompletion);
        assert_eq!(buf, "/-impeccable");
    }

    #[test]
    fn workflow_unique_completion() {
        let mut buf = "{workflow:polish".to_string();
        let r = complete(&mut buf, &srcs());
        assert_eq!(r, CompletionResult::UniqueCompletion);
        assert_eq!(buf, "{workflow:polish-pass}");
    }

    #[test]
    fn workflow_completion_with_closing_brace() {
        // User typed the brace already; we still match on the inner partial.
        let mut buf = "{workflow:polish}".to_string();
        let r = complete(&mut buf, &srcs());
        assert_eq!(r, CompletionResult::UniqueCompletion);
        assert_eq!(buf, "{workflow:polish-pass}");
    }

    #[test]
    fn workflow_multiple_returns_candidates() {
        let mut buf = "{workflow:".to_string();
        let r = complete(&mut buf, &srcs());
        match r {
            CompletionResult::MultipleCandidates { candidates } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected MultipleCandidates, got {other:?}"),
        }
    }

    #[test]
    fn free_form_text_is_no_match() {
        let mut buf = "hello world".to_string();
        assert_eq!(complete(&mut buf, &srcs()), CompletionResult::NoMatch);
        assert_eq!(buf, "hello world");
    }

    #[test]
    fn slash_with_whitespace_is_no_match() {
        let mut buf = "/foo bar".to_string();
        assert_eq!(complete(&mut buf, &srcs()), CompletionResult::NoMatch);
    }

    #[test]
    fn longest_common_prefix_handles_full_match() {
        let names = vec!["alpha".to_string(), "alpha".to_string()];
        let refs: Vec<&String> = names.iter().collect();
        assert_eq!(longest_common_prefix(&refs), "alpha");
    }

    #[test]
    fn common_prefix_len_handles_unicode_safely() {
        assert_eq!(common_prefix_len("αβ", "αγ"), "α".len());
    }

    /// Regression: `load_sources()` must merge embedded skills (the vendored
    /// `superpowers/` catalog) with user overrides, not just read the user
    /// directory. Otherwise a fresh install with an empty `~/.origin/skills/`
    /// yields an empty suggestion list and typing `/` shows no popup.
    ///
    /// We don't drive `load_sources` directly (it reads `$HOME`/`ORIGIN_HOME`
    /// and this crate forbids `unsafe`, which `std::env::set_var` now
    /// requires). Instead we exercise the same `origin_skills::load_all`
    /// path with a guaranteed-empty user root — if the embedded catalog is
    /// wired up, the merged list is still non-empty.
    #[test]
    fn embedded_skills_populate_completion_sources() {
        let empty_user_root = std::path::Path::new("/this/path/does/not/exist/zzz");
        let names: Vec<String> = origin_skills::load_all(empty_user_root)
            .map(|v| v.into_iter().map(|s| s.front.name).collect())
            .unwrap_or_default();
        assert!(
            !names.is_empty(),
            "expected embedded skills to populate completion sources, got empty list"
        );
        assert!(
            names.iter().any(|n| n == "systematic-debugging"),
            "expected `systematic-debugging` skill in completion sources, got: {names:?}"
        );
    }
}
