//! `origin --tutorial` — a 7-step guided tour of origin's core surfaces.
//!
//! The data table ([`steps`]) is decoupled from the runner ([`run`]) so the
//! content is straightforward to unit-test, and so the runner can be driven
//! against arbitrary [`std::io::BufRead`] / [`std::io::Write`] pairs in tests
//! (instead of stdin/stdout).

#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub id: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

/// The ordered list of tutorial steps. Order is part of the public contract.
#[must_use]
pub const fn steps() -> &'static [Step] {
    &[
        Step {
            id: "welcome",
            title: "Welcome to origin",
            body: "We'll spend ~5 minutes touring the agent loop, code graph, memory, skills, and swarm. Press Enter to continue.",
        },
        Step {
            id: "agent-loop",
            title: "The agent loop",
            body: "Type a prompt; origin streams the response, parses tool_use, and runs pure tools speculatively. Try: \"List the files in this directory.\"",
        },
        Step {
            id: "code-graph",
            title: "Code knowledge graph",
            body: "origin builds a graph of your code on first run. Try: \"What calls the function `foo`?\"",
        },
        Step {
            id: "memory",
            title: "Cross-session memory",
            body: "Memories are auto-extracted at the end of each turn. The side panel lets you accept/reject. Try: \"Remember that I prefer 2-space indents in Python.\"",
        },
        Step {
            id: "skills",
            title: "Skills",
            body: "Skills are markdown-frontmatter capabilities; origin injects matching ones automatically. Try: \"Use the refactor skill to clean up README.md.\"",
        },
        Step {
            id: "swarm",
            title: "Parallel workers",
            body: "Spawn a swarm to tackle a refactor in parallel. Try: \"Split this module into three files in parallel.\"",
        },
        Step {
            id: "done",
            title: "You're set",
            body: "Tour complete. `origin --help` lists every subcommand. Run `origin run` for a one-shot, or just `origin` for the TUI.",
        },
    ]
}

/// Run the tutorial interactively against a [`std::io::BufRead`] + writer pair.
///
/// Each step prints `-- {title} --`, the body, a `(press Enter)` prompt, then
/// blocks on a single line of input. Returning `Ok(())` means all 7 steps were
/// shown.
///
/// # Errors
/// Propagates any I/O error from reading the input stream or writing the output.
pub fn run<R: std::io::BufRead, W: std::io::Write>(mut r: R, mut w: W) -> std::io::Result<()> {
    for st in steps() {
        writeln!(w, "-- {} --", st.title)?;
        writeln!(w, "{}", st.body)?;
        writeln!(w, "(press Enter)")?;
        let mut buf = String::new();
        r.read_line(&mut buf)?;
    }
    Ok(())
}
