// SPDX-License-Identifier: Apache-2.0
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
    /// Short descriptions parallel to [`skills`](Self::skills) (frontmatter
    /// `description:`). Same length as `skills` when populated by
    /// [`load_sources`]; an empty vector means "no descriptions known" so a
    /// missing entry is treated as the empty string. Additive — consumers
    /// that don't render descriptions are unaffected.
    pub skill_descriptions: Vec<String>,
    /// Built-in slash-command verbs that the TUI dispatches inline (e.g.
    /// `clear`, `effort`, `model`). Matched against the `/<partial>` shape
    /// exactly like skills so the popup surfaces them too.
    pub verbs: Vec<String>,
    /// Short static descriptions parallel to [`verbs`](Self::verbs). Same
    /// length as `verbs` when populated; an empty vector means "no
    /// descriptions known". Additive.
    pub verb_descriptions: Vec<String>,
    /// Workflow names from `~/.origin/workflows.toml`.
    pub workflows: Vec<String>,
}

impl CompletionSources {
    /// The combined skill-shape candidate names: built-in [`verbs`](Self::verbs)
    /// followed by [`skills`](Self::skills). Used for the `/<partial>` and
    /// `/-<partial>` shapes so both kinds match identically.
    #[must_use]
    pub fn skill_candidates(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.verbs.len() + self.skills.len());
        out.extend(self.verbs.iter().cloned());
        out.extend(self.skills.iter().cloned());
        out
    }

    /// The descriptions parallel to [`skill_candidates`](Self::skill_candidates),
    /// in the same order (verbs first, then skills). A candidate with no known
    /// description maps to the empty string so the two vectors stay aligned.
    #[must_use]
    pub fn skill_candidate_descriptions(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.verbs.len() + self.skills.len());
        out.extend(aligned_descriptions(&self.verbs, &self.verb_descriptions));
        out.extend(aligned_descriptions(&self.skills, &self.skill_descriptions));
        out
    }
}

/// Pad/truncate `descriptions` so it lines up one-to-one with `names`,
/// filling missing entries with the empty string. Lets callers store an
/// empty `descriptions` vector to mean "none known" without panicking on a
/// length mismatch.
fn aligned_descriptions(names: &[String], descriptions: &[String]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(i, _)| descriptions.get(i).cloned().unwrap_or_default())
        .collect()
}

/// The built-in slash-command verbs the TUI dispatches inline, paired with a
/// short static description. Kept here (rather than in `main.rs`) so the
/// completion sources are self-contained. Only verbs that are actually wired
/// up belong here; `theme`/`vim`/`help` are intentionally omitted until a
/// later wave dispatches them.
const BUILTIN_VERBS: &[(&str, &str)] = &[
    ("model", "switch the active model"),
    ("effort", "set reasoning effort (fast..max)"),
    ("fast", "shortcut for minimal reasoning effort"),
    ("output-style", "set the output style"),
    ("steer", "queue a steering hint for the next turn"),
    ("plan", "toggle read-only plan mode"),
    ("attach", "stage an image/PDF for the next prompt"),
    ("account", "switch the active provider account"),
    ("mem", "manage proposed memories"),
    ("permissions", "toggle approving tools before they run"),
    ("mouse", "toggle mouse capture (off to select & copy)"),
    ("theme", "switch palette (default/dark/light/high-contrast)"),
    ("copy", "copy the last reply to the clipboard (OSC 52)"),
    ("help", "show the command + keybinding cheatsheet"),
    ("clear", "clear the conversation and goal"),
];

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
    // Skill shapes — `/-<name>` (deactivate) or `/<name>` (activate). Both
    // match against the combined verb+skill candidate list.
    let candidates = sources.skill_candidates();
    if let Some(partial) = buffer.strip_prefix("/-") {
        let partial = partial.to_string();
        return complete_with(buffer, &partial, &candidates, |full| format!("/-{full}"));
    }
    if let Some(partial) = buffer.strip_prefix('/') {
        // Whitespace inside the partial means it's not a slash command.
        if partial.chars().any(char::is_whitespace) {
            return CompletionResult::NoMatch;
        }
        let partial = partial.to_string();
        return complete_with(buffer, &partial, &candidates, |full| format!("/{full}"));
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
/// (every `<dir>/SKILL.md`).
///
/// Also reads `~/.origin/workflows.toml` for workflows.
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
    //
    // Keep `skills` and `skill_descriptions` index-aligned: build them in one
    // pass so a later wave can render the frontmatter description next to each
    // candidate.
    let (skills, skill_descriptions): (Vec<String>, Vec<String>) = origin_skills::load_all(&skills_dir)
        .map(|v| v.into_iter().map(|s| (s.front.name, s.front.description)).unzip())
        .unwrap_or_default();
    let workflows: Vec<String> = crate::workflows::load_from(&workflows_path)
        .ok()
        .flatten()
        .map(|f| f.workflows.into_iter().map(|w| w.name).collect())
        .unwrap_or_default();
    let verbs: Vec<String> = BUILTIN_VERBS.iter().map(|(v, _)| (*v).to_string()).collect();
    let verb_descriptions: Vec<String> = BUILTIN_VERBS.iter().map(|(_, d)| (*d).to_string()).collect();
    CompletionSources {
        skills,
        skill_descriptions,
        verbs,
        verb_descriptions,
        workflows,
    }
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
            ..Default::default()
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

    /// `load_sources` must inject the dispatched built-in verbs so typing
    /// `/eff` Tab-completes to `/effort` even with no matching skill.
    #[test]
    fn builtin_verb_completes_effort() {
        let sources = load_sources();
        assert!(
            sources.verbs.iter().any(|v| v == "effort"),
            "expected `effort` built-in verb in completion sources, got: {:?}",
            sources.verbs
        );
        let mut buf = "/eff".to_string();
        let r = complete(&mut buf, &sources);
        assert_eq!(r, CompletionResult::UniqueCompletion);
        assert_eq!(buf, "/effort");
    }

    /// The still-non-dispatched verb `vim` must NOT be injected yet — a later
    /// wave wires it up. (`theme`/`help` ARE dispatched now, so they belong.)
    #[test]
    fn undispatched_verbs_are_absent() {
        let sources = load_sources();
        assert!(
            !sources.verbs.iter().any(|v| v == "vim"),
            "did not expect `vim` among built-in verbs: {:?}",
            sources.verbs
        );
        for present in ["theme", "help"] {
            assert!(
                sources.verbs.iter().any(|v| v == present),
                "`{present}` should be a built-in verb"
            );
        }
    }

    /// Built-in verbs participate in the same `/<partial>` match as skills.
    #[test]
    fn verbs_and_skills_share_skill_shape() {
        let sources = CompletionSources {
            skills: vec!["effortless-skill".into()],
            verbs: vec!["effort".into()],
            ..Default::default()
        };
        let mut buf = "/effo".to_string();
        let r = complete(&mut buf, &sources);
        // Two candidates share the "/effo" prefix: the verb and the skill.
        match r {
            CompletionResult::MultipleCandidates { candidates } => {
                assert_eq!(candidates.len(), 2);
                assert!(candidates.iter().any(|c| c == "effort"));
                assert!(candidates.iter().any(|c| c == "effortless-skill"));
            }
            other => panic!("expected MultipleCandidates, got {other:?}"),
        }
    }

    /// `skill_candidate_descriptions` stays index-aligned with
    /// `skill_candidates` (verbs first, then skills), padding missing
    /// entries with the empty string.
    #[test]
    fn candidate_descriptions_stay_aligned() {
        let sources = CompletionSources {
            skills: vec!["alpha".into(), "beta".into()],
            skill_descriptions: vec!["A skill".into()], // shorter on purpose
            verbs: vec!["effort".into()],
            verb_descriptions: vec!["set reasoning effort".into()],
            ..Default::default()
        };
        let names = sources.skill_candidates();
        let descs = sources.skill_candidate_descriptions();
        assert_eq!(names.len(), descs.len());
        assert_eq!(names, vec!["effort", "alpha", "beta"]);
        assert_eq!(descs[0], "set reasoning effort");
        assert_eq!(descs[1], "A skill");
        assert_eq!(descs[2], ""); // missing description → empty string
    }
}
