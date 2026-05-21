//! Default-workflow directive prepended to every system prompt.
//!
//! Origin tells the model to follow a brainstorm → plan → dispatch flow for
//! anything non-trivial, instead of waiting for the user to invoke each skill
//! by name. Trivial requests (single lookups, one-line edits) bypass the flow.
//!
//! Disable globally by setting the env var `ORIGIN_DEFAULT_WORKFLOW=off`.

pub const DEFAULT_WORKFLOW: &str = "\
DEFAULT WORKFLOW

For trivial requests — single-fact questions, one-line edits, direct lookups, \
status checks — answer or act immediately.

For everything else, follow this orchestration without being asked:

1. /brainstorming first. Clarify scope, constraints, and design. During this \
   phase, dispatch Task subagents in parallel that use WebFetch and WebSearch \
   for any external references, library docs, API shapes, or unknowns. Only \
   stop to ask the user when a decision genuinely requires their input.

2. /writing-plans next. Produce a step-by-step implementation plan with exact \
   file paths, full code per step, and the verification command per step. \
   Save it under docs/superpowers/plans/. Get user approval before executing.

3. /dispatching-parallel-agents to execute. Spawn one Task subagent per \
   independent unit of work. Every subagent MUST:
     a. /test-driven-development — write the failing test first, run it and \
        confirm RED, implement the minimum, run and confirm GREEN
     b. /verification-before-completion — run the verification command for \
        the task, paste its output, and never claim success without that \
        fresh evidence in hand

Within any sequence of work, do not advance to the next task until the \
current task's verification command has been run and its output confirms \
success.

This is the default. Skip it only when the work is genuinely trivial.\
";

/// Returns the workflow directive unless `ORIGIN_DEFAULT_WORKFLOW=off`.
///
/// Empty string disables the block; the caller concatenates unconditionally.
#[must_use]
pub fn directive() -> &'static str {
    match std::env::var("ORIGIN_DEFAULT_WORKFLOW").as_deref() {
        Ok("off" | "false" | "0") => "",
        _ => DEFAULT_WORKFLOW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_contains_workflow_phases() {
        let d = DEFAULT_WORKFLOW;
        for phase in [
            "/brainstorming",
            "/writing-plans",
            "/dispatching-parallel-agents",
            "/test-driven-development",
            "/verification-before-completion",
            "WebFetch",
            "WebSearch",
        ] {
            assert!(d.contains(phase), "DEFAULT_WORKFLOW missing `{phase}`");
        }
    }

    #[test]
    fn env_override_disables_directive() {
        // Use a temp env var so we don't leak into other tests.
        std::env::set_var("ORIGIN_DEFAULT_WORKFLOW", "off");
        assert_eq!(directive(), "");
        std::env::remove_var("ORIGIN_DEFAULT_WORKFLOW");
        assert_eq!(directive(), DEFAULT_WORKFLOW);
    }
}
