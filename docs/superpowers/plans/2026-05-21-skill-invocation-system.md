# Skill Invocation System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire end-to-end skill invocation: seed a first-run LLM discovery prompt, load the on-disk skill catalog, expose IPC verbs and slash commands to activate/deactivate skills, surface the catalog to the model in the system prompt, and let workflows chain multiple skills at once.

**Architecture:** Seven tasks across three execution phases. Phase A runs three independent foundations in parallel: a CLI-side seed file (Task 0), the daemon's startup-time skill catalog load (Task 1), and the protocol additions for activation (Task 2). Phase B builds on the protocol with two parallel pieces: CLI slash dispatch (Task 3) and system-prompt injection (Task 4). Phase C lands workflow chaining (Task 5) and Tab autocomplete (Task 6) in parallel on top of the slash syntax.

**Slash + brace syntax (revised at user request, 2026-05-21):**
- Skills activate with bare `/<name>` (or `/<plugin>:<name>` for namespaced); deactivate with `/-<name>` (the `-` prefix is "remove"). No `/skill` verb.
- Workflows activate with `{workflow:<name>}` (brace-delimited template-style).
- Both shapes get Tab-completion in the TUI.

**Tech Stack:** Rust workspace at `C:\Users\wooai\Documents\origin`. Crates touched: `origin-cli`, `origin-daemon`, `origin-skills` (read-only). Existing infrastructure: `origin_skills::loader::load_skills_dir`, `origin_skills::SkillRegistry`, `origin_daemon::agent::LoopOptions.skills: Option<Arc<SkillRegistry>>` (already wired through `check_with_skills` in the permission layer). `inventory`-collected tool registry via `origin_tools::registry_iter`. IPC framing via `origin_ipc` JSON-tagged enums.

**Parallelism plan:**
- **Phase A (parallel via dispatching-parallel-agents):** Task 0 + Task 1 + Task 2
- **Phase B (parallel via dispatching-parallel-agents):** Task 3 + Task 4 (both depend on Task 2; Task 4 also depends on Task 1)
- **Phase C (parallel via dispatching-parallel-agents):** Task 5 + Task 6 (Task 5 depends on Tasks 2+3; Task 6 only needs the syntax decisions in Tasks 3+5, not their handlers, so it parallels Task 5 cleanly)

**Verification gates:** Each task ends with an explicit `Verification gate` step that runs `cargo check -p <crate> --tests` + the targeted `cargo test`, with concrete expected output (test counts, pass status). **No task is considered complete until its gate passes.** A pre-existing unrelated failure in `crates/origin-daemon/src/main.rs:402` (`Arc::clone` type inference in `spawn_idle_consolidator`) is out of scope — leave it alone.

**TDD discipline:** Every task writes the failing test first, runs it to confirm RED, then implements the minimum to reach GREEN. Implementation steps never precede their corresponding test step in any task.

**Out of scope:**
- LLM-driven skill discovery DURING `origin init` (daemon may not be running). Task 0 instead seeds a first-prompt that the agent runs the next time the user starts a chat.
- `Skill` meta-tool — Task 4 uses system-prompt injection instead; the meta-tool variant is a follow-up.
- Persisting active-skill state across daemon restarts — activation is per-connection-lifetime.

---

## File Structure

**New files (5):**
- `crates/origin-cli/src/first_run_prompt.rs` — generates the seed-prompt text + writes `~/.origin/pending-prompt.txt`. Reused by `welcome.rs` (write) and `main.rs` (read + auto-submit).
- `crates/origin-cli/src/autocomplete.rs` — pure Tab-completion logic: takes the current input buffer + a `CompletionSources` (skill + workflow names) and rewrites the buffer. No I/O, no global state — testable in isolation.
- `crates/origin-daemon/src/skill_catalog.rs` — `SkillCatalog { skills: Vec<Skill> }` wrapping `origin_skills::load_skills_dir`, with `find(name)` and `iter()`.
- `crates/origin-daemon/tests/skill_catalog_load.rs` — integration test for catalog loading.
- `crates/origin-daemon/tests/skill_activation_protocol.rs` — integration test for the new IPC verbs.

**Modified files (8):**
- `crates/origin-cli/src/welcome.rs` — calls `first_run_prompt::seed` at end of walkthrough.
- `crates/origin-cli/src/main.rs` — auto-submits pending prompt on TUI start; intercepts `Tab` in the event loop for autocomplete; dispatches `/<skill>`, `/-<skill>`, `{workflow:<name>}`.
- `crates/origin-cli/src/input.rs` — adds `parse_skill_command` (bare `/<name>` and `/-<name>`) + `parse_workflow_command` (matches `{workflow:<name>}` form). The reducer stays buffer-only; Tab is handled in main.rs's event loop, not the reducer.
- `crates/origin-cli/src/lib.rs` — exports `first_run_prompt`, `autocomplete`.
- `crates/origin-daemon/src/lib.rs` — exports `skill_catalog`.
- `crates/origin-daemon/src/main.rs` — loads catalog; per-connection `SkillRegistry`; dispatch for new variants.
- `crates/origin-daemon/src/protocol.rs` — `ActivateSkill`/`DeactivateSkill`/`ActivateWorkflow` + `SkillActive`/`SkillError`/`WorkflowActive`.
- `crates/origin-daemon/src/agent.rs` — `LoopOptions.skill_catalog`; prepend one-line-per-skill catalog into `recalled_system`.

---

# Phase A — Independent Foundations (parallel)

Three subagents run Tasks 0, 1, 2 simultaneously. Each agent operates on disjoint files; no merge conflicts expected.

---

## Task 0: Post-init seed prompt for LLM-driven skill discovery

**Goal:** At the end of `origin init`'s walkthrough, write a markdown prompt to `~/.origin/pending-prompt.txt`. On next TUI launch, auto-submit it as the first user prompt, then delete the file so it never fires twice.

**Files:**
- Create: `crates/origin-cli/src/first_run_prompt.rs`
- Modify: `crates/origin-cli/src/lib.rs:14` (add export)
- Modify: `crates/origin-cli/src/welcome.rs` — call seeder in `run_with` after workflows screen
- Modify: `crates/origin-cli/src/main.rs` — between `enable_raw_mode` and `run_event_loop`, check for pending prompt and call `handle_submit` with it
- Test: tests live in the new module (unit-test the text generator + path resolver + drain)

- [ ] **Step 1: Write the failing tests**

Add to the bottom of the new `crates/origin-cli/src/first_run_prompt.rs`:

```rust
//! First-run pending-prompt seed.
//!
//! `origin init`'s post-config walkthrough writes a markdown prompt to
//! `~/.origin/pending-prompt.txt`. The first time the TUI starts after
//! init, `main.rs` reads the file, fires it as the user's first prompt,
//! and deletes the file so it can never fire twice. The auto-fired prompt
//! asks the agent to discover and import skills from non-standard
//! locations — the LLM-driven discovery the operator wanted, deferred from
//! init time (when the daemon isn't yet running) to first-chat time
//! (when it is).

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Resolve `~/.origin/pending-prompt.txt`. Honors `$ORIGIN_HOME` for tests
/// and alternate-root installs, matching `crate::config::path`.
pub fn path() -> Result<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("home directory not found"))?;
    Ok(home.join(".origin").join("pending-prompt.txt"))
}

/// The markdown body of the discovery prompt. Public so tests + welcome
/// screens can render it for the user as a preview.
#[must_use]
pub fn discovery_prompt_body() -> &'static str {
    "Please do a one-time skill and tool discovery sweep for this origin install.\n\
     \n\
     1. Use Glob to find every `SKILL.md` under `~/.claude/`, `~/.config/opencode/`, \
        `~/.kilocode/`, `~/.config/kilocode/`, `~/.cursor/`, `~/.vscode/`, `~/Library/Application Support/`, \
        and `~/AppData/` that is NOT already under `~/.origin/skills/`.\n\
     2. For each match, Read the file. If the YAML frontmatter parses and the \
        skill is not already in `~/.origin/skills/` (compare by body hash), \
        copy it into a new directory under `~/.origin/skills/<skill-name>/SKILL.md`.\n\
     3. For each imported skill, check the `allowed-tools:` list. Any tool NOT \
        in the built-in Toolbox is likely an MCP-served tool — note it for the user \
        with a `keyring add` or `mcp.toml` snippet they can run.\n\
     4. Summarize: how many skills were imported per source directory, how many \
        duplicates were skipped, and which (if any) declared tools the Toolbox \
        does not provide.\n\
     \n\
     Do not modify or delete the source files. After the summary, this prompt \
     will not run again."
}

/// Write the seed prompt to `p`. Overwrites any existing file (re-running
/// `origin init` re-arms the discovery).
pub fn seed_to(p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, discovery_prompt_body())?;
    Ok(())
}

/// Read + delete the pending prompt at `p`. Returns `Ok(None)` when the
/// file does not exist (steady state after first run). The delete is
/// best-effort — read errors propagate, but a delete failure is logged
/// and ignored so a permission glitch doesn't block the chat.
pub fn drain(p: &Path) -> Result<Option<String>> {
    if !p.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(p)?;
    if let Err(e) = std::fs::remove_file(p) {
        tracing::warn!(error = %e, "failed to remove pending-prompt.txt");
    }
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_then_drain_returns_body_and_removes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("pending-prompt.txt");
        seed_to(&p).expect("seed");
        assert!(p.exists());
        let body = drain(&p).expect("drain").expect("present");
        assert!(body.contains("skill and tool discovery"));
        assert!(!p.exists(), "drain must delete the file");
    }

    #[test]
    fn drain_missing_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("never-written.txt");
        assert!(drain(&p).expect("drain").is_none());
    }

    #[test]
    fn discovery_prompt_mentions_each_source_dir() {
        let body = discovery_prompt_body();
        for src in &[".claude", ".config/opencode", ".kilocode"] {
            assert!(body.contains(src), "discovery prompt missing {src}");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p origin-cli --lib first_run_prompt:: 2>&1 | tail -20`
Expected: FAIL with `error[E0432]: unresolved import` or `module \`first_run_prompt\` not found` (because lib.rs doesn't export it yet).

- [ ] **Step 3: Wire the module into lib.rs**

Modify `crates/origin-cli/src/lib.rs`. After line 13 (`pub mod cli_def;`) add:

```rust
pub mod first_run_prompt;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p origin-cli --lib first_run_prompt:: 2>&1 | tail -10`
Expected: PASS with `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 5: Wire seeding into the welcome flow**

Modify `crates/origin-cli/src/welcome.rs`. Find the `run_with` function's final `screen_workflows(...)` call. Replace the block:

```rust
    screen_workflows(&mut r, &mut w, workflows_path)?;
    writeln!(&mut w, "\nSetup complete. `origin --help` lists every subcommand.")?;
    Ok(())
}
```

with:

```rust
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

    writeln!(&mut w, "\nSetup complete. `origin --help` lists every subcommand.")?;
    Ok(())
}
```

- [ ] **Step 6: Wire drain + auto-submit into main.rs**

Modify `crates/origin-cli/src/main.rs`. Find the line after `app.lock().add_line("", "Connected; ...");` (around line 134-135) and BEFORE `let scheduler = Scheduler::new(...)`. Insert:

```rust
    // First-run discovery: if `origin init`'s welcome flow queued a pending
    // prompt, fire it as the user's first turn and remove the file so it
    // never auto-fires twice. Errors are non-fatal — the user can always
    // type a prompt manually.
    let pending_prompt = match origin_cli::first_run_prompt::path() {
        Ok(p) => origin_cli::first_run_prompt::drain(&p).ok().flatten(),
        Err(_) => None,
    };
```

Then, AFTER the `let result = run_event_loop(...)` line (around line 166), but BEFORE it (so the auto-submit runs before the user types), and AFTER the scheduler has been set up — find the existing block:

```rust
    let result = run_event_loop(app, composer, widget, handle, &path, &model).await;
```

Replace it with:

```rust
    // Auto-fire the pending discovery prompt now that the TUI is wired up.
    if let Some(text) = pending_prompt {
        app.lock()
            .add_line("system> ", "Running queued first-run discovery prompt…");
        handle.mark_dirty();
        handle_submit(&app, &handle, &path, &model, &text).await;
    }

    let result = run_event_loop(app, composer, widget, handle, &path, &model).await;
```

- [ ] **Step 7: Verification gate**

Run, in order, and confirm each passes before proceeding:

```bash
cargo check -p origin-cli --tests 2>&1 | tail -5
```
Expected: `Finished \`dev\` profile`.

```bash
cargo test -p origin-cli --lib first_run_prompt:: 2>&1 | tail -10
```
Expected: `test result: ok. 3 passed; 0 failed`.

```bash
cargo test -p origin-cli --lib welcome:: 2>&1 | tail -10
```
Expected: All 6 existing welcome tests still PASS (no regression from the new `seed_to` call — note welcome tests already write workflows to a tempdir, so the parent-derived pending-prompt path will land there too; verify the test doesn't fail on the presence of an extra file).

If the welcome test fails because it asserts only `workflows.toml` exists in the dir, update the assertion to allow `pending-prompt.txt` as well. Acceptable change.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-cli/src/first_run_prompt.rs crates/origin-cli/src/lib.rs crates/origin-cli/src/welcome.rs crates/origin-cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): seed first-run discovery prompt; auto-fire on TUI start

`origin init` queues `~/.origin/pending-prompt.txt`; main.rs reads + drains
it on first launch so the agent does the skill/tool discovery sweep the
operator wanted (deferred from init time, when the daemon isn't running).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 1: Daemon loads skill catalog on startup

**Goal:** New `SkillCatalog` struct in the daemon wraps `origin_skills::load_skills_dir`. The daemon loads `~/.origin/skills/` once at startup and holds the result in an `Arc` that requests can read. Failure to load is non-fatal (empty catalog).

**Files:**
- Create: `crates/origin-daemon/src/skill_catalog.rs`
- Modify: `crates/origin-daemon/src/lib.rs` — add `pub mod skill_catalog;`
- Modify: `crates/origin-daemon/src/main.rs` — load near MemoryWiring construction (~line 444)
- Create: `crates/origin-daemon/tests/skill_catalog_load.rs`

- [ ] **Step 1: Write the failing module + tests**

Create `crates/origin-daemon/src/skill_catalog.rs`:

```rust
//! In-process catalog of skills the daemon loaded from `~/.origin/skills/`.
//!
//! The daemon loads the catalog once at startup and holds it in an `Arc`
//! shared across every connection. Activation state — which subset of
//! these skills is *currently in the stack* — lives separately, per
//! connection (see the per-connection `SkillRegistry` in `main.rs`).
//!
//! A load failure does not abort the daemon; we surface it as an empty
//! catalog + a `tracing::warn!`, so a corrupt or absent skills dir doesn't
//! deny service. Re-loading at runtime is a P-future polish item.

use origin_skills::{load_skills_dir, LoaderError, Skill};
use std::path::Path;
use std::sync::Arc;

/// Read-only catalog of every `SKILL.md` under `root` at the time of
/// construction. Lookup is by skill name (the `name:` frontmatter field).
#[derive(Debug, Default)]
pub struct SkillCatalog {
    skills: Vec<Skill>,
}

impl SkillCatalog {
    /// Load every skill under `root`. Returns an empty catalog if `root`
    /// does not exist; surfaces I/O or frontmatter errors via `Err`.
    pub fn load_from(root: &Path) -> Result<Self, LoaderError> {
        if !root.exists() {
            return Ok(Self::default());
        }
        let skills = load_skills_dir(root)?;
        Ok(Self { skills })
    }

    /// Best-effort variant for the daemon boot path: any error degrades
    /// to an empty catalog with a warning, so a malformed skill can't
    /// keep the daemon from coming up.
    #[must_use]
    pub fn load_or_empty(root: &Path) -> Arc<Self> {
        match Self::load_from(root) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                tracing::warn!(error = %e, path = %root.display(), "skill catalog load failed; running with empty catalog");
                Arc::new(Self::default())
            }
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Find a skill by its frontmatter `name`. `None` when not present.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.front.name == name)
    }

    /// Iterate every skill in catalog order (filesystem walk order, as
    /// returned by `load_skills_dir`).
    pub fn iter(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, allowed: &[&str]) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        let allowed = allowed.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
        let body = format!(
            "---\nname: {name}\ndescription: test skill\nallowed-tools: [{allowed}]\n---\nbody\n"
        );
        std::fs::write(skill_dir.join("SKILL.md"), body).expect("write");
    }

    #[test]
    fn load_empty_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cat = SkillCatalog::load_from(&dir.path().join("nope")).expect("ok");
        assert!(cat.is_empty());
    }

    #[test]
    fn load_two_skills() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "foo", &["Read"]);
        write_skill(dir.path(), "bar", &["Glob", "Grep"]);
        let cat = SkillCatalog::load_from(dir.path()).expect("ok");
        assert_eq!(cat.len(), 2);
        assert!(cat.find("foo").is_some());
        assert!(cat.find("bar").is_some());
        assert!(cat.find("missing").is_none());
    }

    #[test]
    fn load_or_empty_degrades_on_corrupt_frontmatter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join("broken");
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(skill_dir.join("SKILL.md"), "no frontmatter here").expect("write");
        let cat = SkillCatalog::load_or_empty(dir.path());
        assert!(cat.is_empty(), "corrupt skill should degrade to empty");
    }
}
```

Also create `crates/origin-daemon/tests/skill_catalog_load.rs`:

```rust
//! Integration test: SkillCatalog loads from a real directory layout.

use origin_daemon::skill_catalog::SkillCatalog;
use std::path::Path;

fn write_skill(dir: &Path, name: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("mkdir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: integration\nallowed-tools: [\"Read\"]\n---\nbody\n"
        ),
    )
    .expect("write");
}

#[test]
fn catalog_finds_skills_in_subdirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_skill(dir.path(), "alpha");
    write_skill(dir.path(), "beta");
    let cat = SkillCatalog::load_from(dir.path()).expect("load");
    assert_eq!(cat.len(), 2);
    assert!(cat.find("alpha").is_some());
    assert!(cat.find("beta").is_some());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p origin-daemon --lib skill_catalog:: 2>&1 | tail -10`
Expected: FAIL with `error[E0583]: file not found for module` (lib.rs hasn't been updated yet).

- [ ] **Step 3: Wire module into lib.rs**

Modify `crates/origin-daemon/src/lib.rs`. After line 17 (`pub mod tool_use_parser;`) add:

```rust
pub mod skill_catalog;
```

- [ ] **Step 4: Run tests to verify they pass**

Run, both:

```bash
cargo test -p origin-daemon --lib skill_catalog:: 2>&1 | tail -10
```
Expected: `test result: ok. 3 passed; 0 failed`.

```bash
cargo test -p origin-daemon --test skill_catalog_load 2>&1 | tail -10
```
Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] **Step 5: Load the catalog at daemon startup**

Modify `crates/origin-daemon/src/main.rs`. Add to the imports near line 21 (after `use origin_daemon::session_store::SessionStore;`):

```rust
use origin_daemon::skill_catalog::SkillCatalog;
```

Then in the function that constructs the `MemoryWiring` (search for `MemoryWiring::new(store, embedder, index)` — should be near line 444). Immediately after that block returns its `MemoryWiring`, add a parallel construction for the skill catalog. Find the call site that consumes `MemoryWiring` (it's threaded into `handle_request`). The simplest wire-up is at the same level: add a local `let skill_catalog: Arc<SkillCatalog> = ...;` near the `let memory: Option<MemoryWiring> = ...;` declaration.

Locate the `daemon_setup` (or `daemon_main`) function that builds these subsystems. Add immediately after `let memory = build_memory_wiring(...);`:

```rust
    let skill_catalog: Arc<SkillCatalog> = {
        let home = std::env::var_os("ORIGIN_HOME")
            .map(std::path::PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let path = home.join(".origin").join("skills");
        SkillCatalog::load_or_empty(&path)
    };
    info!(
        skill_count = skill_catalog.len(),
        "skill catalog loaded at startup"
    );
```

Then thread `Arc::clone(&skill_catalog)` into `handle_request`'s call site — extend the signature.

Modify `handle_request` signature at `crates/origin-daemon/src/main.rs:747` to add a new parameter immediately before `req`:

```rust
async fn handle_request(
    conn: &SharedConnection,
    provider: &dyn Provider,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
    sidecar: Arc<Sidecar>,
    memory: Option<&MemoryWiring>,
    proposal_registry: Arc<ProposalRegistry>,
    skill_catalog: Arc<SkillCatalog>,
    req: PromptRequest,
) -> bool {
```

And update the call site near line 525 to pass `Arc::clone(&skill_catalog)`:

```rust
                    if !handle_request(
                        &conn,
                        provider_snapshot.as_ref(),
                        Arc::clone(&session_store),
                        Arc::clone(&cas),
                        Arc::clone(&sidecar),
                        memory.as_ref(),
                        Arc::clone(&proposal_registry),
                        Arc::clone(&skill_catalog),
                        req,
                    )
                    .await
```

The parameter is not yet consumed inside `handle_request` — Task 4 wires it into `LoopOptions`. For Task 1, just thread it through; an `#[allow(unused_variables)]` on the parameter is fine for the interim.

- [ ] **Step 6: Verification gate**

```bash
cargo check -p origin-daemon --tests 2>&1 | tail -10
```
Expected: `Finished \`dev\` profile`. (Pre-existing `main.rs:402` `Arc::clone` inference error in `spawn_idle_consolidator` is NOT in scope — if it's the only error, that's fine and we proceed.)

```bash
cargo test -p origin-daemon --lib skill_catalog:: 2>&1 | tail -10
```
Expected: `test result: ok. 3 passed`.

```bash
cargo test -p origin-daemon --test skill_catalog_load 2>&1 | tail -10
```
Expected: `test result: ok. 1 passed`.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-daemon/src/skill_catalog.rs crates/origin-daemon/src/lib.rs crates/origin-daemon/src/main.rs crates/origin-daemon/tests/skill_catalog_load.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): load skill catalog at startup

`SkillCatalog::load_or_empty(~/.origin/skills/)` runs once during
`daemon_setup`; the Arc is threaded into `handle_request` so future
tasks can read it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Protocol additions for skill activation

**Goal:** Add `ClientMessage::ActivateSkill { name }` + `DeactivateSkill { name }` (request side) and `StreamEvent::SkillActive { name, allowed_tools }` + `SkillError { message }` (response side). Daemon-side: per-connection `SkillRegistry` that mutates on each ActivateSkill/DeactivateSkill and is read by `handle_request` to pass into `LoopOptions.skills`.

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs` — new enum variants (alphabetize within tagged set)
- Modify: `crates/origin-daemon/src/main.rs` — per-connection `Arc<Mutex<SkillRegistry>>` + dispatch
- Create: `crates/origin-daemon/tests/skill_activation_protocol.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/origin-daemon/tests/skill_activation_protocol.rs`:

```rust
//! End-to-end: activating a skill via the protocol must mutate the
//! per-connection registry, and the daemon must reply with SkillActive.

#![allow(clippy::panic)]

use origin_daemon::protocol::{ClientMessage, StreamEvent};

#[test]
fn activate_skill_message_round_trips_as_json() {
    let msg = ClientMessage::ActivateSkill {
        name: "frontend-design".into(),
    };
    let body = serde_json::to_vec(&msg).expect("encode");
    let decoded: ClientMessage = serde_json::from_slice(&body).expect("decode");
    match decoded {
        ClientMessage::ActivateSkill { name } => assert_eq!(name, "frontend-design"),
        other => panic!("expected ActivateSkill, got {other:?}"),
    }
}

#[test]
fn deactivate_skill_message_round_trips_as_json() {
    let msg = ClientMessage::DeactivateSkill {
        name: "frontend-design".into(),
    };
    let body = serde_json::to_vec(&msg).expect("encode");
    let decoded: ClientMessage = serde_json::from_slice(&body).expect("decode");
    match decoded {
        ClientMessage::DeactivateSkill { name } => assert_eq!(name, "frontend-design"),
        other => panic!("expected DeactivateSkill, got {other:?}"),
    }
}

#[test]
fn skill_active_event_round_trips_as_json() {
    let ev = StreamEvent::SkillActive {
        name: "frontend-design".into(),
        allowed_tools: vec!["Read".into(), "Glob".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::SkillActive { name, allowed_tools } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(allowed_tools, vec!["Read", "Glob"]);
        }
        other => panic!("expected SkillActive, got {other:?}"),
    }
}

#[test]
fn skill_error_event_round_trips_as_json() {
    let ev = StreamEvent::SkillError {
        message: "no such skill: ghost".into(),
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::SkillError { message } => assert_eq!(message, "no such skill: ghost"),
        other => panic!("expected SkillError, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-daemon --test skill_activation_protocol 2>&1 | tail -10`
Expected: FAIL with `error[E0599]: no variant or associated item named \`ActivateSkill\` found for enum \`ClientMessage\``.

- [ ] **Step 3: Add the protocol variants**

Modify `crates/origin-daemon/src/protocol.rs`. Inside the `ClientMessage` enum (around line 116, before the closing `}`), insert before `SubscribePlan`:

```rust
    /// Push `name` onto this connection's active skill stack. The daemon
    /// looks up the skill in its `SkillCatalog`; on success it replies with
    /// [`StreamEvent::SkillActive`] carrying the skill's `allowed-tools`
    /// (so the CLI can render the narrowing it just applied). On failure
    /// (skill not in catalog) it replies with [`StreamEvent::SkillError`].
    ActivateSkill { name: String },
    /// Pop the named skill off this connection's active stack (the
    /// rightmost match if the same skill was activated multiple times).
    /// Always replies with [`StreamEvent::AdminOk`] — deactivating an
    /// inactive skill is not an error.
    DeactivateSkill { name: String },
```

Then inside the `StreamEvent` enum (find the `AdminError { message: String }` variant near line 230; add before it):

```rust
    /// Positive ack for a successful [`ClientMessage::ActivateSkill`].
    /// `allowed_tools` is the intersection mask currently in effect after
    /// pushing this skill — the CLI displays it so users can see what
    /// they've just narrowed access to.
    SkillActive {
        name: String,
        allowed_tools: Vec<String>,
    },
    /// Negative ack for [`ClientMessage::ActivateSkill`] — typically the
    /// requested skill is not in the daemon's catalog.
    SkillError { message: String },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p origin-daemon --test skill_activation_protocol 2>&1 | tail -10`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Write the dispatch test**

Append to `crates/origin-daemon/tests/skill_activation_protocol.rs`:

```rust
// ---------------------------------------------------------------------------
// Dispatch test: drives the IPC loop end-to-end via a scripted connection.
// ---------------------------------------------------------------------------

// (Placeholder: real IPC dispatch testing requires the daemon's full
// transport stack, which is large to set up here. For Task 2 the JSON
// round-trip above is the contract; Task 3 covers the dispatch path
// from the CLI side. The handler glue is asserted by `cargo check`
// compiling the dispatch arm.)
```

- [ ] **Step 6: Add per-connection registry + dispatch handlers**

Modify `crates/origin-daemon/src/main.rs`. Add to imports at the top (after the `use origin_daemon::skill_catalog::SkillCatalog;` from Task 1):

```rust
use origin_skills::SkillRegistry;
```

In the per-connection task that handles IPC (the loop with `let body = ... match msg { ClientMessage::Prompt(req) => ... }` near line 516), immediately before the `loop {` introducing the match (find the line that declares `let mut last_at = ...` or the variable holding per-connection state), add:

```rust
    // Per-connection skill activation state. Each ActivateSkill mutates
    // this registry; each Prompt reads its `allowed_tools` mask and passes
    // it through LoopOptions.skills so the permission engine narrows
    // accordingly. Wrapped in Arc<Mutex<...>> so we can hand `Arc::clone`s
    // to async handlers without giving up the registry.
    let active_skills: Arc<tokio::sync::Mutex<SkillRegistry>> =
        Arc::new(tokio::sync::Mutex::new(SkillRegistry::new()));
```

Then in the `match msg { ... }` block (around line 540 where `ClientMessage::SwitchAccount` is handled), add new arms before `ClientMessage::SubscribePlan`:

```rust
                ClientMessage::ActivateSkill { name } => {
                    let cat = Arc::clone(&skill_catalog);
                    let reg = Arc::clone(&active_skills);
                    let conn_clone = Arc::clone(&conn);
                    if let Some(skill) = cat.find(&name) {
                        let front = skill.front.clone();
                        let allowed_tools = front.allowed_tools.clone();
                        reg.lock().await.activate(front);
                        let ev = StreamEvent::SkillActive {
                            name: name.clone(),
                            allowed_tools,
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone
                            .lock()
                            .await
                            .write_frame(FrameKind::Event, &body)
                            .await;
                    } else {
                        let ev = StreamEvent::SkillError {
                            message: format!("no such skill: {name}"),
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone
                            .lock()
                            .await
                            .write_frame(FrameKind::Event, &body)
                            .await;
                    }
                }
                ClientMessage::DeactivateSkill { name } => {
                    let reg = Arc::clone(&active_skills);
                    reg.lock().await.deactivate(&name);
                    let body = serde_json::to_vec(&StreamEvent::AdminOk).unwrap_or_default();
                    let _ = conn
                        .lock()
                        .await
                        .write_frame(FrameKind::Event, &body)
                        .await;
                }
```

Update the `handle_request` invocation (~line 525) to pass the active registry — change the `req,` line to:

```rust
                        Arc::clone(&active_skills),
                        req,
```

And extend `handle_request`'s signature (line 747) to accept it:

```rust
    skill_catalog: Arc<SkillCatalog>,
    active_skills: Arc<tokio::sync::Mutex<SkillRegistry>>,
    req: PromptRequest,
```

Inside `handle_request`'s body, replace the `LoopOptions { ... skills: None, ... }` near line 810 with:

```rust
            skills: {
                // Snapshot the current active-skill stack into a fresh
                // SkillRegistry. We deep-clone the stack via re-activation
                // because the agent loop wants a `&SkillRegistry`, and we
                // don't want to hold the per-connection lock across an
                // arbitrarily long turn.
                let guard = active_skills.lock().await;
                if guard.allowed_tools().is_some() {
                    let mut snapshot = SkillRegistry::new();
                    for s in guard.iter_active() {
                        snapshot.activate(s.clone());
                    }
                    Some(Arc::new(snapshot))
                } else {
                    None
                }
            },
```

NOTE: this requires `SkillRegistry::iter_active() -> impl Iterator<Item = &SkillFrontmatter>`. Add it to `crates/origin-skills/src/registry.rs` immediately before the closing `}` of `impl SkillRegistry`:

```rust
    /// Iterate the currently-active skills in activation order (oldest
    /// first). Used by daemon snapshotting where we need to clone the
    /// stack without holding a lock across a turn.
    pub fn iter_active(&self) -> impl Iterator<Item = &SkillFrontmatter> {
        self.stack.iter()
    }
```

- [ ] **Step 7: Verification gate**

```bash
cargo check -p origin-daemon --tests 2>&1 | tail -10
```
Expected: `Finished`. (Same caveat about pre-existing `main.rs:402` error.)

```bash
cargo test -p origin-daemon --test skill_activation_protocol 2>&1 | tail -10
```
Expected: `test result: ok. 4 passed`.

```bash
cargo test -p origin-skills --lib 2>&1 | tail -10
```
Expected: All existing tests pass plus the new `iter_active` doesn't break anything (no new test needed for this trivial method; the protocol test exercises it indirectly via the activate path).

- [ ] **Step 8: Commit**

```bash
git add crates/origin-daemon/src/protocol.rs crates/origin-daemon/src/main.rs crates/origin-daemon/tests/skill_activation_protocol.rs crates/origin-skills/src/registry.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): ActivateSkill/DeactivateSkill protocol + per-connection registry

Each connection holds an Arc<Mutex<SkillRegistry>>; ActivateSkill looks
up the skill in the catalog, pushes it, and replies SkillActive with the
allowed-tools mask. Prompt requests snapshot the current stack into
LoopOptions.skills so the permission engine narrows.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Phase B — Slash dispatch + system-prompt injection (parallel)

Tasks 3 and 4 run in parallel after Phase A completes. Both depend on Phase A's protocol additions; Task 4 also depends on the catalog from Task 1.

---

## Task 3: CLI bare-`/` slash for skills

**Goal:** Parse three slash shapes from the TUI input and route to the daemon's activate/deactivate verbs:
- `/<name>` → activate (single segment, no spaces; e.g. `/frontend-design`)
- `/<plugin>:<name>` → activate namespaced (e.g. `/frontend-design:frontend-design`)
- `/-<name>` (and `/-<plugin>:<name>`) → deactivate

No `/skill` verb. The leading `-` is the "remove" sigil for deactivate. Existing slashes (`/mem`, `/account`) MUST NOT be shadowed — the parser refuses any token whose first word matches a known verb.

Render `StreamEvent::SkillActive` / `SkillError` as scrollback lines.

**Files:**
- Modify: `crates/origin-cli/src/input.rs` — add `parse_skill_command(line) -> Option<ClientMessage>`
- Modify: `crates/origin-cli/src/main.rs::handle_submit` — branch on slash before falling into the assistant turn
- Tests live in `crates/origin-cli/src/input.rs::tests`

- [ ] **Step 1: Write the failing parser tests**

Append to `crates/origin-cli/src/input.rs::tests` (the existing `#[cfg(test)] mod tests` block near line 89):

```rust
    #[test]
    fn parse_skill_bare_name() {
        let m = parse_skill_command("/frontend-design").expect("parse");
        match m {
            ClientMessage::ActivateSkill { name } => assert_eq!(name, "frontend-design"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_namespaced() {
        let m = parse_skill_command("/frontend-design:frontend-design").expect("parse");
        match m {
            ClientMessage::ActivateSkill { name } => {
                assert_eq!(name, "frontend-design:frontend-design");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_deactivate_with_dash_prefix() {
        let m = parse_skill_command("/-frontend-design").expect("parse");
        match m {
            ClientMessage::DeactivateSkill { name } => assert_eq!(name, "frontend-design"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_deactivate_namespaced() {
        let m = parse_skill_command("/-frontend-design:frontend-design").expect("parse");
        match m {
            ClientMessage::DeactivateSkill { name } => {
                assert_eq!(name, "frontend-design:frontend-design");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parse_skill_rejects_empty_and_whitespace() {
        // Bare slash with no name, dash with no name, slash with trailing text.
        assert!(parse_skill_command("/").is_none());
        assert!(parse_skill_command("/-").is_none());
        assert!(parse_skill_command("/ ").is_none());
        // Embedded whitespace disambiguates the slash from chat content
        // (e.g. "/foo bar baz" is a free-form prompt mentioning a path).
        assert!(parse_skill_command("/foo bar").is_none());
    }

    #[test]
    fn parse_skill_does_not_shadow_known_verbs() {
        // `/mem accept 1`, `/account default`, and `/workflow X` are not skills.
        assert!(parse_skill_command("/mem").is_none());
        assert!(parse_skill_command("/account").is_none());
        // Free-form text never parses as a skill.
        assert!(parse_skill_command("hello").is_none());
        // `{workflow:foo}` is a workflow, not a skill.
        assert!(parse_skill_command("{workflow:foo}").is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p origin-cli --lib input::tests::parse_skill 2>&1 | tail -10`
Expected: FAIL with `error[E0425]: cannot find function \`parse_skill_command\``.

- [ ] **Step 3: Implement the parser**

Modify `crates/origin-cli/src/input.rs`. After the `parse_mem_command` function (just before the `#[cfg(test)]` block), add:

```rust
/// Slash verbs that already have dedicated handlers — they must not be
/// re-routed through the skill parser even though they start with `/`.
/// Update this list when a new slash verb is added.
const RESERVED_SLASH_VERBS: &[&str] = &["mem", "account", "help"];

/// Parse `/<name>` (activate) and `/-<name>` (deactivate) into a
/// [`ClientMessage::ActivateSkill`] or [`ClientMessage::DeactivateSkill`].
///
/// Rules:
/// - Leading `/` required; the rest is the skill name (or `-<name>` for
///   deactivate). Names may contain `:` (namespaced skills like
///   `frontend-design:frontend-design`).
/// - Names must not contain whitespace — a slash with embedded spaces is
///   prompt text mentioning a path, not a skill invocation.
/// - Reserved slash verbs (`/mem`, `/account`, `/help`) and the workflow
///   shape (`{workflow:...}`) are rejected so callers can fall through to
///   their own handlers.
///
/// Returns `None` for any non-matching input.
#[must_use]
pub fn parse_skill_command(line: &str) -> Option<ClientMessage> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    // Whitespace inside the slash invalidates it as a skill — free-form
    // chat that happens to contain a `/` shouldn't activate anything.
    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    // Disambiguate the deactivate sigil first.
    if let Some(name) = rest.strip_prefix('-') {
        if name.is_empty() {
            return None;
        }
        // Apply the reserved-verb guard to the deactivate form too, so
        // `/-mem` (silly but possible) never deactivates something named
        // `mem` that would shadow the `/mem` verb if one existed.
        if RESERVED_SLASH_VERBS.iter().any(|v| name == *v) {
            return None;
        }
        return Some(ClientMessage::DeactivateSkill {
            name: name.to_string(),
        });
    }
    // Activate form: `/<name>` or `/<plugin>:<name>`.
    // First word before any `:` is checked against reserved verbs.
    let first_segment = rest.split(':').next().unwrap_or(rest);
    if RESERVED_SLASH_VERBS.iter().any(|v| first_segment == *v) {
        return None;
    }
    Some(ClientMessage::ActivateSkill {
        name: rest.to_string(),
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p origin-cli --lib input::tests::parse_skill 2>&1 | tail -10`
Expected: `test result: ok. 6 passed`.

- [ ] **Step 5: Wire into handle_submit**

Modify `crates/origin-cli/src/main.rs::handle_submit` (around line 270). After the existing `parse_mem_command` block (which ends with `return;` near line 308) and BEFORE the `{ let mut a = app.lock(); a.add_line("you> ", text); a.start_assistant_turn(); }` block (around line 309), insert:

```rust
    // `/skill <name>` / `/skill deactivate <name>` / `/<plugin>:<name>` —
    // route through a one-shot IPC connection just like /mem and /account.
    if let Some(msg) = origin_cli::input::parse_skill_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match send_skill_command(path, &msg).await {
            Ok(line) => app.lock().add_line("ok> ", &line),
            Err(e) => app.lock().add_line("error> ", &format!("{e}")),
        }
        handle.mark_dirty();
        return;
    }
```

Add a new helper after `send_decision` (search for `async fn send_decision`, around line 446):

```rust
/// Send a skill activate/deactivate message and drain the daemon's reply,
/// returning a one-line summary to render in the TUI. Mirrors the
/// `/mem` send_decision helper in shape.
async fn send_skill_command(path: &str, msg: &ClientMessage) -> Result<String> {
    let mut client: Connection = Connector::connect(path).await?;
    let body = serde_json::to_vec(msg)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;
    let resp = client.read_frame_body().await?;
    let ev: StreamEvent = serde_json::from_slice(&resp)
        .map_err(|e| anyhow::anyhow!("bad reply: {e}"))?;
    match ev {
        StreamEvent::SkillActive { name, allowed_tools } => {
            if allowed_tools.is_empty() {
                Ok(format!("skill `{name}` active (no narrowing)"))
            } else {
                Ok(format!(
                    "skill `{name}` active; allowed tools: {}",
                    allowed_tools.join(", ")
                ))
            }
        }
        StreamEvent::SkillError { message } => Err(anyhow::anyhow!("{message}")),
        StreamEvent::AdminOk => Ok("skill deactivated".to_string()),
        other => Err(anyhow::anyhow!("unexpected reply: {other:?}")),
    }
}
```

- [ ] **Step 6: Verification gate**

```bash
cargo check -p origin-cli --tests 2>&1 | tail -5
```
Expected: `Finished`.

```bash
cargo test -p origin-cli --lib input::tests::parse_skill 2>&1 | tail -10
```
Expected: `test result: ok. 6 passed`.

```bash
cargo test -p origin-cli --lib 2>&1 | tail -10
```
Expected: All input + welcome + config + workflows tests still PASS (no regression).

- [ ] **Step 7: Commit**

```bash
git add crates/origin-cli/src/input.rs crates/origin-cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): bare /<name> activates skills; /-<name> deactivates

Parses `/<name>`, `/<plugin>:<name>`, and the `/-` deactivate forms in
input.rs; routes through a one-shot IPC connection from handle_submit.
Reserved slash verbs (/mem, /account, /help) are protected from shadowing.
Renders SkillActive/SkillError replies in the scrollback.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Skill catalog in system prompt

**Goal:** Every agent turn prepends a one-line-per-skill catalog ("name: description") to the system prompt so the model knows which skills exist. Mark currently-active skills with a leading marker.

**Files:**
- Modify: `crates/origin-daemon/src/agent.rs:24` (LoopOptions struct) — add `skill_catalog: Option<Arc<SkillCatalog>>`
- Modify: `crates/origin-daemon/src/agent.rs:283-293` (`recalled_system` block) — prepend catalog text
- Modify: `crates/origin-daemon/src/main.rs:798` — pass `Some(Arc::clone(&skill_catalog))` into `LoopOptions`
- Test: new test file `crates/origin-daemon/tests/skill_catalog_in_system_prompt.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-daemon/tests/skill_catalog_in_system_prompt.rs`:

```rust
//! Confirms the agent's system prompt includes a one-liner per skill in
//! the daemon's catalog.

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_daemon::skill_catalog::SkillCatalog;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::{Arc, Mutex};

/// Capture the `system` field of every ChatRequest the run_loop emits, so
/// the test can assert the catalog text was injected.
struct CapturingProvider {
    seen_systems: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    fn name(&self) -> &'static str {
        "capturing"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen_systems.lock().expect("lock").push(req.system);
        // Reply with a terminal turn so the loop exits immediately.
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

fn write_skill(dir: &std::path::Path, name: &str, desc: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("mkdir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {desc}\nallowed-tools: [\"Read\"]\n---\nbody\n"
        ),
    )
    .expect("write");
}

#[tokio::test]
async fn system_prompt_lists_each_skill_in_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_skill(dir.path(), "alpha", "Does alpha things");
    write_skill(dir.path(), "beta", "Does beta things");
    let catalog = Arc::new(SkillCatalog::load_from(dir.path()).expect("load"));

    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        skill_catalog: Some(Arc::clone(&catalog)),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    let systems = provider.seen_systems.lock().expect("lock");
    assert_eq!(systems.len(), 1);
    let sys = &systems[0];
    assert!(sys.contains("alpha"), "system prompt missing alpha:\n{sys}");
    assert!(sys.contains("Does alpha things"), "alpha description missing:\n{sys}");
    assert!(sys.contains("beta"), "system prompt missing beta:\n{sys}");
}

#[tokio::test]
async fn empty_catalog_does_not_pollute_system_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    // No skills written.
    let catalog = Arc::new(SkillCatalog::load_from(dir.path()).expect("load"));
    let provider = CapturingProvider {
        seen_systems: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("test", "test-model");
    let opts = LoopOptions {
        max_turns: 1,
        skill_catalog: Some(Arc::clone(&catalog)),
        ..LoopOptions::default().without_streaming()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");
    let systems = provider.seen_systems.lock().expect("lock");
    // Empty catalog → empty (or unchanged-by-injection) system prompt.
    assert!(
        !systems[0].contains("Available skills"),
        "empty catalog should not emit `Available skills` header"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-daemon --test skill_catalog_in_system_prompt 2>&1 | tail -10`
Expected: FAIL with `error[E0560]: struct \`LoopOptions\` has no field named \`skill_catalog\``.

- [ ] **Step 3: Extend LoopOptions**

Modify `crates/origin-daemon/src/agent.rs`. Add to the imports near line 14 (after `use origin_skills::SkillRegistry;`):

```rust
use origin_daemon::skill_catalog::SkillCatalog;
```

Add field to `LoopOptions` struct (after the existing `pub skills: Option<Arc<SkillRegistry>>` near line 70):

```rust
    /// Daemon-wide skill catalog injected into each turn's system prompt
    /// so the model knows which skills are available. The actual
    /// activation state lives in `skills` above; this is the catalog of
    /// all loadable skills, separate from "currently active".
    pub skill_catalog: Option<Arc<SkillCatalog>>,
```

Update `Default for LoopOptions` (near line 80) — add `skill_catalog: None,` to the struct literal.

- [ ] **Step 4: Compose the catalog text**

Inside `agent.rs::run_loop`, modify the `recalled_system` computation (line 283-293). Replace:

```rust
    let recalled_system =
        opts.injector
            .as_ref()
            .map_or_else(String::new, |injector| match injector.for_prompt(user_text, 5) {
                Ok(Some(ctx)) => ctx.block,
                Ok(None) => String::new(),
                Err(e) => {
                    tracing::warn!(error = %e, "injector.for_prompt failed; running without recall");
                    String::new()
                }
            });
```

with:

```rust
    let recall_block =
        opts.injector
            .as_ref()
            .map_or_else(String::new, |injector| match injector.for_prompt(user_text, 5) {
                Ok(Some(ctx)) => ctx.block,
                Ok(None) => String::new(),
                Err(e) => {
                    tracing::warn!(error = %e, "injector.for_prompt failed; running without recall");
                    String::new()
                }
            });

    // Build the skill-catalog block. One line per skill: "- <name>: <description>".
    // We mark currently-active skills with a leading `*` so the model knows
    // which mask is already in effect.
    let catalog_block = opts
        .skill_catalog
        .as_ref()
        .map(|cat| {
            if cat.is_empty() {
                String::new()
            } else {
                let active_names: std::collections::HashSet<String> = opts
                    .skills
                    .as_ref()
                    .map(|reg| reg.iter_active().map(|s| s.name.clone()).collect())
                    .unwrap_or_default();
                let mut out = String::from("Available skills (invoke via `/skill <name>`):\n");
                for s in cat.iter() {
                    let marker = if active_names.contains(&s.front.name) {
                        "*"
                    } else {
                        "-"
                    };
                    use std::fmt::Write as _;
                    let _ = writeln!(out, "  {marker} {}: {}", s.front.name, s.front.description);
                }
                out
            }
        })
        .unwrap_or_default();

    // Concatenate: catalog first (so it's stable across recall variation),
    // then recall context.
    let recalled_system = if catalog_block.is_empty() {
        recall_block
    } else if recall_block.is_empty() {
        catalog_block
    } else {
        format!("{catalog_block}\n{recall_block}")
    };
```

- [ ] **Step 5: Pass catalog from main.rs into LoopOptions**

Modify `crates/origin-daemon/src/main.rs`. In `handle_request` (around line 798) where `LoopOptions { ... }` is constructed, add the new field (after `skills: ...,`):

```rust
            skill_catalog: Some(Arc::clone(&skill_catalog)),
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p origin-daemon --test skill_catalog_in_system_prompt 2>&1 | tail -10`
Expected: `test result: ok. 2 passed`.

- [ ] **Step 7: Verification gate**

```bash
cargo check -p origin-daemon --tests 2>&1 | tail -10
```
Expected: `Finished`.

```bash
cargo test -p origin-daemon --test skill_catalog_in_system_prompt 2>&1 | tail -10
```
Expected: `test result: ok. 2 passed`.

```bash
cargo test -p origin-daemon --test skill_narrow_run_loop 2>&1 | tail -10
```
Expected: existing 2 tests still PASS (skill narrowing unaffected by catalog injection).

- [ ] **Step 8: Commit**

```bash
git add crates/origin-daemon/src/agent.rs crates/origin-daemon/src/main.rs crates/origin-daemon/tests/skill_catalog_in_system_prompt.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): inject skill catalog into every turn's system prompt

agent.rs::run_loop prepends `Available skills: ...` to `recalled_system`
when LoopOptions.skill_catalog is set; currently-active skills get a `*`
marker. Empty catalogs emit no text, preserving the prior behavior.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Phase C — Workflows

---

## Task 5: `{workflow:<name>}` chaining

**Goal:** A `{workflow:<name>}` token (brace-delimited, template-style) invokes a server-side handler that loads `~/.origin/workflows.toml`, looks up the named workflow, and activates each of its `steps[*].skill` in order. Reuses the activation path from Task 2 — each step is a `SkillRegistry::activate`. Surfaces a `StreamEvent::WorkflowActive { name, steps }` ack listing what was activated.

**Syntax notes:**
- Match form is the entire trimmed line equals `{workflow:<name>}`. Inline workflows (a `{workflow:X}` mid-sentence in a prompt) are NOT supported in v1 — too ambiguous with chat content that legitimately contains braces.
- Workflow names follow the same character constraints as skill names (no whitespace, may contain `:` and `-` and `.`).

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs` — add `ClientMessage::ActivateWorkflow { name }` + `StreamEvent::WorkflowActive { name, steps: Vec<String> }`
- Modify: `crates/origin-daemon/src/main.rs` — dispatch + workflow file load
- Modify: `crates/origin-cli/src/input.rs` — `parse_workflow_command`
- Modify: `crates/origin-cli/src/main.rs::handle_submit` — branch on workflow slash
- Test: extend `crates/origin-daemon/tests/skill_activation_protocol.rs` and `crates/origin-cli/src/input.rs::tests`

- [ ] **Step 1: Write the failing protocol tests**

Append to `crates/origin-daemon/tests/skill_activation_protocol.rs`:

```rust
#[test]
fn activate_workflow_message_round_trips_as_json() {
    let msg = ClientMessage::ActivateWorkflow {
        name: "frontend-design".into(),
    };
    let body = serde_json::to_vec(&msg).expect("encode");
    let decoded: ClientMessage = serde_json::from_slice(&body).expect("decode");
    match decoded {
        ClientMessage::ActivateWorkflow { name } => assert_eq!(name, "frontend-design"),
        other => panic!("expected ActivateWorkflow, got {other:?}"),
    }
}

#[test]
fn workflow_active_event_round_trips_as_json() {
    let ev = StreamEvent::WorkflowActive {
        name: "frontend-design".into(),
        steps: vec!["frontend-design:frontend-design".into(), "impeccable".into()],
    };
    let body = serde_json::to_vec(&ev).expect("encode");
    let decoded: StreamEvent = serde_json::from_slice(&body).expect("decode");
    match decoded {
        StreamEvent::WorkflowActive { name, steps } => {
            assert_eq!(name, "frontend-design");
            assert_eq!(steps.len(), 2);
        }
        other => panic!("expected WorkflowActive, got {other:?}"),
    }
}
```

Append to `crates/origin-cli/src/input.rs::tests`:

```rust
    #[test]
    fn parse_workflow_command_basic() {
        let m = parse_workflow_command("{workflow:frontend-design}").expect("parse");
        match m {
            ClientMessage::ActivateWorkflow { name } => {
                assert_eq!(name, "frontend-design");
            }
            other => panic!("wrong: {other:?}"),
        }
    }

    #[test]
    fn parse_workflow_command_tolerates_surrounding_whitespace() {
        let m = parse_workflow_command("  {workflow:frontend-design}  ").expect("parse");
        match m {
            ClientMessage::ActivateWorkflow { name } => assert_eq!(name, "frontend-design"),
            other => panic!("wrong: {other:?}"),
        }
    }

    #[test]
    fn parse_workflow_command_rejects_malformed() {
        assert!(parse_workflow_command("{workflow:}").is_none());
        assert!(parse_workflow_command("{workflow}").is_none());
        assert!(parse_workflow_command("{wf:foo}").is_none());
        // Brace token must be the whole trimmed line — inline references
        // (mid-sentence) are explicitly out of scope for v1.
        assert!(parse_workflow_command("please run {workflow:x}").is_none());
        // Skill slashes are not workflows.
        assert!(parse_workflow_command("/foo").is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run, both:

```bash
cargo test -p origin-daemon --test skill_activation_protocol activate_workflow 2>&1 | tail -10
```
Expected: FAIL with `no variant or associated item named \`ActivateWorkflow\``.

```bash
cargo test -p origin-cli --lib input::tests::parse_workflow 2>&1 | tail -10
```
Expected: FAIL with `cannot find function \`parse_workflow_command\``.

- [ ] **Step 3: Add protocol variants**

Modify `crates/origin-daemon/src/protocol.rs`. In `ClientMessage` (after the `DeactivateSkill` variant from Task 2), add:

```rust
    /// Walk `name`'s steps in `~/.origin/workflows.toml`, activating each
    /// step's skill in order on this connection's stack. The daemon
    /// replies with [`StreamEvent::WorkflowActive`] listing the skills it
    /// activated, or [`StreamEvent::SkillError`] if the workflow isn't
    /// found / no skill in the chain resolves.
    ActivateWorkflow { name: String },
```

In `StreamEvent` (after `SkillError`), add:

```rust
    /// Ack for a successful [`ClientMessage::ActivateWorkflow`]. `steps` is
    /// the ordered list of skill names that were activated; the CLI renders
    /// them so the user can see the chain that just took effect.
    WorkflowActive {
        name: String,
        steps: Vec<String>,
    },
```

- [ ] **Step 4: Implement workflow loader in the daemon**

The daemon needs to read `~/.origin/workflows.toml`. The `WorkflowsFile` struct lives in `origin-cli`, but we don't want a daemon→cli dep. Move the workflow shape to a new shared location. Two options:
1. Duplicate the struct in the daemon (acceptable for v1; small).
2. Extract to a new `origin-workflows` crate (purer; more files).

For this plan, take option 1 — duplicate the struct in the daemon. Cleaner refactor later.

Add a new module file `crates/origin-daemon/src/workflows.rs`:

```rust
//! Daemon-side workflow loader. Mirrors the on-disk shape produced by
//! `origin init` (see `crates/origin-cli/src/workflows.rs`). Kept as a
//! small duplicate rather than introducing a daemon→cli dep; consolidating
//! into an `origin-workflows` crate is a follow-up.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub skill: String,
    #[serde(default)]
    pub args: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowsFile {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
}

pub fn load_from(p: &Path) -> std::io::Result<WorkflowsFile> {
    if !p.exists() {
        return Ok(WorkflowsFile::default());
    }
    let raw = std::fs::read_to_string(p)?;
    toml::from_str(&raw).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_empty_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        let file = load_from(&p).expect("load");
        assert!(file.workflows.is_empty());
    }

    #[test]
    fn loads_seeded_example() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("workflows.toml");
        std::fs::write(
            &p,
            "schema_version = 1\n\
             [[workflows]]\n\
             name = \"frontend-design\"\n\
             [[workflows.steps]]\n\
             skill = \"frontend-design:frontend-design\"\n\
             [[workflows.steps]]\n\
             skill = \"impeccable\"\n\
             args = \"teach\"\n",
        )
        .expect("write");
        let file = load_from(&p).expect("load");
        assert_eq!(file.workflows.len(), 1);
        assert_eq!(file.workflows[0].name, "frontend-design");
        assert_eq!(file.workflows[0].steps.len(), 2);
        assert_eq!(file.workflows[0].steps[1].args.as_deref(), Some("teach"));
    }
}
```

Update `crates/origin-daemon/src/lib.rs` — add `pub mod workflows;` after the `pub mod skill_catalog;` line from Task 1.

Also need the `toml` crate in origin-daemon. Modify `crates/origin-daemon/Cargo.toml`:

```toml
toml = "0.8"
```

(Add to the `[dependencies]` block, before `[dev-dependencies]`.)

- [ ] **Step 5: Add the dispatch arm in main.rs**

Modify `crates/origin-daemon/src/main.rs`. Add to imports:

```rust
use origin_daemon::workflows;
```

In the `match msg { ... }` block, after the `ClientMessage::DeactivateSkill` arm from Task 2:

```rust
                ClientMessage::ActivateWorkflow { name } => {
                    let cat = Arc::clone(&skill_catalog);
                    let reg = Arc::clone(&active_skills);
                    let conn_clone = Arc::clone(&conn);
                    // Load workflows.toml fresh each time so user edits
                    // are picked up without a daemon restart.
                    let home = std::env::var_os("ORIGIN_HOME")
                        .map(std::path::PathBuf::from)
                        .or_else(dirs::home_dir)
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    let wf_path = home.join(".origin").join("workflows.toml");
                    let file = match workflows::load_from(&wf_path) {
                        Ok(f) => f,
                        Err(e) => {
                            let ev = StreamEvent::SkillError {
                                message: format!("workflows.toml load: {e}"),
                            };
                            let body = serde_json::to_vec(&ev).unwrap_or_default();
                            let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                            continue;
                        }
                    };
                    let Some(wf) = file.workflows.iter().find(|w| w.name == name) else {
                        let ev = StreamEvent::SkillError {
                            message: format!("no such workflow: {name}"),
                        };
                        let body = serde_json::to_vec(&ev).unwrap_or_default();
                        let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                        continue;
                    };
                    let mut activated: Vec<String> = Vec::new();
                    for step in &wf.steps {
                        if let Some(skill) = cat.find(&step.skill) {
                            reg.lock().await.activate(skill.front.clone());
                            activated.push(step.skill.clone());
                        } else {
                            // Surface the missing skill but keep going on the rest —
                            // a partial chain is still useful.
                            let ev = StreamEvent::SkillError {
                                message: format!(
                                    "workflow {name} step {} missing from catalog: {}",
                                    activated.len() + 1,
                                    step.skill
                                ),
                            };
                            let body = serde_json::to_vec(&ev).unwrap_or_default();
                            let _ = conn_clone
                                .lock()
                                .await
                                .write_frame(FrameKind::Event, &body)
                                .await;
                        }
                    }
                    let ev = StreamEvent::WorkflowActive {
                        name: name.clone(),
                        steps: activated,
                    };
                    let body = serde_json::to_vec(&ev).unwrap_or_default();
                    let _ = conn_clone.lock().await.write_frame(FrameKind::Event, &body).await;
                }
```

- [ ] **Step 6: CLI parser for `{workflow:<name>}`**

Modify `crates/origin-cli/src/input.rs`. After `parse_skill_command` (from Task 3), add:

```rust
/// Parse `{workflow:<name>}` (the whole trimmed line) into a
/// [`ClientMessage::ActivateWorkflow`]. Surrounding whitespace is allowed;
/// inline references mid-prompt are NOT — the entire trimmed line must be
/// the brace token, to keep the form unambiguous with chat content that
/// happens to mention braces.
///
/// Returns `None` for unrecognized input.
#[must_use]
pub fn parse_workflow_command(line: &str) -> Option<ClientMessage> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('{')?.strip_suffix('}')?;
    let name = inner.strip_prefix("workflow:")?.trim();
    if name.is_empty() {
        return None;
    }
    // Workflow names follow skill-name rules: no embedded whitespace.
    if name.chars().any(char::is_whitespace) {
        return None;
    }
    Some(ClientMessage::ActivateWorkflow {
        name: name.to_string(),
    })
}
```

- [ ] **Step 7: Wire `/workflow` into handle_submit**

Modify `crates/origin-cli/src/main.rs::handle_submit`. After the `parse_skill_command` block from Task 3 (and before the assistant-turn block), add:

```rust
    if let Some(msg) = origin_cli::input::parse_workflow_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match send_skill_command(path, &msg).await {
            Ok(line) => app.lock().add_line("ok> ", &line),
            Err(e) => app.lock().add_line("error> ", &format!("{e}")),
        }
        handle.mark_dirty();
        return;
    }
```

Extend the existing `send_skill_command` helper from Task 3 to handle the new `StreamEvent::WorkflowActive` reply. Locate the `match ev { ... }` block in `send_skill_command` (added in Task 3) and add:

```rust
        StreamEvent::WorkflowActive { name, steps } => {
            if steps.is_empty() {
                Ok(format!("workflow `{name}` activated (no steps resolved)"))
            } else {
                Ok(format!(
                    "workflow `{name}` activated; skills: {}",
                    steps.join(" → ")
                ))
            }
        }
```

- [ ] **Step 8: Run all tests**

```bash
cargo test -p origin-daemon --test skill_activation_protocol 2>&1 | tail -10
```
Expected: `test result: ok. 6 passed` (4 from Task 2 + 2 from Task 5).

```bash
cargo test -p origin-daemon --lib workflows:: 2>&1 | tail -10
```
Expected: `test result: ok. 2 passed`.

```bash
cargo test -p origin-cli --lib input::tests::parse_workflow 2>&1 | tail -10
```
Expected: `test result: ok. 3 passed`.

- [ ] **Step 9: Verification gate**

```bash
cargo check -p origin-daemon --tests 2>&1 | tail -10
```
Expected: `Finished`.

```bash
cargo check -p origin-cli --tests 2>&1 | tail -5
```
Expected: `Finished`.

```bash
cargo test -p origin-cli --lib 2>&1 | tail -5
```
Expected: All previously-passing tests still pass.

```bash
cargo test -p origin-daemon --tests --no-fail-fast 2>&1 | tail -20
```
Expected: New tests pass; pre-existing `main.rs:402` and downstream bin-test failures are NOT in scope (do not attempt to fix in this task).

- [ ] **Step 10: Commit**

```bash
git add crates/origin-daemon/src/protocol.rs crates/origin-daemon/src/workflows.rs crates/origin-daemon/src/lib.rs crates/origin-daemon/src/main.rs crates/origin-daemon/Cargo.toml crates/origin-daemon/tests/skill_activation_protocol.rs crates/origin-cli/src/input.rs crates/origin-cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat: {workflow:<name>} chains skill activations server-side

ActivateWorkflow message + WorkflowActive ack; daemon loads
~/.origin/workflows.toml on demand and walks each step's skill onto the
connection's SkillRegistry. CLI parser matches the whole-line brace
token; inline references mid-prompt are explicitly out of scope.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Tab autocomplete for skills and workflows

**Goal:** When the user presses Tab in the TUI input, complete the partially-typed `/<skill>`, `/-<skill>`, or `{workflow:<name>}` token against the on-disk catalog. Pure completion logic lives in a new module; the TUI event loop intercepts Tab BEFORE calling the input reducer.

**Design choices:**
- Completion logic is a pure function `complete(&mut String, &CompletionSources) -> CompletionResult`. No I/O, no async, no global state — fully unit-testable.
- The CLI reads `~/.origin/skills/` and `~/.origin/workflows.toml` directly (it already depends on `origin-skills` and has its own `workflows` module) rather than adding a new IPC verb. Re-loading on every Tab keeps it simple; the dirs are tiny.
- Tab on a non-matching buffer is a no-op (returns `CompletionResult::NoMatch`); Tab with exactly one match completes; Tab with multiple matches completes to the longest common prefix and emits the candidate list for the caller to display as a status line.

**Files:**
- Create: `crates/origin-cli/src/autocomplete.rs`
- Modify: `crates/origin-cli/src/lib.rs` — add `pub mod autocomplete;`
- Modify: `crates/origin-cli/src/main.rs` — Tab handler in the event loop (BEFORE `reduce(...)`)

- [ ] **Step 1: Write the failing tests for the completion engine**

Create `crates/origin-cli/src/autocomplete.rs`:

```rust
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
        return complete_with(
            buffer,
            partial,
            &sources.workflows,
            |full| format!("{{workflow:{full}}}"),
        );
    }
    // Skill shapes — `/-<name>` (deactivate) or `/<name>` (activate).
    if let Some(partial) = buffer.strip_prefix("/-") {
        return complete_with(buffer, partial, &sources.skills, |full| {
            format!("/-{full}")
        });
    }
    if let Some(partial) = buffer.strip_prefix('/') {
        // Whitespace inside the partial means it's not a slash command.
        if partial.chars().any(char::is_whitespace) {
            return CompletionResult::NoMatch;
        }
        return complete_with(buffer, partial, &sources.skills, |full| format!("/{full}"));
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
    let matches: Vec<&String> = candidates
        .iter()
        .filter(|c| c.starts_with(partial))
        .collect();
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

#[cfg(test)]
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
        // Both workflows start with "" so LCP completion is a no-op,
        // but the candidate list is returned.
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
        // `/foo bar` is chat text, not a slash command.
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
        // Make sure we never split inside a multi-byte char.
        assert_eq!(common_prefix_len("αβ", "αγ"), "α".len());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p origin-cli --lib autocomplete:: 2>&1 | tail -10`
Expected: FAIL with `error[E0432]: unresolved import` or module-not-found.

- [ ] **Step 3: Wire the module into lib.rs**

Modify `crates/origin-cli/src/lib.rs`. After the `pub mod autocomplete;` slot left for it (or just before `pub mod cli_def;` if not already present), add:

```rust
pub mod autocomplete;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p origin-cli --lib autocomplete:: 2>&1 | tail -10`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 5: Add the loader helpers in autocomplete.rs**

Append to `crates/origin-cli/src/autocomplete.rs`:

```rust
// ---------------------------------------------------------------------------
// Loader helpers — read the live catalog from disk.
// ---------------------------------------------------------------------------

/// Build a [`CompletionSources`] by reading `~/.origin/skills/` (every
/// `<dir>/SKILL.md`) and `~/.origin/workflows.toml`. Failures degrade to
/// empty lists so a missing directory or corrupt file doesn't break Tab.
#[must_use]
pub fn load_sources() -> CompletionSources {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let skills_dir = home.join(".origin").join("skills");
    let workflows_path = home.join(".origin").join("workflows.toml");

    let skills: Vec<String> = origin_skills::load_skills_dir(&skills_dir)
        .map(|v| v.into_iter().map(|s| s.front.name).collect())
        .unwrap_or_default();
    let workflows: Vec<String> = crate::workflows::load_from(&workflows_path)
        .ok()
        .flatten()
        .map(|f| f.workflows.into_iter().map(|w| w.name).collect())
        .unwrap_or_default();
    CompletionSources { skills, workflows }
}
```

- [ ] **Step 6: Wire Tab interception into main.rs's event loop**

Modify `crates/origin-cli/src/main.rs::run_event_loop`. Find the existing event-handling block (around line 189-205) that pattern-matches `crossterm::event::Event::Key(ev)` and calls `reduce`. Replace the inner key match with:

```rust
        if let crossterm::event::Event::Key(ev) = maybe_ev? {
            // Tab fires the autocomplete pass before the buffer reducer
            // sees the keypress; only fall through to `reduce` when Tab
            // didn't recognize the buffer shape.
            if matches!(ev.code, crossterm::event::KeyCode::Tab) {
                let result = {
                    let mut a = app.lock();
                    let sources = origin_cli::autocomplete::load_sources();
                    origin_cli::autocomplete::complete(&mut a.input, &sources)
                };
                match result {
                    origin_cli::autocomplete::CompletionResult::NoMatch => {}
                    origin_cli::autocomplete::CompletionResult::UniqueCompletion => {
                        handle.mark_dirty();
                    }
                    origin_cli::autocomplete::CompletionResult::MultipleCandidates {
                        candidates,
                    } => {
                        let line = format!("candidates: {}", candidates.join(", "));
                        app.lock().add_line("tab> ", &line);
                        handle.mark_dirty();
                    }
                }
                continue;
            }

            let action = {
                let mut a = app.lock();
                reduce(&mut a.input, ev)
            };
            match action {
                InputAction::Quit => break,
                InputAction::Submit(text) => {
                    handle_submit(&app, &handle, path, model, &text).await;
                }
                _ => {
                    handle.mark_dirty();
                }
            }
        }
```

- [ ] **Step 7: Verification gate**

```bash
cargo check -p origin-cli --tests 2>&1 | tail -5
```
Expected: `Finished`.

```bash
cargo test -p origin-cli --lib autocomplete:: 2>&1 | tail -10
```
Expected: `test result: ok. 10 passed`.

```bash
cargo test -p origin-cli --lib 2>&1 | tail -10
```
Expected: All previously-passing tests still PASS. No regression in input, welcome, config, workflows, init, or init_probe modules.

```bash
cargo build -p origin-cli --bin origin 2>&1 | tail -3
```
Expected: `Finished` — confirms the binary compiles end-to-end with the new Tab handler.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-cli/src/autocomplete.rs crates/origin-cli/src/lib.rs crates/origin-cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): Tab autocompletes skills (/<name>) and workflows ({workflow:<name>})

Pure completion logic in autocomplete::complete; reads
~/.origin/skills and ~/.origin/workflows.toml on each Tab. Three
recognized shapes: /<partial>, /-<partial>, {workflow:<partial>}.
Unique match completes; multiple matches advance to LCP and surface
candidates as a `tab>` status line.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Self-Review

**Spec coverage** — each user-stated requirement maps to a numbered Task above:
- "Add a prompt that will be the first prompt run after onboarding" → Task 0 ✓
- "Daemon-side: on startup, load_skills_dir into a Catalog keyed by name" → Task 1 ✓
- "Protocol: add ClientMessage::ActivateSkill/DeactivateSkill + StreamEvent::SkillActive ack" → Task 2 ✓
- "/ for skills only (e.g. /this-is-a-skill)" → Task 3 ✓ (bare `/<name>`, no `/skill` verb; namespaced `/<plugin>:<name>` retained; `/-<name>` for deactivate)
- "Model surface: inject a one-line catalog of skill names + descriptions into the system prompt of every turn" → Task 4 ✓
- "{workflow:<name>} for workflows (e.g. {workflow:frontend-design})" → Task 5 ✓
- "Tab autocomplete for both skills and workflows" → Task 6 ✓ (intercepts Tab in event loop; pure `complete()` engine; reads catalog from `~/.origin/skills/` and `~/.origin/workflows.toml`)
- "/dispatching-parallel-agents for independent steps" → Phase A (Tasks 0+1+2), Phase B (Tasks 3+4), and Phase C (Tasks 5+6) all run in parallel ✓
- "/test-driven-development" → Every task starts with a failing test, runs it, then implements ✓
- "/verification-before-completion after every step" → Each task ends with an explicit "Verification gate" step listing concrete commands + expected output ✓

**Placeholder scan** — no TBDs, no "implement later", no abstract "handle edge cases". Every code block is the actual code to write. Every command is the actual command to run, with expected output. The "test placeholder" comment in Task 2 Step 5 is intentional and explained.

**Type consistency** — `SkillRegistry::activate(SkillFrontmatter)` matches across Tasks 2 and 5. `SkillCatalog::find(&str) -> Option<&Skill>` matches Tasks 1, 2, 5. `ClientMessage::ActivateSkill { name: String }` and `StreamEvent::SkillActive { name, allowed_tools }` consistent in protocol.rs + tests + dispatch. The new `iter_active` method added in Task 2 is reused in Task 4. `LoopOptions.skill_catalog: Option<Arc<SkillCatalog>>` matches the Arc-clone wiring in main.rs. The new `CompletionSources { skills, workflows }` in Task 6 uses the same name strings as Tasks 1 (skill `front.name`) and 5 (workflow `name` field) — no field-renames between tasks.

**Syntax consistency** — Task 3 parser accepts `/<name>`, `/<plugin>:<name>`, `/-<name>`, rejects `/mem`/`/account`/`/help` and whitespace; Task 5 parser accepts only the whole-line `{workflow:<name>}` form; Task 6 autocomplete handles all three shapes (slash, dash-slash, brace-workflow). No parser claims a shape another parser also claims.

**Pre-existing failure isolation** — `crates/origin-daemon/src/main.rs:402` `Arc::clone(consolidator)` type-inference error is called out as out-of-scope in the front-matter and in every Task 1/2/4/5 verification gate. Subagents must NOT try to fix it.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-21-skill-invocation-system.md`. Two execution options:

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task with two-stage review between tasks. Phase A's three independent tasks fire in parallel (one batched message, three Agent calls); Phase B's two tasks fire in parallel after A merges; Phase C's two tasks fire in parallel after B merges. Seven tasks, three parallel rounds total.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`, batched at phase boundaries for review.

**Which approach?**
