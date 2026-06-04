// SPDX-License-Identifier: Apache-2.0
//! `@subagent` mention forcing for the interactive CLI.
//!
//! When the user's turn begins with `@name`, the text is rewritten into an
//! explicit delegation directive so the agent forces a `Task` call to that
//! declarative sub-agent (from the `<origin-subagents>` system block) instead of
//! handling the request inline. A leading `@` that is not a valid mention (e.g.
//! a stray `@` or an email fragment that does not start the line) is left
//! untouched, so normal prompts stay byte-identical.

/// Rewrite a leading `@name …` mention into a sub-agent delegation directive.
///
/// Returns the input unchanged when it does not begin with a `@<name>` token
/// (name = `[A-Za-z0-9_-]+`).
#[must_use]
pub fn force_subagent(text: &str) -> String {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return text.to_string();
    };
    let name_len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    if name_len == 0 {
        return text.to_string();
    }
    let name = &rest[..name_len];
    let goal = rest[name_len..].trim();
    format!(
        "[Directed to the `{name}` sub-agent] Use the Task tool to delegate to the `{name}` \
         sub-agent listed in the <origin-subagents> block, setting allowed_tools to exactly that \
         sub-agent's listed tools, with this goal: {goal}"
    )
}

#[cfg(test)]
mod tests {
    use super::force_subagent;

    #[test]
    fn mention_is_rewritten_to_a_task_delegation() {
        let out = force_subagent("@reviewer check the auth flow for bugs");
        assert!(out.contains("`reviewer` sub-agent"));
        assert!(out.contains("Task tool"));
        assert!(out.contains("check the auth flow for bugs"));
    }

    #[test]
    fn plain_prompt_is_unchanged() {
        let text = "refactor the parser for clarity";
        assert_eq!(force_subagent(text), text);
    }

    #[test]
    fn mid_text_at_sign_is_not_a_mention() {
        let text = "send the report to alice@example.com";
        assert_eq!(force_subagent(text), text);
    }

    #[test]
    fn bare_at_sign_is_not_a_mention() {
        assert_eq!(force_subagent("@ not a name"), "@ not a name");
    }

    #[test]
    fn leading_whitespace_before_mention_still_forces() {
        let out = force_subagent("  @planner draft a plan");
        assert!(out.contains("`planner` sub-agent"));
        assert!(out.contains("draft a plan"));
    }
}
