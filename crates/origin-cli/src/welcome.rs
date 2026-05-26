//! Post-init walkthrough: Toolbox → Skill Repository → port skills → Workflows.
//!
//! Runs after `init.rs` has saved `~/.origin/config.toml`. Each screen is a
//! short explainer + an interaction:
//!
//! 1. **Toolbox** — list the built-in tool registry so the user sees
//!    everything the agent can do out of the box; press Enter to continue.
//! 2. **Skill Repository** — explain skills and the path, mention the
//!    tool→skill validation step; press Enter to continue.
//! 3. **Port skills** — Y/N prompt; on yes, scan well-known harness
//!    directories (`~/.claude/skills/`, `~/.config/opencode/skills/`,
//!    `~/.kilocode/skills/`, `~/.config/kilocode/skills/`) and copy any
//!    skills that aren't already present in `~/.origin/skills/` (dedup
//!    by `SkillHash`). Validates each imported skill's `allowed-tools`
//!    against the Toolbox so missing dependencies surface immediately.
//!    LLM-driven discovery is intentionally deferred: the daemon is not
//!    guaranteed to be running during init, and origin-cli has no direct
//!    chat surface. The user is told the agent can find further skills
//!    once they start chatting.
//! 4. **Workflows** — explain skill chaining; seed
//!    `~/.origin/workflows.toml` with one example if the file doesn't
//!    already exist; press Enter to finish.
//!
//! Modeled on `tutorial.rs` and `init.rs`: a runner driven by
//! [`std::io::BufRead`] + [`std::io::Write`] for unit-testable scripted
//! runs, plus a `run()` wrapper for stdin/stdout.

use crate::workflows;
use anyhow::{anyhow, Result};
use origin_skills::{first_run_import, load_skills_dir, ImportDecision, ImportReport, Skill};
use origin_tools::{registry_iter, ToolMeta};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Built-in scan locations, relative to `$HOME`. Order is the order they
/// surface to the user.
pub const KNOWN_HARNESS_SOURCES: &[(&str, &str)] = &[
    (".claude/skills", "Claude Code"),
    (".config/opencode/skills", "Opencode"),
    (".kilocode/skills", "Kilocode"),
    (".config/kilocode/skills", "Kilocode (config)"),
];

/// Entry point used by `init::run`. Reads from stdin, writes to stdout,
/// scans `$HOME` (or `$ORIGIN_HOME`) for the known source dirs, and
/// targets `~/.origin/skills/` + `~/.origin/workflows.toml`.
///
/// # Errors
/// Returns an error if the home directory cannot be resolved or any of the
/// interactive screens or import steps fails.
pub fn run() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow!("home directory not found"))?;
    let skills_dst = home.join(".origin").join("skills");
    let workflows_path = home.join(".origin").join("workflows.toml");
    let sources: Vec<(PathBuf, String)> = KNOWN_HARNESS_SOURCES
        .iter()
        .map(|(rel, label)| (home.join(rel), (*label).to_string()))
        .collect();
    run_with(stdin.lock(), stdout.lock(), &sources, &skills_dst, &workflows_path)
}

/// Drive every screen with explicit paths so tests don't need to mutate
/// `$ORIGIN_HOME` (Rust 1.83 flags `set_var` as `unsafe`).
///
/// `sources` is the ordered list of `(source_dir, label_for_user)` pairs.
/// `skills_dst` is the merge target (typically `~/.origin/skills/`).
/// `workflows_path` is where the example workflow seed lands.
///
/// # Errors
/// Returns an I/O error from `r`/`w` or propagates failures from the
/// underlying screen handlers (skill import, workflow seeding, etc.).
pub fn run_with<R: BufRead, W: Write>(
    mut r: R,
    mut w: W,
    sources: &[(PathBuf, String)],
    skills_dst: &Path,
    workflows_path: &Path,
) -> Result<()> {
    screen_toolbox(&mut r, &mut w)?;
    screen_skill_repository(&mut r, &mut w, skills_dst)?;
    if yes_no(
        &mut r,
        &mut w,
        "Port skills now? [Y/n]: ",
        true, // default yes — the screen text explicitly invited them
    )? {
        screen_port_skills(&mut w, sources, skills_dst)?;
    } else {
        writeln!(
            &mut w,
            "  Skipped. You can drop skills into {} any time, or ask the \
             agent to find them once you start chatting.",
            skills_dst.display()
        )?;
    }
    screen_workflows(&mut r, &mut w, workflows_path)?;

    // Seed a first-run discovery prompt so the agent can find skills in
    // non-standard locations on its first chat. `origin-cli` can't drive
    // the LLM during init (daemon isn't running), so we queue the work
    // here and let `main.rs` auto-fire it on next TUI start.
    let pending = workflows_path
        .parent()
        .map(|p| p.join("pending-prompt.txt"));
    if let Some(p) = pending {
        if let Err(e) = crate::first_run_prompt::seed_to(&p) {
            writeln!(&mut w, "warning: could not seed first-run prompt: {e}")?;
        } else {
            writeln!(
                &mut w,
                "\nQueued a first-chat discovery prompt at {}.\n\
                 The agent will run it the next time you launch origin.",
                p.display()
            )?;
        }
    }

    writeln!(
        &mut w,
        "\n  {} {}",
        crate::ansi::green("\u{2714}"),
        crate::ansi::bright("Setup complete."),
    )?;
    writeln!(
        &mut w,
        "  {}",
        crate::ansi::muted("`origin --help` lists every subcommand."),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Screen 1: Toolbox
// ---------------------------------------------------------------------------

fn screen_toolbox<R: BufRead, W: Write>(r: &mut R, w: &mut W) -> Result<()> {
    use crate::ansi;
    writeln!(w)?;
    writeln!(w, "  {}  {}", ansi::step_number(1, 4), ansi::heading("toolbox"))?;
    writeln!(w)?;
    let tools: Vec<&'static ToolMeta> = registry_iter().collect();
    writeln!(
        w,
        "  {} {} tools available:",
        ansi::muted("Your agent has"),
        ansi::accent(&tools.len().to_string()),
    )?;
    writeln!(w)?;
    for t in &tools {
        let desc = truncate(t.description, 60);
        writeln!(w, "    {}  {}", ansi::accent(&format!("{:<16}", t.name)), ansi::muted(&desc))?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "  {}",
        ansi::muted("Skills are validated against this toolbox on import.")
    )?;
    press_enter(r, w, "  Press Enter to continue.")
}

// ---------------------------------------------------------------------------
// Screen 2: Skill Repository
// ---------------------------------------------------------------------------

fn screen_skill_repository<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    skills_dst: &Path,
) -> Result<()> {
    use crate::ansi;
    writeln!(w)?;
    writeln!(w, "  {}  {}", ansi::step_number(2, 4), ansi::heading("skills"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {}",
        ansi::muted("Markdown files with YAML frontmatter that teach the agent procedures.")
    )?;
    writeln!(w, "  {}  {}", ansi::muted("Repository:"), ansi::accent(&skills_dst.display().to_string()))?;

    let existing = count_skills(skills_dst);
    if existing > 0 {
        writeln!(w, "  {}",
            ansi::muted(&format!("{existing} skill(s) already installed.")))?;
    }

    writeln!(w)?;
    writeln!(
        w,
        "  {}",
        ansi::muted("Drop SKILL.md files into subdirectories. Origin deduplicates by hash.")
    )?;
    press_enter(r, w, "  Press Enter to continue.")
}

// ---------------------------------------------------------------------------
// Screen 3: Port skills
// ---------------------------------------------------------------------------

fn screen_port_skills<W: Write>(
    w: &mut W,
    sources: &[(PathBuf, String)],
    skills_dst: &Path,
) -> Result<()> {
    use crate::ansi;
    writeln!(w)?;
    writeln!(w, "  {}  {}", ansi::step_number(3, 4), ansi::heading("import skills"))?;
    writeln!(w)?;
    for (path, label) in sources {
        writeln!(w, "    {}  {}", ansi::accent(&format!("{label:<18}")), ansi::muted(&path.display().to_string()))?;
    }
    writeln!(w)?;

    let toolbox: std::collections::HashSet<String> =
        registry_iter().map(|t| t.name.to_string()).collect();

    let mut totals = ImportReport::default();
    let mut all_imported: Vec<Skill> = Vec::new();
    for (src, label) in sources {
        if !src.exists() {
            writeln!(w, "  {label:<20} (not present, skipping)")?;
            continue;
        }
        match first_run_import(src, skills_dst, |_| ImportDecision::Accept) {
            Ok(report) => {
                writeln!(
                    w,
                    "  {label:<20} imported={} duplicates={} rejected={}",
                    report.imported, report.skipped_duplicate, report.rejected,
                )?;
                totals.imported = totals.imported.saturating_add(report.imported);
                totals.skipped_duplicate =
                    totals.skipped_duplicate.saturating_add(report.skipped_duplicate);
                totals.rejected = totals.rejected.saturating_add(report.rejected);
            }
            Err(e) => {
                writeln!(w, "  {label:<20} error: {e}")?;
            }
        }
    }

    // Re-load the destination so we can validate allowed-tools against
    // the Toolbox. `load_skills_dir` returns every skill at the destination,
    // not just the freshly imported ones — that's fine; we just iterate.
    if skills_dst.exists() {
        if let Ok(skills) = load_skills_dir(skills_dst) {
            all_imported = skills;
        }
    }

    writeln!(
        w,
        "\nTotal: {} imported, {} duplicates skipped, {} rejected.",
        totals.imported, totals.skipped_duplicate, totals.rejected,
    )?;

    // Tool-availability check: for each imported skill, every tool listed in
    // `allowed-tools:` must exist in the registry; missing tools surface as
    // warnings so the user knows what they need (typically an MCP server).
    let mut missing: Vec<(String, Vec<String>)> = Vec::new();
    for s in &all_imported {
        let gaps: Vec<String> = s
            .front
            .allowed_tools
            .iter()
            .filter(|t| !toolbox.contains(*t))
            .cloned()
            .collect();
        if !gaps.is_empty() {
            missing.push((s.front.name.clone(), gaps));
        }
    }
    if missing.is_empty() && !all_imported.is_empty() {
        writeln!(
            w,
            "All {} skill(s) declare only tools available in your Toolbox.",
            all_imported.len()
        )?;
    } else if !missing.is_empty() {
        writeln!(w, "\nSkills with tools not yet in your Toolbox:")?;
        for (skill, gaps) in &missing {
            writeln!(w, "  - {skill}: {}", gaps.join(", "))?;
        }
        writeln!(
            w,
            "  (These are typically MCP-served tools. Wire them in via \
             `~/.origin/mcp.toml` or the skill will silently downgrade.)"
        )?;
    }

    writeln!(
        w,
        "\nWhen you start chatting, the agent can discover further skills \
         in non-standard locations and offer to add them."
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Screen 4: Workflows
// ---------------------------------------------------------------------------

fn screen_workflows<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    workflows_path: &Path,
) -> Result<()> {
    use crate::ansi;
    writeln!(w)?;
    writeln!(w, "  {}  {}", ansi::step_number(4, 4), ansi::heading("workflows"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {}",
        ansi::muted("Chain skills into sequences. Each step names a skill + optional args.")
    )?;
    writeln!(w)?;
    writeln!(w, "    {}", ansi::muted("[[workflows]]"))?;
    writeln!(w, "    {}", ansi::muted("name = \"frontend-design\""))?;
    writeln!(w, "    {}", ansi::muted("steps = ["))?;
    writeln!(w, "      {}", ansi::muted("{ skill = \"frontend-design:frontend-design\" },"))?;
    writeln!(w, "      {}", ansi::muted("{ skill = \"impeccable\", args = \"teach\" },"))?;
    writeln!(w, "    {}", ansi::muted("]"))?;
    writeln!(w)?;

    match workflows::seed_if_missing(workflows_path) {
        Ok(true) => writeln!(w, "  {} Seeded {}", ansi::green("\u{2714}"), ansi::muted(&workflows_path.display().to_string()))?,
        Ok(false) => writeln!(
            w,
            "  {} {}",
            ansi::muted("\u{2500}"),
            ansi::muted(&format!("{} already exists", workflows_path.display())),
        )?,
        Err(e) => writeln!(w, "  {} {}", ansi::red("\u{2718}"), ansi::red(&format!("could not seed: {e}")))?,
    }

    press_enter(r, w, "  Press Enter to finish setup.")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count the skills physically present in `dir`. Best-effort — returns 0
/// on any read error so the welcome screen never aborts on permission
/// glitches.
fn count_skills(dir: &Path) -> usize {
    load_skills_dir(dir).map(|v| v.len()).unwrap_or(0)
}

fn press_enter<R: BufRead, W: Write>(r: &mut R, w: &mut W, prompt: &str) -> Result<()> {
    writeln!(w, "\n{prompt}")?;
    w.flush()?;
    let mut buf = String::new();
    // EOF here is non-fatal — for piped scripts the test harness simulates
    // Enter by sending `\n`, but if the user closes stdin we still want to
    // continue rather than abort the rest of the walkthrough.
    let _ = r.read_line(&mut buf);
    Ok(())
}

fn yes_no<R: BufRead, W: Write>(
    r: &mut R,
    w: &mut W,
    prompt: &str,
    default_yes: bool,
) -> Result<bool> {
    loop {
        write!(w, "{prompt}")?;
        w.flush()?;
        let mut buf = String::new();
        let n = r.read_line(&mut buf).map_err(|e| anyhow!("read stdin: {e}"))?;
        if n == 0 {
            return Ok(default_yes);
        }
        match buf.trim() {
            "" => return Ok(default_yes),
            s if s.starts_with('y') || s.starts_with('Y') => return Ok(true),
            s if s.starts_with('n') || s.starts_with('N') => return Ok(false),
            _ => writeln!(w, "  (please answer y or n)")?,
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    // Char-boundary-safe truncate. For the Toolbox listing this is fine
    // because descriptions are ASCII; the boundary check is defensive.
    if s.len() <= max {
        return s;
    }
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a single fake SKILL.md under `src_dir/skill_name/`. Returns
    /// the directory so tests can assert against it.
    fn write_fake_skill(src_dir: &Path, name: &str, allowed_tools: &[&str]) -> PathBuf {
        let skill_dir = src_dir.join(name);
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        let allowed = allowed_tools
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let body = format!(
            "---\nname: {name}\ndescription: A fake skill for tests\nallowed-tools: [{allowed}]\n---\nbody of {name}\n"
        );
        std::fs::write(skill_dir.join("SKILL.md"), body).expect("write SKILL.md");
        skill_dir
    }

    #[test]
    fn truncate_ascii_under_and_over() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcde");
    }

    #[test]
    fn yes_no_default_yes_on_empty() {
        let input = std::io::Cursor::new(b"\n".as_slice());
        let mut out: Vec<u8> = Vec::new();
        assert!(yes_no(&mut std::io::BufReader::new(input), &mut out, "x? ", true).expect("ok"));
    }

    #[test]
    fn yes_no_treats_eof_as_default() {
        // Empty cursor → read_line returns 0 → fall through to default.
        let input = std::io::Cursor::new(Vec::<u8>::new());
        let mut out: Vec<u8> = Vec::new();
        assert!(!yes_no(&mut std::io::BufReader::new(input), &mut out, "x? ", false).expect("ok"));
    }

    #[test]
    fn full_walkthrough_with_port_yes() {
        // Set up: a Claude-style source dir with one skill that uses Read
        // (a real Toolbox tool), and an Opencode source with a skill that
        // declares an unknown tool (so we exercise the missing-tools warning).
        let dir = tempfile::tempdir().expect("tempdir");
        let claude_src = dir.path().join("claude").join("skills");
        let opencode_src = dir.path().join("opencode").join("skills");
        write_fake_skill(&claude_src, "real-tool-skill", &["Read", "Glob"]);
        write_fake_skill(&opencode_src, "fake-tool-skill", &["NotARealTool"]);
        let skills_dst = dir.path().join(".origin").join("skills");
        let workflows_path = dir.path().join(".origin").join("workflows.toml");

        let sources: Vec<(PathBuf, String)> = vec![
            (claude_src, "Claude Code".into()),
            (opencode_src, "Opencode".into()),
        ];

        // Script: Enter (toolbox) -> Enter (skill repo) -> y (port) ->
        // Enter (workflows).
        let script = b"\n\ny\n\n";
        let input = std::io::Cursor::new(script.to_vec());
        let mut output: Vec<u8> = Vec::new();
        run_with(input, &mut output, &sources, &skills_dst, &workflows_path).expect("run_with");

        // Both skills landed at the destination.
        assert!(skills_dst.join("real-tool-skill").join("SKILL.md").exists());
        assert!(skills_dst.join("fake-tool-skill").join("SKILL.md").exists());
        // Workflows seed file exists.
        assert!(workflows_path.exists());
        // Output mentions the missing tool.
        let out = String::from_utf8(output).expect("utf8");
        assert!(
            out.contains("NotARealTool"),
            "missing-tool warning not surfaced:\n{out}"
        );
        assert!(out.contains("frontend-design"), "workflows screen not shown:\n{out}");
    }

    #[test]
    fn decline_port_skips_import_but_still_seeds_workflows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let claude_src = dir.path().join("claude").join("skills");
        write_fake_skill(&claude_src, "skip-me", &["Read"]);
        let skills_dst = dir.path().join(".origin").join("skills");
        let workflows_path = dir.path().join(".origin").join("workflows.toml");

        let sources: Vec<(PathBuf, String)> = vec![(claude_src, "Claude Code".into())];

        // Script: Enter -> Enter -> n (decline port) -> Enter.
        let script = b"\n\nn\n\n";
        let input = std::io::Cursor::new(script.to_vec());
        let mut output: Vec<u8> = Vec::new();
        run_with(input, &mut output, &sources, &skills_dst, &workflows_path).expect("run_with");

        // No copy happened.
        assert!(!skills_dst.join("skip-me").join("SKILL.md").exists());
        // Workflows still seeded.
        assert!(workflows_path.exists());
    }

    #[test]
    fn port_with_no_source_dirs_present_is_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dst = dir.path().join(".origin").join("skills");
        let workflows_path = dir.path().join(".origin").join("workflows.toml");
        // Sources point at paths that don't exist.
        let sources: Vec<(PathBuf, String)> = vec![
            (dir.path().join("nope-claude"), "Claude Code".into()),
            (dir.path().join("nope-opencode"), "Opencode".into()),
        ];
        let script = b"\n\ny\n\n";
        let input = std::io::Cursor::new(script.to_vec());
        let mut output: Vec<u8> = Vec::new();
        run_with(input, &mut output, &sources, &skills_dst, &workflows_path).expect("run_with");

        let out = String::from_utf8(output).expect("utf8");
        assert!(out.contains("not present"));
        assert!(out.contains("0 imported"));
    }

    #[test]
    fn toolbox_screen_lists_at_least_one_builtin() {
        // Just confirms the toolbox iterator yields entries — guards against
        // a future refactor that breaks the `inventory` collection path
        // and silently makes the screen blank.
        let dir = tempfile::tempdir().expect("tempdir");
        let skills_dst = dir.path().join("skills");
        let workflows_path = dir.path().join("wf.toml");

        // Script: 4 Enters/responses through all four screens.
        let script = b"\n\nn\n\n";
        let input = std::io::Cursor::new(script.to_vec());
        let mut output: Vec<u8> = Vec::new();
        run_with(input, &mut output, &[], &skills_dst, &workflows_path).expect("run_with");

        let out = String::from_utf8(output).expect("utf8");
        // Read is a known compile-time builtin (see crates/origin-tools/src/builtins/read.rs).
        assert!(out.contains("Read"), "Toolbox listing missing `Read`:\n{out}");
    }
}
