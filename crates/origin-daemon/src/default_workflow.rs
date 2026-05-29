// SPDX-License-Identifier: Apache-2.0
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

1. /goal first. Pin the concrete outcome the user wants from this session: \
   what 'done' looks like, the success criterion, and any hard constraints. \
   This phase is owned by a dedicated /goal subagent; your job as orchestrator \
   is to invoke it and respect its output. The /goal phase MUST be \
   interactive — drive it with AskUserQuestion, presenting 2-4 mutually- \
   exclusive options per turn (one question at a time), not open-ended prose \
   prompts. Only fall back to open prose when no option set can capture the \
   choice.

2. /brainstorming next. With the goal pinned, explore HOW to reach it: \
   surface 2-3 viable approaches, name the trade-offs, recommend one. This \
   phase MUST also be interactive — every choice point is an AskUserQuestion \
   with 2-4 mutually-exclusive options, not an open-ended question. During \
   this phase, dispatch Task subagents in parallel that use WebFetch and \
   WebSearch for any external references, library docs, API shapes, or \
   unknowns. Only stop to ask the user when a decision genuinely requires \
   their input — and when you do, ask it as a multiple-choice question.

3. /writing-plans next. Produce a step-by-step implementation plan with exact \
   file paths, full code per step, and the verification command per step. \
   Save it under docs/plans/. Get user approval before executing.

4. /dispatching-parallel-agents to execute. Spawn one Task subagent per \
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
            "/goal",
            "/brainstorming",
            "/writing-plans",
            "/dispatching-parallel-agents",
            "/test-driven-development",
            "/verification-before-completion",
            "WebFetch",
            "WebSearch",
            "AskUserQuestion",
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
