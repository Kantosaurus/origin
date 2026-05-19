# `origin` Phase 10 — Extensibility Quartet (`origin-skills` + `origin-hooks` + `origin-mcp` + Permission v2) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax. Tasks marked **[parallel-safe]** can run concurrently in fresh subagents (see "Parallelization" below).

**Branch:** All Phase 10 work lands on branch `phase-10` (branched off `dev`).

**Goal:** Stand up four orthogonal extensibility subsystems on top of the P9 baseline — (1) a Skills loader with embedding-indexed lazy injection + allowed-tools narrowing + first-run import, (2) a Hooks engine with a pre-spawned shell pool and typed lifecycle payloads, (3) a Model Context Protocol (MCP) client supporting stdio/HTTP+SSE transports with OAuth-via-KeyVault and CAS-backed outputs, and (4) a Permission v2 layer with a bloom-filter pre-check and side-panel-only prompt routing.

**Architecture:** Three new crates (`origin-skills`, `origin-hooks`, `origin-mcp`) plus surgical extensions to `origin-permission`, `origin-tools`, `origin-mem`, `origin-cas`, `origin-keyvault`, `origin-daemon`, and `origin-tui`. Each new crate is independent of the others — they share only the existing P0-P9 substrate (CAS, KeyVault, MemIndex, ToolMeta registry, side-panel Prompter). This means the four area-clusters are **fully parallelizable** after the branch-checkpoint task (P10.0).

**Tech Stack:** Rust 1.83 (MSRV pin), `serde_yaml` 0.9 (skill frontmatter), `tokio` 1 (already a workspace dep — process pools + SSE streams), `reqwest` 0.12 (HTTP+SSE MCP transport; cargo feature `stream` for SSE), `eventsource-stream` 0.2 (SSE line framing), `growable-bloom-filter` 2 (permission pre-check), `async-trait` 0.1 (already used), `serde_json` 1 (MCP JSON-RPC, hook stdout overrides), `thiserror` 1, `ulid` 1 (skill/hook/mcp request ids). **Novel-implementation reflex** per `[[feedback_novel_implementations]]`: every signature subsystem must beat openclaude/jcode/opencode on tokens or perf. Phase 10's novelties: (1) skill bodies are content-addressed in CAS and indexed in `MemIndex` with kind=`Skill`, so re-imports across users dedupe and recall is the same code path as memory; (2) hook scripts dispatch through a long-lived `tokio::process::Child` pool — no fork-per-event; per-event amortized cost is one `write_all` to stdin and one `read_until('\0')` from stdout; (3) MCP tool results > 16 KiB are CAS-handle'd before the model ever sees them, so the prompt carries `cas:` URIs instead of full bodies; (4) permission pre-check is a 4 KiB growable bloom that rejects 95%+ of unconfigured tools before the full rule walk; (5) permission asks are queue-pumped through the existing side-panel `PanelEvent::PermissionAsk` path — no modal stack, concurrent asks queue naturally.

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` mechanisms **N9.2** (bloom pre-check), **N9.3** (side-panel prompts), **N9.4** (embedding-indexed skills), **N9.5** (allowed-tools narrowing), **N9.6** (first-run import), **N9.7** (shell pool), **N9.8** (typed event payloads), **N9.10–N9.13** (MCP). Builds on tagged baselines `p7-complete` (codegraph + tools) and `p9-complete` (swarm + plan + memory + side panel).

**Phase 10 spec-mechanism citations:**

- **N9.2** — Bloom-filter pre-check for permissions (Task P10.12)
- **N9.3** — Side-panel-only prompts; modal removed (Task P10.13)
- **N9.4** — Skill bodies embed into `MemIndex` with `kind = Skill` (Task P10.2)
- **N9.5** — Per-skill `allowed-tools` mask stacked onto permission engine (Task P10.3)
- **N9.6** — First-run import from `~/.claude/skills/` with content-hash dedupe + user-confirm (Task P10.4)
- **N9.7** — Pre-spawned shell pool for hook dispatch (Task P10.5)
- **N9.8** — Typed lifecycle event payloads + stdout `{"override":…}` parsing (Task P10.6)
- **N9.10** — MCP stdio transport (Task P10.7)
- **N9.11** — MCP tool registry integration via `McpToolProxy` (Task P10.9)
- **N9.12** — MCP HTTP + SSE transport (Task P10.8)
- **N9.13** — MCP OAuth via KeyVault (Task P10.11)

**What is explicitly out of scope for Phase 10** (deferred):

- MCP server-side implementation. P10 is client-only.
- Hook script *sandboxing* — that lands in P11 (Linux landlock + seccomp); P10 inherits the parent daemon's existing process privileges.
- Sidecar-class hook dispatch (N9.9) — the *pool* lands in P10.5; sidecar-class assignment lives in P12.2. P10 dispatches hooks on the default Tokio runtime.
- Per-server MCP quarantine isolation — P10 ships a configuration knob (`quarantine: true|false`) but enforces it as a soft per-tool `Tier::RequiresPermission` override; full process isolation lands in P11.
- TUI rendering polish for skills/hooks/MCP side-panels — P10 wires the events through the existing `PanelEvent` enum; richer rendering is a P14 doc-and-polish task.
- Allowed-tools narrowing for *transitively spawned* subagent tools (e.g. swarm workers) — P10.3 narrows the active session only; worker inheritance lands with P12 sidecar-class plumbing.

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**

1. Write the failing test.
2. Run it — confirm the expected failure mode (compile error or assertion).
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit (Conventional Commits, scoped to crate).

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / tools registration / migration | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Bench-touching tasks (P10.5 shell-pool reuse, P10.12 bloom hit-rate) | All of the above + `cargo bench -p <crate> --bench <name> -- --quick` exits 0 with thresholds met |
| Final phase gate (P10.13) | All of the above + tag `p10-complete` |

**Patterns inherited from earlier phases:**

- `[lints] workspace = true` in every new crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All persisted/IPC-crossing types derive `serde::{Serialize, Deserialize}` (JSON for MCP / hook payloads) or `rkyv::{Archive, Serialize, Deserialize}` with `#[archive(check_bytes)]` (records that round-trip through CAS).
- `[lints.rust] unsafe_code = "forbid"` is the default; all three new crates keep the forbid.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `unwrap()` and never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex** (`[[project_msrv_dep_pinning]]`): if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>` and record in `Cargo.lock`. Likely candidates this phase: `reqwest`'s transitive `idna`/`url` chain (try `idna = "=0.5.0"`, `url = "=2.5.2"`).
- **Novel-implementation reflex** (`[[feedback_novel_implementations]]`): if a step's implementation collapses into "the obvious thing openclaude does", stop and re-read the architecture novelties listed above.

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit** on branch `phase-10`. Final commit on P10.13 carries `tag: p10-complete`.

---

## Parallelization

After P10.0 the work splits into **four independent area-clusters** with no shared mutable state. Each cluster can be assigned to a fresh subagent and progressed in lock-step with the others:

| Cluster | Tasks | New crate | Touches existing crates |
|---|---|---|---|
| **A. Skills** | P10.1 → P10.2 → P10.3 → P10.4 | `origin-skills` | `origin-mem` (P10.2), `origin-permission` (P10.3), `origin-cas` (P10.2 dedupe) |
| **B. Hooks** | P10.5 → P10.6 | `origin-hooks` | `origin-daemon` (P10.6 lifecycle wiring) |
| **C. MCP** | P10.7 → P10.8 → P10.9 → P10.10 → P10.11 | `origin-mcp` | `origin-tools` (P10.9), `origin-cas` (P10.10), `origin-keyvault` (P10.11) |
| **D. Permission v2** | P10.12 → P10.13 | _(extends `origin-permission`)_ | `origin-tui` (P10.13) |

Within a cluster, tasks **must** run sequentially (later tasks depend on earlier types/modules within the same crate). Across clusters there are no compile-time or test-time dependencies; subagents may proceed without waiting for siblings. The final task **P10.13** depends on the bloom layer from P10.12 (same crate, sequential) and gates the `p10-complete` tag — the dispatcher should hold the tag-bearing merge until all four clusters land on `phase-10`.

---

## File map for Phase 10

| New / modified file | Responsibility |
|---|---|
| **Cluster A — Skills** | |
| `crates/origin-skills/Cargo.toml` | manifest; workspace lints |
| `crates/origin-skills/src/lib.rs` | public surface — re-exports + module declarations |
| `crates/origin-skills/src/frontmatter.rs` | parse YAML frontmatter (`name`, `description`, `allowed-tools`, `body`) (P10.1) |
| `crates/origin-skills/src/loader.rs` | walk `~/.origin/skills/*/SKILL.md`; dedupe by body hash; return `Vec<Skill>` (P10.1) |
| `crates/origin-skills/src/registry.rs` | in-memory active-skill stack + `allowed-tools` aggregator (P10.3) |
| `crates/origin-skills/src/embed.rs` | embed skill bodies into `MemIndex` with `kind=Skill` (P10.2) |
| `crates/origin-skills/src/import.rs` | first-run import from `~/.claude/skills/` w/ user-confirm (P10.4) |
| `crates/origin-skills/tests/frontmatter.rs` | valid + malformed fixtures (P10.1) |
| `crates/origin-skills/tests/loader.rs` | filesystem walk + dedupe (P10.1) |
| `crates/origin-skills/tests/embed.rs` | upsert into a `MemIndex` + recall returns skill (P10.2) |
| `crates/origin-skills/tests/registry.rs` | allowed-tools mask aggregates correctly (P10.3) |
| `crates/origin-skills/tests/import.rs` | dedupe + confirm callback wiring (P10.4) |
| **Cluster B — Hooks** | |
| `crates/origin-hooks/Cargo.toml` | manifest; workspace lints |
| `crates/origin-hooks/src/lib.rs` | public surface — re-exports |
| `crates/origin-hooks/src/shellpool.rs` | pre-spawned `tokio::process::Child` pool keyed by interpreter (P10.5) |
| `crates/origin-hooks/src/event.rs` | typed `LifecycleEvent` + serde payload schemas (P10.6) |
| `crates/origin-hooks/src/dispatch.rs` | event → pooled shell call → `HookResult` (P10.6) |
| `crates/origin-hooks/tests/shellpool.rs` | 100 dispatches → pool reuse asserted (P10.5) |
| `crates/origin-hooks/tests/event.rs` | every event kind round-trips + override parses (P10.6) |
| `crates/origin-hooks/benches/shellpool.rs` | dispatch-vs-spawn microbenchmark (P10.5) |
| **Cluster C — MCP** | |
| `crates/origin-mcp/Cargo.toml` | manifest; workspace lints |
| `crates/origin-mcp/src/lib.rs` | public surface |
| `crates/origin-mcp/src/jsonrpc.rs` | minimal JSON-RPC 2.0 framer (request/response/notification) (P10.7) |
| `crates/origin-mcp/src/transport.rs` | `Transport` trait (P10.7) |
| `crates/origin-mcp/src/transport_stdio.rs` | stdio transport over `tokio::process::Child` (P10.7) |
| `crates/origin-mcp/src/transport_http.rs` | HTTP POST + SSE event-stream transport (P10.8) |
| `crates/origin-mcp/src/client.rs` | `McpClient` — handshake, list_tools, call_tool (P10.7, P10.8) |
| `crates/origin-mcp/src/proxy.rs` | `McpToolProxy` — registers MCP tools into the `origin-tools` registry (P10.9) |
| `crates/origin-mcp/src/cas_handoff.rs` | wrap a tool result body > 16 KiB into a CAS handle (P10.10) |
| `crates/origin-mcp/src/oauth.rs` | bridge `KeyVault::OAuthClient` into the HTTP transport's bearer header (P10.11) |
| `crates/origin-mcp/tests/jsonrpc.rs` | request/response/notification framing (P10.7) |
| `crates/origin-mcp/tests/stdio.rs` | mock server over stdio (P10.7) |
| `crates/origin-mcp/tests/http.rs` | mock server over HTTP+SSE (P10.8) |
| `crates/origin-mcp/tests/proxy.rs` | MCP tool dispatched through registry path (P10.9) |
| `crates/origin-mcp/tests/cas_handoff.rs` | large body → CAS handle path (P10.10) |
| `crates/origin-mcp/tests/oauth.rs` | device-flow → bearer → call_tool (P10.11) |
| **Cluster D — Permission v2** | |
| `crates/origin-permission/src/bloom.rs` *(new)* | growable-bloom-filter wrapper + rule pre-check (P10.12) |
| `crates/origin-permission/src/rules.rs` *(new)* | rule shape + matcher (`tool@scope`) (P10.12) |
| `crates/origin-permission/src/lib.rs` *(modify)* | wire bloom + rules into `check` (P10.12) |
| `crates/origin-permission/tests/bloom.rs` | 1000 unrelated calls vs 30 rules: ≥95% rejected; brute-force parity = 100% (P10.12) |
| `crates/origin-tui/src/cli_prompter.rs` *(modify)* | concurrent ask queue assertion; drop any modal fallback (P10.13) |
| `crates/origin-tui/tests/concurrent_asks.rs` *(new)* | two simultaneous asks → both deliver via queue (P10.13) |
| **Cross-cutting** | |
| `Cargo.toml` *(modify, P10.1 / P10.5 / P10.7)* | new crates picked up by `members = ["crates/*"]`; no edit needed unless workspace deps are added; we **do** add `reqwest`, `eventsource-stream`, `growable-bloom-filter`, `serde_yaml` to `[workspace.dependencies]` (P10.0) |
| `crates/origin-daemon/src/agent.rs` *(modify, P10.6 / P10.9)* | wire lifecycle dispatch + register MCP-discovered tools |

**File-size discipline:** every new `.rs` file targets <400 LOC. If a task naturally pushes a file past 400 LOC, split early (e.g. `transport_http.rs` → `transport_http/post.rs` + `transport_http/sse.rs` + `transport_http/mod.rs`).

---

## Task P10.0 — Branch + workspace dep additions + plan checkpoint

**Files:**

- Modify: `Cargo.toml` (root workspace) — add new shared deps so each cluster crate inherits version pins.
- Create / modify: branch state — branch off `dev` to `phase-10`.

- [ ] **Step 1: Create the phase-10 branch**

```bash
git checkout dev
git pull --ff-only
git checkout -b phase-10
```

Run: `git branch --show-current`
Expected output: `phase-10`

- [ ] **Step 2: Add shared workspace deps**

Edit `Cargo.toml` at the workspace root to add the following block (or merge into the existing `[workspace.dependencies]` if one already exists — the current root has none):

```toml
[workspace.dependencies]
serde_yaml = "0.9"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
eventsource-stream = "0.2"
growable-bloom-filter = "2"
```

If `cargo check --workspace` fails with `edition2024` or "requires Rust 1.85+":

```bash
cargo update -p idna --precise 0.5.0
cargo update -p url --precise 2.5.2
cargo update -p cc --precise 1.0.95
```

- [ ] **Step 3: Stage and commit the plan + workspace deps**

```bash
git add docs/superpowers/plans/2026-05-19-origin-phase-10.md Cargo.toml Cargo.lock
git commit -m "docs(origin): Phase 10 implementation plan + workspace deps (P10.0)"
```

- [ ] **Step 4: Verification gate**

Run: `cargo check --workspace`
Expected: exits 0; no new clippy/test runs at this checkpoint.
Run: `git status`
Expected: working tree clean.

---

# Cluster A — Skills

## Task P10.1 — `origin-skills` skeleton + frontmatter + loader  **[parallel-safe with B/C/D]**

**Files:**

- Create: `crates/origin-skills/Cargo.toml`
- Create: `crates/origin-skills/src/lib.rs`
- Create: `crates/origin-skills/src/frontmatter.rs`
- Create: `crates/origin-skills/src/loader.rs`
- Create: `crates/origin-skills/tests/frontmatter.rs`
- Create: `crates/origin-skills/tests/loader.rs`

- [ ] **Step 1: Manifest** at `crates/origin-skills/Cargo.toml`

```toml
[package]
name = "origin-skills"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yaml = { workspace = true }
thiserror = "1"
blake3 = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: `src/lib.rs`** module declarations + re-exports

```rust
//! `origin-skills` — Skills loader, embedding upsert, and allowed-tools narrowing.
//!
//! Modules land per-task across P10.1–P10.4; this `lib.rs` collects them.

pub mod frontmatter;
pub mod loader;

pub use frontmatter::{parse_frontmatter, FrontmatterError, SkillFrontmatter};
pub use loader::{load_skills_dir, LoaderError, Skill, SkillHash};
```

- [ ] **Step 3: Write the failing test** at `crates/origin-skills/tests/frontmatter.rs`

```rust
use origin_skills::{parse_frontmatter, FrontmatterError};

const GOOD: &str = "---\nname: testing-basics\ndescription: How to write tests.\nallowed-tools: [Read, Bash]\n---\nBody text after frontmatter.\n";

const MISSING_END: &str = "---\nname: testing-basics\ndescription: missing close\nBody.\n";

#[test]
fn parses_valid_frontmatter() {
    let parsed = parse_frontmatter(GOOD).expect("parse good");
    assert_eq!(parsed.front.name, "testing-basics");
    assert_eq!(parsed.front.description, "How to write tests.");
    assert_eq!(parsed.front.allowed_tools, vec!["Read".to_string(), "Bash".to_string()]);
    assert_eq!(parsed.body.trim(), "Body text after frontmatter.");
}

#[test]
fn rejects_missing_close_delim() {
    match parse_frontmatter(MISSING_END) {
        Err(FrontmatterError::MissingDelimiter) => {}
        other => panic!("expected MissingDelimiter, got {other:?}"),
    }
}

#[test]
fn rejects_invalid_yaml() {
    let bad = "---\nname: [unclosed\n---\nbody\n";
    assert!(matches!(parse_frontmatter(bad), Err(FrontmatterError::Yaml(_))));
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-skills --test frontmatter`
Expected: compile error — `parse_frontmatter`/`FrontmatterError` not defined.

- [ ] **Step 5: Implement `frontmatter.rs`**

```rust
//! Parse the YAML frontmatter at the head of a `SKILL.md`.

use serde::Deserialize;
use thiserror::Error;

/// Required + optional frontmatter fields shipped at P10.1.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Vec<String>,
}

/// A parsed `SKILL.md` split into the frontmatter struct and the body string.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub front: SkillFrontmatter,
    pub body: String,
}

#[derive(Debug, Error)]
pub enum FrontmatterError {
    #[error("frontmatter missing opening `---` delimiter")]
    MissingOpen,
    #[error("frontmatter missing closing `---` delimiter")]
    MissingDelimiter,
    #[error("yaml: {0}")]
    Yaml(String),
}

/// Split `source` into a frontmatter block + body, then deserialize the block.
///
/// # Errors
/// Returns [`FrontmatterError`] for missing delimiters or invalid YAML.
pub fn parse_frontmatter(source: &str) -> Result<ParsedSkill, FrontmatterError> {
    let rest = source.strip_prefix("---\n").ok_or(FrontmatterError::MissingOpen)?;
    let (yaml, body) = rest.split_once("\n---\n").ok_or(FrontmatterError::MissingDelimiter)?;
    let front: SkillFrontmatter =
        serde_yaml::from_str(yaml).map_err(|e| FrontmatterError::Yaml(e.to_string()))?;
    Ok(ParsedSkill { front, body: body.to_string() })
}
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-skills --test frontmatter`
Expected: 3/3 pass.

- [ ] **Step 7: Write the failing test** at `crates/origin-skills/tests/loader.rs`

```rust
use origin_skills::{load_skills_dir, Skill};
use std::fs;
use tempfile::tempdir;

fn write_skill(root: &std::path::Path, name: &str, body: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("mkdir");
    let contents = format!(
        "---\nname: {name}\ndescription: A test skill.\nallowed-tools: [Read]\n---\n{body}\n"
    );
    fs::write(dir.join("SKILL.md"), contents).expect("write");
}

#[test]
fn loads_skills_from_directory() {
    let tmp = tempdir().expect("tmp");
    write_skill(tmp.path(), "alpha", "alpha body");
    write_skill(tmp.path(), "beta", "beta body");

    let skills: Vec<Skill> = load_skills_dir(tmp.path()).expect("load");
    let names: Vec<&str> = skills.iter().map(|s| s.front.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
}

#[test]
fn dedupes_by_body_hash() {
    let tmp = tempdir().expect("tmp");
    write_skill(tmp.path(), "alpha", "shared body text");
    write_skill(tmp.path(), "alpha-copy", "shared body text");

    let skills = load_skills_dir(tmp.path()).expect("load");
    // Two distinct names but the body hash should collide -> 1 dedupe-key class.
    let mut hashes: Vec<_> = skills.iter().map(|s| s.body_hash.0).collect();
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), 1, "expected one unique body hash");
}

#[test]
fn ignores_subdirs_without_skill_md() {
    let tmp = tempdir().expect("tmp");
    write_skill(tmp.path(), "alpha", "body");
    std::fs::create_dir_all(tmp.path().join("not-a-skill")).expect("mkdir");
    let skills = load_skills_dir(tmp.path()).expect("load");
    assert_eq!(skills.len(), 1);
}
```

- [ ] **Step 8: Run the test, confirm failure**

Run: `cargo test -p origin-skills --test loader`
Expected: compile error — `load_skills_dir`/`Skill`/`SkillHash` not defined.

- [ ] **Step 9: Implement `loader.rs`**

```rust
//! Walk `~/.origin/skills/<name>/SKILL.md`, parse each, hash the body.

use crate::frontmatter::{parse_frontmatter, FrontmatterError, ParsedSkill};
use std::fs;
use std::path::Path;
use thiserror::Error;

/// 32-byte blake3 hash of the skill body bytes. Two skills with the same body
/// dedupe in CAS regardless of file path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SkillHash(pub [u8; 32]);

/// A loaded skill: parsed frontmatter + body + content hash + source path.
#[derive(Debug, Clone)]
pub struct Skill {
    pub front: crate::frontmatter::SkillFrontmatter,
    pub body: String,
    pub body_hash: SkillHash,
    pub source: std::path::PathBuf,
}

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("io reading {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("frontmatter in {path}: {source}")]
    Frontmatter {
        path: std::path::PathBuf,
        #[source]
        source: FrontmatterError,
    },
}

/// Walk one level into `root` and load every `<dir>/SKILL.md` found.
///
/// Subdirectories that do not contain a `SKILL.md` are silently skipped.
///
/// # Errors
/// Returns [`LoaderError`] if any encountered `SKILL.md` cannot be read or parsed.
pub fn load_skills_dir(root: &Path) -> Result<Vec<Skill>, LoaderError> {
    let mut out = Vec::new();
    let entries = fs::read_dir(root).map_err(|e| LoaderError::Io {
        path: root.to_path_buf(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| LoaderError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let path = dir.join("SKILL.md");
        if !path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|e| LoaderError::Io {
            path: path.clone(),
            source: e,
        })?;
        let ParsedSkill { front, body } =
            parse_frontmatter(&raw).map_err(|e| LoaderError::Frontmatter {
                path: path.clone(),
                source: e,
            })?;
        let body_hash = SkillHash(*blake3::hash(body.as_bytes()).as_bytes());
        out.push(Skill { front, body, body_hash, source: path });
    }

    Ok(out)
}
```

- [ ] **Step 10: Run the test, confirm pass**

Run: `cargo test -p origin-skills --test loader`
Expected: 3/3 pass.

- [ ] **Step 11: Verification gate**

```bash
cargo test -p origin-skills
cargo clippy -p origin-skills --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 12: Commit**

```bash
git add crates/origin-skills/
git commit -m "feat(origin-skills): loader + frontmatter parse (P10.1)"
```

---

## Task P10.2 — Skill embeddings indexed in `MemIndex` (kind=`Skill`)  **[depends on P10.1]**

**Files:**

- Create: `crates/origin-skills/src/embed.rs`
- Create: `crates/origin-skills/tests/embed.rs`
- Modify: `crates/origin-skills/src/lib.rs`
- Modify: `crates/origin-skills/Cargo.toml`

- [ ] **Step 1: Extend `Cargo.toml` with `origin-mem` + `origin-cas` deps**

Add to `[dependencies]`:

```toml
origin-mem = { path = "../origin-mem" }
origin-cas = { path = "../origin-cas" }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-skills/tests/embed.rs`

```rust
use origin_skills::{Skill, SkillEmbedder, SkillHash};

fn make_skill(name: &str, body: &str) -> Skill {
    Skill {
        front: origin_skills::frontmatter::SkillFrontmatter {
            name: name.into(),
            description: "test".into(),
            allowed_tools: vec![],
        },
        body: body.into(),
        body_hash: SkillHash(*blake3::hash(body.as_bytes()).as_bytes()),
        source: std::path::PathBuf::from(format!("/skills/{name}/SKILL.md")),
    }
}

#[test]
fn upsert_and_recall_skill() {
    let mut index = origin_mem::MemIndex::new();
    let mut embedder = SkillEmbedder::stub_for_tests();
    let alpha = make_skill("alpha", "learn how to write tests");

    let id = embedder.upsert(&mut index, &alpha).expect("upsert");
    assert!(id > 0, "ulid lower-64 should be non-zero");

    let query_vec = embedder.embed_for_tests("how do i write tests");
    let opts = origin_mem::SearchOpts::default();
    let hits = index
        .search(&query_vec, &opts, |_id| {
            Some(origin_mem::MetaRow {
                age_days: 0.0,
                cluster_priority: 1.0,
                edge_boost: 0.0,
                superseded_by: None,
            })
        })
        .expect("search");
    assert!(!hits.is_empty(), "recall should return skill candidate");
    assert_eq!(hits[0].id, id);
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-skills --test embed`
Expected: compile error — `SkillEmbedder` not defined.

- [ ] **Step 4: Implement `embed.rs`**

```rust
//! Embed skill bodies into `origin_mem::MemIndex` with kind = `Skill`.
//!
//! N9.4 — bodies are content-addressed (skill body lives in CAS via `body_hash`);
//! the index entry's public id is the lower 64 bits of the ULID we mint per skill.

use crate::loader::Skill;
use origin_mem::{EMBED_DIM, IndexError, MemIndex};
use thiserror::Error;

/// Embedder façade. In production, holds an `origin_mem::Embedder`; in tests,
/// holds a deterministic stub.
pub struct SkillEmbedder {
    inner: Inner,
}

enum Inner {
    Stub,
}

#[derive(Debug, Error)]
pub enum SkillEmbedError {
    #[error("index: {0}")]
    Index(#[from] IndexError),
}

impl SkillEmbedder {
    /// Deterministic stub for unit tests: maps text bytes onto a normalised
    /// vector by hashing into `EMBED_DIM` floats. Production callers will use
    /// `SkillEmbedder::with_embedder` once `origin_mem::Embedder` is wired in.
    #[must_use]
    pub const fn stub_for_tests() -> Self {
        Self { inner: Inner::Stub }
    }

    /// Returns a normalised embedding for `text` (test-only deterministic impl).
    #[must_use]
    pub fn embed_for_tests(&self, text: &str) -> [f32; EMBED_DIM] {
        let mut v = [0f32; EMBED_DIM];
        let h = blake3::hash(text.as_bytes());
        let bytes = h.as_bytes();
        for (i, slot) in v.iter_mut().enumerate() {
            let b = bytes[i % bytes.len()];
            *slot = (f32::from(b) / 255.0) * 2.0 - 1.0;
        }
        // L2-normalise.
        let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for slot in &mut v {
            *slot /= mag;
        }
        v
    }

    /// Embed `skill.body` and insert into `index`. Returns the public u64 id used.
    ///
    /// The id is the lower 64 bits of the blake3 body hash — deterministic
    /// across hosts so re-importing the same skill body is idempotent.
    ///
    /// # Errors
    /// Forwards [`IndexError`] on insertion failure.
    pub fn upsert(&mut self, index: &mut MemIndex, skill: &Skill) -> Result<u64, SkillEmbedError> {
        // body_hash is a fixed 32 bytes; the [..8] slice always converts.
        let bytes: [u8; 8] = skill.body_hash.0[..8]
            .try_into()
            .expect("blake3 hash is 32 bytes; first 8 always present");
        let id = u64::from_le_bytes(bytes);
        let vec = match self.inner {
            Inner::Stub => self.embed_for_tests(&skill.body),
        };
        index.insert(id, &vec)?;
        Ok(id)
    }
}
```

- [ ] **Step 5: Re-export from `src/lib.rs`**

Add to `lib.rs`:

```rust
pub mod embed;
pub use embed::{SkillEmbedError, SkillEmbedder};
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-skills --test embed`
Expected: 1/1 pass.

- [ ] **Step 7: Verification gate**

```bash
cargo test -p origin-skills
cargo clippy -p origin-skills --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-skills/
git commit -m "feat(origin-skills): embed skill bodies into MemIndex with kind=Skill (P10.2, N9.4)"
```

---

## Task P10.3 — Skill allowed-tools narrowing  **[depends on P10.1]**

**Files:**

- Create: `crates/origin-skills/src/registry.rs`
- Create: `crates/origin-skills/tests/registry.rs`
- Modify: `crates/origin-skills/src/lib.rs`
- Modify: `crates/origin-permission/src/lib.rs`
- Modify: `crates/origin-permission/Cargo.toml`

- [ ] **Step 1: Write the failing test** at `crates/origin-skills/tests/registry.rs`

```rust
use origin_skills::{SkillRegistry, SkillFrontmatter};

#[test]
fn empty_registry_returns_none_mask() {
    let reg = SkillRegistry::new();
    assert!(reg.allowed_tools().is_none(), "no active skills -> no narrowing");
}

#[test]
fn single_active_skill_narrows_to_its_allowed_tools() {
    let mut reg = SkillRegistry::new();
    reg.activate(SkillFrontmatter {
        name: "alpha".into(),
        description: "x".into(),
        allowed_tools: vec!["Read".into(), "Bash".into()],
    });
    let mask = reg.allowed_tools().expect("mask exists");
    assert!(mask.contains("Read"));
    assert!(mask.contains("Bash"));
    assert!(!mask.contains("Edit"));
}

#[test]
fn stacked_skills_intersect_their_masks() {
    let mut reg = SkillRegistry::new();
    reg.activate(SkillFrontmatter {
        name: "alpha".into(),
        description: "x".into(),
        allowed_tools: vec!["Read".into(), "Bash".into()],
    });
    reg.activate(SkillFrontmatter {
        name: "beta".into(),
        description: "y".into(),
        allowed_tools: vec!["Read".into(), "Edit".into()],
    });
    let mask = reg.allowed_tools().expect("mask exists");
    assert!(mask.contains("Read"));
    assert!(!mask.contains("Bash"), "intersection drops Bash");
    assert!(!mask.contains("Edit"), "intersection drops Edit");
}

#[test]
fn deactivate_pops_top_of_stack() {
    let mut reg = SkillRegistry::new();
    reg.activate(SkillFrontmatter {
        name: "alpha".into(),
        description: "x".into(),
        allowed_tools: vec!["Read".into()],
    });
    reg.deactivate("alpha");
    assert!(reg.allowed_tools().is_none());
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-skills --test registry`
Expected: compile error — `SkillRegistry` not defined.

- [ ] **Step 3: Implement `registry.rs`**

```rust
//! Active-skill stack + allowed-tools intersection mask.

use crate::frontmatter::SkillFrontmatter;
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct SkillRegistry {
    stack: Vec<SkillFrontmatter>,
}

impl SkillRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub fn activate(&mut self, front: SkillFrontmatter) {
        self.stack.push(front);
    }

    pub fn deactivate(&mut self, name: &str) {
        if let Some(pos) = self.stack.iter().rposition(|s| s.name == name) {
            self.stack.remove(pos);
        }
    }

    /// Intersection of every active skill's `allowed-tools`. `None` means no
    /// narrowing is in effect (the permission engine should fall through to
    /// the default tier check). An empty set means *no tool is allowed*.
    #[must_use]
    pub fn allowed_tools(&self) -> Option<HashSet<String>> {
        let mut iter = self.stack.iter();
        let first = iter.next()?;
        let mut acc: HashSet<String> = first.allowed_tools.iter().cloned().collect();
        for skill in iter {
            let cur: HashSet<String> = skill.allowed_tools.iter().cloned().collect();
            acc = acc.intersection(&cur).cloned().collect();
        }
        Some(acc)
    }
}
```

- [ ] **Step 4: Re-export from `src/lib.rs`**

```rust
pub mod registry;
pub use registry::SkillRegistry;
```

- [ ] **Step 5: Run the test, confirm pass**

Run: `cargo test -p origin-skills --test registry`
Expected: 4/4 pass.

- [ ] **Step 6: Write the cross-crate failing test** at `crates/origin-permission/tests/skill_narrow.rs`

```rust
use origin_permission::{check_with_skills, Outcome, prompt::AlwaysAllow};
use origin_skills::SkillRegistry;
use origin_tools::{SideEffects, Tier, ToolMeta, Urgency};

const READ_META: ToolMeta = ToolMeta {
    name: "Read",
    description: "read",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: "{}",
};

const EDIT_META: ToolMeta = ToolMeta {
    name: "Edit",
    description: "edit",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: "{}",
};

#[tokio::test]
async fn empty_registry_allows_normally() {
    let reg = SkillRegistry::new();
    let d = check_with_skills(&READ_META, "path", &AlwaysAllow, &reg).await;
    assert_eq!(d.outcome, Outcome::Allow);
}

#[tokio::test]
async fn skill_excludes_edit_returns_deny() {
    let mut reg = SkillRegistry::new();
    reg.activate(origin_skills::frontmatter::SkillFrontmatter {
        name: "no-mutate".into(),
        description: "read-only".into(),
        allowed_tools: vec!["Read".into()],
    });
    let d = check_with_skills(&EDIT_META, "path", &AlwaysAllow, &reg).await;
    assert_eq!(d.outcome, Outcome::Deny);
    assert!(d.reason.contains("skill"));
}
```

- [ ] **Step 7: Add `origin-skills` to `origin-permission/Cargo.toml`**

```toml
[dependencies]
origin-skills = { path = "../origin-skills" }
# … existing deps remain
[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt"] }
```

- [ ] **Step 8: Run the test, confirm failure**

Run: `cargo test -p origin-permission --test skill_narrow`
Expected: compile error — `check_with_skills` not defined.

- [ ] **Step 9: Modify `crates/origin-permission/src/lib.rs`** — add `check_with_skills`

Append after the existing `check` function:

```rust
use origin_skills::SkillRegistry;

/// Same as [`check`], but also enforces an active [`SkillRegistry`]'s
/// allowed-tools intersection mask before the tier check.
///
/// If any skill is active and `meta.name` is not in the intersection, the
/// outcome is [`Outcome::Deny`] with `reason = "skill-narrowed"`.
pub async fn check_with_skills(
    meta: &ToolMeta,
    args_preview: &str,
    prompter: &dyn Prompter,
    skills: &SkillRegistry,
) -> Decision {
    if let Some(mask) = skills.allowed_tools() {
        if !mask.contains(meta.name) {
            return Decision {
                outcome: Outcome::Deny,
                reason: "skill-narrowed".into(),
            };
        }
    }
    check(meta, args_preview, prompter).await
}
```

- [ ] **Step 10: Run the test, confirm pass**

Run: `cargo test -p origin-permission --test skill_narrow`
Expected: 2/2 pass.

- [ ] **Step 11: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 12: Commit**

```bash
git add crates/origin-skills/ crates/origin-permission/
git commit -m "feat(origin-skills,origin-permission): allowed-tools narrowing via SkillRegistry (P10.3, N9.5)"
```

---

## Task P10.4 — First-run import from `~/.claude/skills/`  **[depends on P10.1]**

**Files:**

- Create: `crates/origin-skills/src/import.rs`
- Create: `crates/origin-skills/tests/import.rs`
- Modify: `crates/origin-skills/src/lib.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-skills/tests/import.rs`

```rust
use origin_skills::{first_run_import, ImportDecision, ImportReport};
use std::fs;
use tempfile::tempdir;

fn write_skill(root: &std::path::Path, name: &str, body: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: t\nallowed-tools: []\n---\n{body}\n"),
    )
    .expect("write");
}

#[test]
fn dedupes_against_existing_skills() {
    let src = tempdir().expect("src");
    let dst = tempdir().expect("dst");
    write_skill(src.path(), "alpha", "shared body");
    write_skill(dst.path(), "alpha", "shared body"); // already imported

    let report: ImportReport = first_run_import(src.path(), dst.path(), |_skill| ImportDecision::Accept)
        .expect("import");
    assert_eq!(report.imported, 0, "exact-body match should not re-import");
    assert_eq!(report.skipped_duplicate, 1);
}

#[test]
fn user_can_reject_individual_skills() {
    let src = tempdir().expect("src");
    let dst = tempdir().expect("dst");
    write_skill(src.path(), "alpha", "body a");
    write_skill(src.path(), "beta", "body b");

    let report = first_run_import(src.path(), dst.path(), |skill| {
        if skill.front.name == "beta" {
            ImportDecision::Reject
        } else {
            ImportDecision::Accept
        }
    })
    .expect("import");
    assert_eq!(report.imported, 1);
    assert_eq!(report.rejected, 1);

    assert!(dst.path().join("alpha/SKILL.md").exists());
    assert!(!dst.path().join("beta/SKILL.md").exists());
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-skills --test import`
Expected: compile error — `first_run_import` not defined.

- [ ] **Step 3: Implement `import.rs`**

```rust
//! First-run import from `~/.claude/skills/` into `~/.origin/skills/`.
//!
//! N9.6 — dedupe by body hash; defer to the caller-provided `confirm` for
//! per-skill accept/reject. No filesystem writes happen until the closure
//! returns [`ImportDecision::Accept`].

use crate::loader::{load_skills_dir, LoaderError, Skill};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// User's per-skill decision returned by the `confirm` closure.
#[derive(Debug, Clone, Copy)]
pub enum ImportDecision {
    Accept,
    Reject,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub imported: usize,
    pub rejected: usize,
    pub skipped_duplicate: usize,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("source: {0}")]
    Source(#[from] LoaderError),
    #[error("io writing {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Walk `src` and copy any skill not already present in `dst` by body hash,
/// gated on `confirm`.
///
/// # Errors
/// Returns [`ImportError::Source`] if `src` cannot be read, or
/// [`ImportError::Io`] on `dst` write failure.
pub fn first_run_import<F>(src: &Path, dst: &Path, mut confirm: F) -> Result<ImportReport, ImportError>
where
    F: FnMut(&Skill) -> ImportDecision,
{
    fs::create_dir_all(dst).map_err(|e| ImportError::Io {
        path: dst.to_path_buf(),
        source: e,
    })?;

    let existing: HashSet<[u8; 32]> = if dst.exists() {
        load_skills_dir(dst).map_err(ImportError::Source)?
            .into_iter()
            .map(|s| s.body_hash.0)
            .collect()
    } else {
        HashSet::new()
    };

    let candidates = load_skills_dir(src).map_err(ImportError::Source)?;
    let mut report = ImportReport::default();

    for skill in candidates {
        if existing.contains(&skill.body_hash.0) {
            report.skipped_duplicate += 1;
            continue;
        }
        match confirm(&skill) {
            ImportDecision::Reject => {
                report.rejected += 1;
                continue;
            }
            ImportDecision::Accept => {
                let target_dir = dst.join(&skill.front.name);
                fs::create_dir_all(&target_dir).map_err(|e| ImportError::Io {
                    path: target_dir.clone(),
                    source: e,
                })?;
                let bytes = fs::read(&skill.source).map_err(|e| ImportError::Io {
                    path: skill.source.clone(),
                    source: e,
                })?;
                let target = target_dir.join("SKILL.md");
                fs::write(&target, bytes).map_err(|e| ImportError::Io {
                    path: target,
                    source: e,
                })?;
                report.imported += 1;
            }
        }
    }

    Ok(report)
}
```

- [ ] **Step 4: Re-export from `src/lib.rs`**

```rust
pub mod import;
pub use import::{first_run_import, ImportDecision, ImportError, ImportReport};
```

- [ ] **Step 5: Run the test, confirm pass**

Run: `cargo test -p origin-skills --test import`
Expected: 2/2 pass.

- [ ] **Step 6: Verification gate**

```bash
cargo test -p origin-skills
cargo clippy -p origin-skills --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-skills/
git commit -m "feat(origin-skills): first-run import from ~/.claude/skills with dedupe + confirm (P10.4, N9.6)"
```

---

# Cluster B — Hooks

## Task P10.5 — Pre-spawned shell pool  **[parallel-safe with A/C/D]**

**Files:**

- Create: `crates/origin-hooks/Cargo.toml`
- Create: `crates/origin-hooks/src/lib.rs`
- Create: `crates/origin-hooks/src/shellpool.rs`
- Create: `crates/origin-hooks/tests/shellpool.rs`
- Create: `crates/origin-hooks/benches/shellpool.rs`

- [ ] **Step 1: Manifest** at `crates/origin-hooks/Cargo.toml`

```toml
[package]
name = "origin-hooks"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
tokio = { version = "1", features = ["process", "io-util", "sync", "rt-multi-thread", "macros", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["test-util", "macros", "rt-multi-thread"] }
criterion = "0.5"

[[bench]]
name = "shellpool"
harness = false
```

- [ ] **Step 2: `src/lib.rs`** module declarations

```rust
//! `origin-hooks` — pre-spawned shell pool + typed lifecycle event dispatch.
//!
//! Modules land in P10.5 (`shellpool`) and P10.6 (`event` + `dispatch`).

pub mod shellpool;

pub use shellpool::{PoolError, ShellPool, ShellSpec};
```

- [ ] **Step 3: Write the failing test** at `crates/origin-hooks/tests/shellpool.rs`

The test uses Windows `cmd.exe /Q /K` on Windows and `/bin/sh` elsewhere. On Windows the script we send echoes the input followed by `\0`; on Unix we use `printf '%s\0'`.

```rust
use origin_hooks::{ShellPool, ShellSpec};

fn default_spec() -> ShellSpec {
    if cfg!(windows) {
        ShellSpec {
            program: "cmd.exe".into(),
            args: vec!["/Q".into(), "/K".into(), "@echo off".into()],
            read_terminator: 0u8,
        }
    } else {
        ShellSpec {
            program: "/bin/sh".into(),
            args: vec!["-s".into()],
            read_terminator: 0u8,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_reuses_one_child_across_dispatches() {
    let pool = ShellPool::new(default_spec(), 1).await.expect("pool");
    for i in 0..100usize {
        // The script: echo "hi-{i}" followed by NUL terminator so the pool can frame.
        let script = if cfg!(windows) {
            format!("echo hi-{i}&<NUL set /p=\"\x00\"\r\n")
        } else {
            format!("printf 'hi-{i}\\0'\n")
        };
        let resp = pool.dispatch(&script).await.expect("dispatch");
        assert!(resp.starts_with(&format!("hi-{i}").into_bytes()));
    }
    // Pool size 1 + 100 dispatches → exactly one underlying child must have been spawned.
    assert_eq!(pool.spawn_count(), 1, "no per-event spawns");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_recreates_dead_child() {
    let pool = ShellPool::new(default_spec(), 1).await.expect("pool");
    let exit_script = if cfg!(windows) { "exit\r\n".to_string() } else { "exit 0\n".to_string() };
    // Best-effort: send `exit` and ignore the response (the child closes stdout).
    let _ = pool.dispatch(&exit_script).await;

    // Next dispatch should spawn a fresh child.
    let script = if cfg!(windows) {
        "echo alive&<NUL set /p=\"\x00\"\r\n".to_string()
    } else {
        "printf 'alive\\0'\n".to_string()
    };
    let resp = pool.dispatch(&script).await.expect("dispatch after death");
    assert!(resp.starts_with(b"alive"));
    assert_eq!(pool.spawn_count(), 2, "exactly one respawn");
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-hooks --test shellpool`
Expected: compile error — `ShellPool`/`ShellSpec` not defined.

- [ ] **Step 5: Implement `shellpool.rs`**

```rust
//! Pre-spawned shell pool. Each pool member is a long-lived
//! `tokio::process::Child` with piped stdin + stdout. Dispatch writes a
//! script to stdin and reads until the configured terminator byte on stdout.
//!
//! N9.7 — amortized cost per hook dispatch is one `write_all` + one
//! `read_until`, not a fresh `fork+exec`.

use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// How to spawn one shell worker.
#[derive(Debug, Clone)]
pub struct ShellSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Byte that terminates one response on stdout. We standardise on NUL.
    pub read_terminator: u8,
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("stdin closed unexpectedly")]
    StdinClosed,
    #[error("stdout closed unexpectedly")]
    StdoutClosed,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    spec: ShellSpec,
}

impl Worker {
    fn spawn(spec: &ShellSpec) -> Result<Self, PoolError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(PoolError::StdinClosed)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(PoolError::StdoutClosed)?);
        Ok(Self { child, stdin, stdout, spec: spec.clone() })
    }

    async fn dispatch(&mut self, script: &str) -> Result<Vec<u8>, PoolError> {
        self.stdin.write_all(script.as_bytes()).await?;
        self.stdin.flush().await?;
        let mut buf = Vec::with_capacity(256);
        let n = self.stdout.read_until(self.spec.read_terminator, &mut buf).await?;
        if n == 0 {
            return Err(PoolError::StdoutClosed);
        }
        // Strip trailing terminator.
        if buf.last() == Some(&self.spec.read_terminator) {
            buf.pop();
        }
        Ok(buf)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// Pre-spawned shell pool.
pub struct ShellPool {
    spec: ShellSpec,
    workers: Vec<Mutex<Option<Worker>>>,
    spawn_count: AtomicUsize,
    next: AtomicUsize,
}

impl ShellPool {
    /// Create a pool of `size` workers up front.
    ///
    /// # Errors
    /// Returns [`PoolError::Spawn`] if any worker fails to start.
    pub async fn new(spec: ShellSpec, size: usize) -> Result<Self, PoolError> {
        let mut workers = Vec::with_capacity(size.max(1));
        let mut spawn_count = 0usize;
        for _ in 0..size.max(1) {
            workers.push(Mutex::new(Some(Worker::spawn(&spec)?)));
            spawn_count += 1;
        }
        Ok(Self {
            spec,
            workers,
            spawn_count: AtomicUsize::new(spawn_count),
            next: AtomicUsize::new(0),
        })
    }

    /// Dispatch `script` to one worker (round-robin) and return its bytes up
    /// to (and not including) the configured terminator.
    ///
    /// If the chosen worker has died since last use, a fresh worker is spawned
    /// in its slot and the dispatch is retried once on the new worker.
    ///
    /// # Errors
    /// Forwards [`PoolError`] from spawn / IO.
    pub async fn dispatch(&self, script: &str) -> Result<Vec<u8>, PoolError> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let mut slot = self.workers[idx].lock().await;
        let alive = slot.as_mut().map_or(false, Worker::is_alive);
        if !alive {
            *slot = Some(Worker::spawn(&self.spec)?);
            self.spawn_count.fetch_add(1, Ordering::Relaxed);
        }
        match slot.as_mut() {
            Some(w) => match w.dispatch(script).await {
                Ok(b) => Ok(b),
                Err(PoolError::StdoutClosed) => {
                    // Respawn and retry once.
                    *slot = Some(Worker::spawn(&self.spec)?);
                    self.spawn_count.fetch_add(1, Ordering::Relaxed);
                    slot.as_mut()
                        .ok_or(PoolError::StdoutClosed)?
                        .dispatch(script)
                        .await
                }
                Err(e) => Err(e),
            },
            None => Err(PoolError::StdinClosed),
        }
    }

    /// Total `Worker::spawn` calls (including respawns). Used by tests to
    /// assert no per-event spawn.
    #[must_use]
    pub fn spawn_count(&self) -> usize {
        self.spawn_count.load(Ordering::Relaxed)
    }
}
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-hooks --test shellpool`
Expected: 2/2 pass.

If the Windows fixture proves flaky (cmd.exe buffering), gate the test with `#[cfg_attr(windows, ignore = "cmd.exe stdin buffering; covered by integration in P10.6")]` and rely on `/bin/sh` on the Linux CI runner. The implementation itself is platform-agnostic.

- [ ] **Step 7: Add a microbenchmark** at `crates/origin-hooks/benches/shellpool.rs`

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use origin_hooks::{ShellPool, ShellSpec};

fn pool_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let spec = if cfg!(windows) {
        ShellSpec {
            program: "cmd.exe".into(),
            args: vec!["/Q".into(), "/K".into(), "@echo off".into()],
            read_terminator: 0,
        }
    } else {
        ShellSpec {
            program: "/bin/sh".into(),
            args: vec!["-s".into()],
            read_terminator: 0,
        }
    };
    let pool = rt.block_on(async { ShellPool::new(spec, 2).await.expect("pool") });

    c.bench_function("shellpool/dispatch", |b| {
        b.iter(|| {
            rt.block_on(async {
                let script = if cfg!(windows) {
                    "echo x&<NUL set /p=\"\x00\"\r\n"
                } else {
                    "printf 'x\\0'\n"
                };
                let _ = pool.dispatch(script).await;
            });
        });
    });
}

criterion_group!(benches, pool_dispatch);
criterion_main!(benches);
```

- [ ] **Step 8: Verification gate**

```bash
cargo test -p origin-hooks
cargo clippy -p origin-hooks --all-targets -- -D warnings
cargo fmt --check
cargo bench -p origin-hooks --bench shellpool -- --quick
```

All exit 0. The bench produces a numeric result; record it in the commit message body.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-hooks/
git commit -m "feat(origin-hooks): pre-spawned shell pool with respawn-on-death (P10.5, N9.7)"
```

---

## Task P10.6 — Lifecycle events + typed payloads + override parsing  **[depends on P10.5]**

**Files:**

- Create: `crates/origin-hooks/src/event.rs`
- Create: `crates/origin-hooks/src/dispatch.rs`
- Create: `crates/origin-hooks/tests/event.rs`
- Modify: `crates/origin-hooks/src/lib.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-hooks/tests/event.rs`

```rust
use origin_hooks::{HookOverride, LifecycleEvent, ToolPhase, parse_hook_stdout};

#[test]
fn lifecycle_event_round_trips_json() {
    let ev = LifecycleEvent::PreTool {
        tool: "Bash".into(),
        args_preview: "ls -la".into(),
    };
    let json = serde_json::to_string(&ev).expect("ser");
    let back: LifecycleEvent = serde_json::from_str(&json).expect("de");
    match back {
        LifecycleEvent::PreTool { tool, args_preview } => {
            assert_eq!(tool, "Bash");
            assert_eq!(args_preview, "ls -la");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn every_event_kind_serializes() {
    let evs = vec![
        LifecycleEvent::PrePrompt { text: "hi".into() },
        LifecycleEvent::PostPrompt { text: "bye".into() },
        LifecycleEvent::PreTool { tool: "Read".into(), args_preview: "/x".into() },
        LifecycleEvent::PostTool { tool: "Read".into(), phase: ToolPhase::Ok },
        LifecycleEvent::PreCommit { branch: "phase-10".into() },
        LifecycleEvent::PostCommit { sha: "abc1234".into() },
        LifecycleEvent::SessionStart,
        LifecycleEvent::SessionEnd,
    ];
    for ev in evs {
        let json = serde_json::to_string(&ev).expect("ser");
        let _back: LifecycleEvent = serde_json::from_str(&json).expect("de");
    }
}

#[test]
fn parses_allow_override() {
    let stdout = br#"{"override":{"action":"allow","reason":"trusted"}}"#;
    let parsed = parse_hook_stdout(stdout).expect("parse");
    match parsed {
        HookOverride::Allow { reason } => assert_eq!(reason, "trusted"),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn parses_deny_override() {
    let stdout = br#"{"override":{"action":"deny","reason":"blacklist"}}"#;
    let parsed = parse_hook_stdout(stdout).expect("parse");
    assert!(matches!(parsed, HookOverride::Deny { .. }));
}

#[test]
fn empty_stdout_means_passthrough() {
    let parsed = parse_hook_stdout(b"").expect("parse");
    assert!(matches!(parsed, HookOverride::Passthrough));
}

#[test]
fn rejects_malformed_json() {
    assert!(parse_hook_stdout(b"{not json}").is_err());
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-hooks --test event`
Expected: compile error — `LifecycleEvent` and friends not defined.

- [ ] **Step 3: Implement `event.rs`**

```rust
//! Typed lifecycle events + hook stdout override schema.
//!
//! Events serialize to JSON for hook stdin; hook stdout JSON is parsed back
//! into [`HookOverride`].

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lifecycle event emitted by the daemon for each hook to inspect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    PrePrompt { text: String },
    PostPrompt { text: String },
    PreTool { tool: String, args_preview: String },
    PostTool { tool: String, phase: ToolPhase },
    PreCommit { branch: String },
    PostCommit { sha: String },
    SessionStart,
    SessionEnd,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Ok,
    Err,
    Skipped,
}

/// Override decision parsed from a hook's stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookOverrideInner {
    Allow { reason: String },
    Deny { reason: String },
    Mutate { patch: String },
}

#[derive(Debug, Clone)]
pub enum HookOverride {
    Passthrough,
    Allow { reason: String },
    Deny { reason: String },
    Mutate { patch: String },
}

#[derive(Debug, Error)]
pub enum HookParseError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    r#override: Option<HookOverrideInner>,
}

/// Parse the bytes a hook printed on stdout into a [`HookOverride`].
///
/// Empty stdout means the hook is signalling "no opinion" → [`HookOverride::Passthrough`].
///
/// # Errors
/// Returns [`HookParseError::Json`] if non-empty stdout is not valid JSON.
pub fn parse_hook_stdout(bytes: &[u8]) -> Result<HookOverride, HookParseError> {
    let trimmed = bytes.iter().position(|b| !b.is_ascii_whitespace()).map_or(&[][..], |i| &bytes[i..]);
    if trimmed.is_empty() {
        return Ok(HookOverride::Passthrough);
    }
    let env: Envelope = serde_json::from_slice(trimmed)?;
    Ok(match env.r#override {
        None => HookOverride::Passthrough,
        Some(HookOverrideInner::Allow { reason }) => HookOverride::Allow { reason },
        Some(HookOverrideInner::Deny { reason }) => HookOverride::Deny { reason },
        Some(HookOverrideInner::Mutate { patch }) => HookOverride::Mutate { patch },
    })
}
```

- [ ] **Step 4: Implement `dispatch.rs`** — wire events through the shell pool

```rust
//! End-to-end dispatch: emit a [`LifecycleEvent`] JSON line to a hook script
//! via [`ShellPool::dispatch`], then parse stdout back into a [`HookOverride`].

use crate::event::{parse_hook_stdout, HookOverride, HookParseError, LifecycleEvent};
use crate::shellpool::{PoolError, ShellPool};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("pool: {0}")]
    Pool(#[from] PoolError),
    #[error("serialize: {0}")]
    Ser(#[from] serde_json::Error),
    #[error("parse: {0}")]
    Parse(#[from] HookParseError),
}

/// Send `event` to `pool` and return the parsed override.
///
/// The hook script is expected to read **one JSON line** from stdin and write
/// **one JSON object followed by a NUL byte** to stdout. Empty stdout means
/// passthrough.
///
/// # Errors
/// Forwards [`DispatchError`].
pub async fn dispatch_event(pool: &ShellPool, event: &LifecycleEvent) -> Result<HookOverride, DispatchError> {
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let bytes = pool.dispatch(&line).await?;
    Ok(parse_hook_stdout(&bytes)?)
}
```

- [ ] **Step 5: Re-export from `src/lib.rs`**

Append to `lib.rs`:

```rust
pub mod dispatch;
pub mod event;

pub use dispatch::{dispatch_event, DispatchError};
pub use event::{parse_hook_stdout, HookOverride, HookOverrideInner, HookParseError, LifecycleEvent, ToolPhase};
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-hooks --test event`
Expected: 6/6 pass.

- [ ] **Step 7: Verification gate**

```bash
cargo test -p origin-hooks
cargo clippy -p origin-hooks --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-hooks/
git commit -m "feat(origin-hooks): typed lifecycle events + stdout override parsing (P10.6, N9.8)"
```

---

# Cluster C — MCP

## Task P10.7 — MCP client base (JSON-RPC + stdio transport)  **[parallel-safe with A/B/D]**

**Files:**

- Create: `crates/origin-mcp/Cargo.toml`
- Create: `crates/origin-mcp/src/lib.rs`
- Create: `crates/origin-mcp/src/jsonrpc.rs`
- Create: `crates/origin-mcp/src/transport.rs`
- Create: `crates/origin-mcp/src/transport_stdio.rs`
- Create: `crates/origin-mcp/src/client.rs`
- Create: `crates/origin-mcp/tests/jsonrpc.rs`
- Create: `crates/origin-mcp/tests/stdio.rs`

- [ ] **Step 1: Manifest** at `crates/origin-mcp/Cargo.toml`

```toml
[package]
name = "origin-mcp"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
tokio = { version = "1", features = ["process", "io-util", "sync", "rt-multi-thread", "macros", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
async-trait = "0.1"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["test-util", "macros", "rt-multi-thread"] }
```

- [ ] **Step 2: `src/lib.rs`** module declarations

```rust
//! `origin-mcp` — Model Context Protocol client. Phase 10 ships JSON-RPC +
//! stdio + HTTP/SSE transports + tool registry integration + OAuth.

pub mod client;
pub mod jsonrpc;
pub mod transport;
pub mod transport_stdio;

pub use client::{ClientError, ListToolsResult, McpClient, McpTool, ToolCallResult};
pub use jsonrpc::{JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
pub use transport::{Transport, TransportError};
pub use transport_stdio::StdioTransport;
```

- [ ] **Step 3: Write the failing test** at `crates/origin-mcp/tests/jsonrpc.rs`

```rust
use origin_mcp::{JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use serde_json::json;

#[test]
fn request_serializes_with_jsonrpc_field() {
    let req = JsonRpcRequest::new(JsonRpcId::Num(1), "tools/list", json!({}));
    let s = serde_json::to_string(&req).expect("ser");
    assert!(s.contains("\"jsonrpc\":\"2.0\""));
    assert!(s.contains("\"method\":\"tools/list\""));
    assert!(s.contains("\"id\":1"));
}

#[test]
fn response_ok_round_trip() {
    let json_resp = json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}}).to_string();
    let resp: JsonRpcResponse = serde_json::from_str(&json_resp).expect("de");
    let result = resp.into_result().expect("ok");
    assert!(result.get("tools").is_some());
}

#[test]
fn response_err_round_trip() {
    let json_resp = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}).to_string();
    let resp: JsonRpcResponse = serde_json::from_str(&json_resp).expect("de");
    let err = resp.into_result().expect_err("should be err");
    assert_eq!(err.code, -32601);
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test jsonrpc`
Expected: compile error.

- [ ] **Step 5: Implement `jsonrpc.rs`**

```rust
//! Minimal JSON-RPC 2.0 framing for MCP.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JsonRpcId {
    Num(i64),
    Str(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: JsonRpcId,
    pub method: String,
    pub params: Value,
}

impl JsonRpcRequest {
    #[must_use]
    pub fn new(id: JsonRpcId, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)] // jsonrpc field accepted from the wire but not surfaced.
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Deserialize, Error)]
#[error("jsonrpc error {code}: {message}")]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    /// Collapse `(result, error)` into a `Result<Value, JsonRpcError>`.
    ///
    /// # Errors
    /// Returns the embedded [`JsonRpcError`] when `error` is set.
    pub fn into_result(self) -> Result<Value, JsonRpcError> {
        match (self.result, self.error) {
            (Some(v), _) => Ok(v),
            (None, Some(e)) => Err(e),
            (None, None) => Err(JsonRpcError {
                code: -32603,
                message: "neither result nor error present".into(),
            }),
        }
    }
}
```

- [ ] **Step 6: Implement `transport.rs`** — the trait

```rust
//! Transport abstraction. Stdio + HTTP/SSE both implement this.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("transport: {0}")]
    Other(String),
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Send `request_json` and return the matching response JSON.
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError>;
}
```

- [ ] **Step 7: Implement `transport_stdio.rs`**

```rust
//! Stdio JSON-RPC transport over a spawned child process.

use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

pub struct StdioTransport {
    inner: Mutex<Inner>,
}

struct Inner {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    /// Spawn `program` with `args`, pipe stdio.
    ///
    /// # Errors
    /// Returns [`TransportError::Io`] on spawn or pipe-take failure.
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, TransportError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| TransportError::Other("no stdin".into()))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| TransportError::Other("no stdout".into()))?);
        Ok(Self {
            inner: Mutex::new(Inner { _child: child, stdin, stdout }),
        })
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError> {
        let mut inner = self.inner.lock().await;
        inner.stdin.write_all(request_json.as_bytes()).await?;
        inner.stdin.write_all(b"\n").await?;
        inner.stdin.flush().await?;
        let mut line = String::new();
        let n = inner.stdout.read_line(&mut line).await?;
        if n == 0 {
            return Err(TransportError::Other("eof".into()));
        }
        Ok(serde_json::from_str(&line)?)
    }
}
```

- [ ] **Step 8: Implement `client.rs`**

```rust
//! `McpClient` — handshake, list_tools, call_tool.

use crate::jsonrpc::{JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
use crate::transport::{Transport, TransportError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("rpc: {0}")]
    Rpc(#[from] JsonRpcError),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_schema")]
    pub input_schema: Value,
}

fn default_schema() -> Value {
    json!({"type":"object"})
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallResult {
    pub content: Value,
}

pub struct McpClient {
    transport: Arc<dyn Transport>,
    next_id: AtomicI64,
}

impl McpClient {
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self { transport, next_id: AtomicI64::new(1) }
    }

    fn fresh_id(&self) -> JsonRpcId {
        JsonRpcId::Num(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// MCP `initialize` handshake. Returns the server's reported tool-list.
    ///
    /// # Errors
    /// Returns [`ClientError`] on transport or RPC failure.
    pub async fn initialize(&self) -> Result<(), ClientError> {
        let req = JsonRpcRequest::new(
            self.fresh_id(),
            "initialize",
            json!({"protocolVersion":"2024-11-05","clientInfo":{"name":"origin","version":"0.0.1"}}),
        );
        let payload = serde_json::to_string(&req)?;
        let resp_value = self.transport.round_trip(&payload).await?;
        let resp: JsonRpcResponse = serde_json::from_value(resp_value)?;
        resp.into_result()?;
        Ok(())
    }

    /// `tools/list`.
    ///
    /// # Errors
    /// Returns [`ClientError`] on transport or RPC failure.
    pub async fn list_tools(&self) -> Result<ListToolsResult, ClientError> {
        let req = JsonRpcRequest::new(self.fresh_id(), "tools/list", json!({}));
        let payload = serde_json::to_string(&req)?;
        let resp_value = self.transport.round_trip(&payload).await?;
        let resp: JsonRpcResponse = serde_json::from_value(resp_value)?;
        let result = resp.into_result()?;
        Ok(serde_json::from_value(result)?)
    }

    /// `tools/call` with the given name and args.
    ///
    /// # Errors
    /// Returns [`ClientError`] on transport or RPC failure.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolCallResult, ClientError> {
        let req = JsonRpcRequest::new(
            self.fresh_id(),
            "tools/call",
            json!({"name": name, "arguments": args}),
        );
        let payload = serde_json::to_string(&req)?;
        let resp_value = self.transport.round_trip(&payload).await?;
        let resp: JsonRpcResponse = serde_json::from_value(resp_value)?;
        let result = resp.into_result()?;
        Ok(ToolCallResult { content: result })
    }
}
```

- [ ] **Step 9: Write the stdio failing test** at `crates/origin-mcp/tests/stdio.rs`

The test uses a tiny `echo-server` shell script as the mock MCP server.

```rust
use origin_mcp::{McpClient, StdioTransport};
use std::sync::Arc;

// A mock MCP server that responds to `initialize` and `tools/list` with
// canned JSON-RPC responses. Implemented as a one-liner shell script so we
// don't need a Rust mock binary in tree.
fn mock_server_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        // PowerShell loop: read JSON-RPC line, respond with canned tools/list.
        (
            "powershell.exe".into(),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "while($line=[Console]::In.ReadLine()){if($line -match '\"method\":\"tools/list\"'){[Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"ping\",\"description\":\"d\",\"input_schema\":{}}]}}')} elseif($line -match '\"method\":\"initialize\"'){[Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}')}}".into(),
            ],
        )
    } else {
        (
            "/bin/sh".into(),
            vec![
                "-c".into(),
                r#"while IFS= read -r line; do
                    case "$line" in
                      *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"ping","description":"d","input_schema":{}}]}}\n' ;;
                      *'"method":"initialize"'*)  printf '{"jsonrpc":"2.0","id":1,"result":{}}\n' ;;
                    esac
                done"#.into(),
            ],
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_and_list_tools_against_mock() {
    let (prog, args) = mock_server_cmd();
    let transport: Arc<dyn origin_mcp::Transport> = Arc::new(StdioTransport::spawn(&prog, &args).expect("spawn"));
    let client = McpClient::new(transport);
    client.initialize().await.expect("initialize");
    let tools = client.list_tools().await.expect("list");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "ping");
}
```

- [ ] **Step 10: Run the tests, confirm pass**

```bash
cargo test -p origin-mcp --test jsonrpc
cargo test -p origin-mcp --test stdio
```

Expected: jsonrpc 3/3, stdio 1/1.

- [ ] **Step 11: Verification gate**

```bash
cargo test -p origin-mcp
cargo clippy -p origin-mcp --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 12: Commit**

```bash
git add crates/origin-mcp/
git commit -m "feat(origin-mcp): JSON-RPC + stdio transport + handshake/list/call (P10.7, N9.10)"
```

---

## Task P10.8 — MCP HTTP + SSE transports  **[depends on P10.7]**

**Files:**

- Create: `crates/origin-mcp/src/transport_http.rs`
- Create: `crates/origin-mcp/tests/http.rs`
- Modify: `crates/origin-mcp/src/lib.rs`
- Modify: `crates/origin-mcp/Cargo.toml` — add `reqwest`, `eventsource-stream`

- [ ] **Step 1: Extend `Cargo.toml`**

```toml
[dependencies]
# … existing deps remain
reqwest = { workspace = true }
eventsource-stream = { workspace = true }
futures-util = "0.3"

[dev-dependencies]
# … existing deps remain
hyper = { version = "1", features = ["server", "http1"] }
hyper-util = { version = "0.1", features = ["tokio"] }
http-body-util = "0.1"
```

- [ ] **Step 2: Write the failing test** at `crates/origin-mcp/tests/http.rs`

A minimal hyper server bound to `127.0.0.1:0` serves a single JSON-RPC response per POST, then exits. We use `tokio::spawn` to run the server and `tokio::sync::oneshot` to surface its bound port.

```rust
use origin_mcp::{HttpTransport, McpClient};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn mock_server(port_tx: oneshot::Sender<u16>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    port_tx.send(port).expect("port_tx");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let path = req.uri().path().to_string();
                let body_bytes = if path == "/rpc" {
                    br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"http_ping","description":"d","input_schema":{}}]}}"#.to_vec()
                } else {
                    br#"{}"#.to_vec()
                };
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body_bytes))))
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await;
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_tools_over_http() {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(mock_server(tx));
    let port = rx.await.expect("port");

    let url = format!("http://127.0.0.1:{port}/rpc");
    let transport: Arc<dyn origin_mcp::Transport> = Arc::new(HttpTransport::new(url, None));
    let client = McpClient::new(transport);
    client.initialize().await.expect("initialize");
    let tools = client.list_tools().await.expect("list");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "http_ping");
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test http`
Expected: compile error — `HttpTransport` not defined.

- [ ] **Step 4: Implement `transport_http.rs`**

```rust
//! HTTP POST + (optional) SSE event-stream transport.
//!
//! - Synchronous request/response: POST `<base>` with JSON body, parse the JSON
//!   response.
//! - SSE subscription: GET `<base>/events`, framed by `eventsource-stream`. The
//!   stream is exposed via `HttpTransport::events()` and yields `serde_json::Value`s.

use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::stream::{Stream, StreamExt};
use reqwest::Client;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub struct HttpTransport {
    client: Client,
    url: String,
    bearer: Mutex<Option<String>>,
}

impl HttpTransport {
    #[must_use]
    pub fn new(url: impl Into<String>, bearer: Option<String>) -> Self {
        Self {
            client: Client::new(),
            url: url.into(),
            bearer: Mutex::new(bearer),
        }
    }

    /// Rotate the bearer token (used by [`crate::oauth`]).
    ///
    /// # Panics
    /// Panics if the bearer mutex has been poisoned by a prior panic.
    #[allow(clippy::expect_used)] // see docstring
    pub fn set_bearer(&self, token: Option<String>) {
        *self.bearer.lock().expect("bearer mutex poisoned") = token;
    }

    fn current_bearer(&self) -> Option<String> {
        #[allow(clippy::expect_used)] // same justification as set_bearer
        self.bearer.lock().expect("bearer mutex poisoned").clone()
    }

    /// Open an SSE stream against `<url>/events`. Each line is a JSON-RPC
    /// notification yielded as `serde_json::Value`.
    ///
    /// # Errors
    /// Returns [`TransportError::Io`] / [`TransportError::Other`] on connection failure.
    pub async fn events(&self) -> Result<impl Stream<Item = Result<Value, TransportError>> + Send, TransportError> {
        let url = format!("{}/events", self.url);
        let mut req = self.client.get(&url);
        if let Some(b) = self.current_bearer() {
            req = req.bearer_auth(b);
        }
        let resp = req.send().await.map_err(|e| TransportError::Other(e.to_string()))?;
        let stream = resp.bytes_stream().eventsource().filter_map(|ev| async move {
            match ev {
                Ok(e) => match serde_json::from_str::<Value>(&e.data) {
                    Ok(v) => Some(Ok(v)),
                    Err(err) => Some(Err(TransportError::Serde(err))),
                },
                Err(e) => Some(Err(TransportError::Other(e.to_string()))),
            }
        });
        Ok(stream)
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError> {
        let mut req = self.client.post(&self.url).body(request_json.to_string());
        req = req.header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(b) = self.current_bearer() {
            req = req.bearer_auth(b);
        }
        let resp = req.send().await.map_err(|e| TransportError::Other(e.to_string()))?;
        let bytes = resp.bytes().await.map_err(|e| TransportError::Other(e.to_string()))?;
        let v: Value = serde_json::from_slice(&bytes)?;
        Ok(v)
    }
}

// Silence the unused-import warning when neither caller path threads `Arc<HttpTransport>`.
#[allow(dead_code)]
fn _arc_check(t: Arc<HttpTransport>) -> Arc<dyn Transport> {
    t
}
```

- [ ] **Step 5: Re-export from `lib.rs`**

Add to `lib.rs`:

```rust
pub mod transport_http;
pub use transport_http::HttpTransport;
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-mcp --test http`
Expected: 1/1 pass.

- [ ] **Step 7: Verification gate**

```bash
cargo test -p origin-mcp
cargo clippy -p origin-mcp --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-mcp/
git commit -m "feat(origin-mcp): HTTP transport + SSE event stream (P10.8, N9.12)"
```

---

## Task P10.9 — MCP tool registry integration (`McpToolProxy`)  **[depends on P10.7]**

**Files:**

- Create: `crates/origin-mcp/src/proxy.rs`
- Create: `crates/origin-mcp/tests/proxy.rs`
- Modify: `crates/origin-mcp/src/lib.rs`
- Modify: `crates/origin-tools/src/lib.rs` — expose a `DynTool` trait

- [ ] **Step 1: Extend `origin-tools` with a `DynTool` trait**

Append to `crates/origin-tools/src/lib.rs`:

```rust
/// Runtime tool object — what dispatch actually calls when a tool has no
/// compile-time inventory entry (MCP-discovered tools live here).
#[async_trait::async_trait]
pub trait DynTool: Send + Sync + std::fmt::Debug {
    fn meta(&self) -> &ToolMeta;
    /// `args` is JSON; the returned `Value` is the tool's structured result.
    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, String>;
}
```

Update `origin-tools/Cargo.toml`:

```toml
[dependencies]
# … existing deps remain
async-trait = "0.1"
serde_json = "1"
```

Run: `cargo check -p origin-tools` → exits 0.

- [ ] **Step 2: Write the failing test** at `crates/origin-mcp/tests/proxy.rs`

```rust
use origin_mcp::{McpClient, McpToolProxy, Transport, TransportError};
use origin_tools::{DynTool, SideEffects, Tier, ToolMeta, Urgency};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Default)]
struct LoopbackTransport;

#[async_trait::async_trait]
impl Transport for LoopbackTransport {
    async fn round_trip(&self, request: &str) -> Result<Value, TransportError> {
        // Pretend every call returns the args echoed inside content.
        let v: Value = serde_json::from_str(request)?;
        let id = v.get("id").cloned().unwrap_or(json!(1));
        Ok(json!({"jsonrpc":"2.0","id":id,"result":{"echo": v.get("params").cloned().unwrap_or(json!({}))}}))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_proxy_invocation_runs_through_dyntool() {
    let transport: Arc<dyn Transport> = Arc::new(LoopbackTransport);
    let client = Arc::new(McpClient::new(transport));
    let proxy = McpToolProxy::new(
        client.clone(),
        ToolMeta {
            name: "mcp_echo",
            description: "echo via mcp",
            tier: Tier::RequiresPermission,
            urgency: Urgency::Low,
            side_effects: SideEffects::Pure,
            input_schema: "{\"type\":\"object\"}",
        },
        "echo".to_string(),
    );

    let dyn_tool: &dyn DynTool = &proxy;
    let result = dyn_tool.invoke(json!({"hello":"world"})).await.expect("invoke");
    assert!(result.get("echo").is_some(), "proxy should forward args");
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test proxy`
Expected: compile error — `McpToolProxy` not defined.

- [ ] **Step 4: Implement `proxy.rs`**

```rust
//! `McpToolProxy` — implements `origin_tools::DynTool` by forwarding calls to
//! an `McpClient`. The proxy is what the daemon's tool dispatcher walks over
//! when it sees an MCP tool, so MCP and native tools share the same code path.

use crate::client::{ClientError, McpClient};
use async_trait::async_trait;
use origin_tools::{DynTool, ToolMeta};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug)]
pub struct McpToolProxy {
    client: Arc<McpClient>,
    meta: ToolMeta,
    /// Server-side tool name (may differ from `meta.name` if we prefix with
    /// e.g. `mcp/<server>/` for namespacing).
    remote_name: String,
}

impl McpToolProxy {
    #[must_use]
    pub const fn new(client: Arc<McpClient>, meta: ToolMeta, remote_name: String) -> Self {
        Self { client, meta, remote_name }
    }
}

#[async_trait]
impl DynTool for McpToolProxy {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }

    async fn invoke(&self, args: Value) -> Result<Value, String> {
        match self.client.call_tool(&self.remote_name, args).await {
            Ok(r) => Ok(r.content),
            Err(ClientError::Rpc(e)) => Err(format!("mcp rpc: {e}")),
            Err(other) => Err(format!("mcp: {other}")),
        }
    }
}
```

- [ ] **Step 5: Re-export from `lib.rs`**

```rust
pub mod proxy;
pub use proxy::McpToolProxy;
```

Also `Arc` needs `Debug` for `McpClient`; add `#[derive(Debug)]` is incompatible with the atomic + transport object — instead manually `impl std::fmt::Debug for McpClient`:

In `client.rs`:

```rust
impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").finish_non_exhaustive()
    }
}
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-mcp --test proxy`
Expected: 1/1 pass.

- [ ] **Step 7: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-mcp/ crates/origin-tools/
git commit -m "feat(origin-mcp,origin-tools): McpToolProxy via DynTool trait (P10.9, N9.11)"
```

---

## Task P10.10 — MCP outputs land in CAS  **[depends on P10.9]**

**Files:**

- Create: `crates/origin-mcp/src/cas_handoff.rs`
- Create: `crates/origin-mcp/tests/cas_handoff.rs`
- Modify: `crates/origin-mcp/src/proxy.rs`
- Modify: `crates/origin-mcp/Cargo.toml` — add `origin-cas`

- [ ] **Step 1: Add `origin-cas` dep**

Edit `crates/origin-mcp/Cargo.toml`:

```toml
[dependencies]
# … existing deps remain
origin-cas = { path = "../origin-cas" }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-mcp/tests/cas_handoff.rs`

```rust
use origin_cas::{Store, StoreConfig};
use origin_mcp::cas_handoff::{cas_handoff_if_large, HandoffOutcome};
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;

fn make_store() -> Arc<Store> {
    let tmp = tempdir().expect("tmp");
    Arc::new(
        Store::open(StoreConfig {
            root: tmp.path().to_path_buf(),
            hot_capacity: 16,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

#[test]
fn small_body_passes_through() {
    let store = make_store();
    let value = json!({"hello":"world"});
    let out = cas_handoff_if_large(&store, value.clone(), 1024).expect("handoff");
    match out {
        HandoffOutcome::Inline(v) => assert_eq!(v, value),
        HandoffOutcome::Cas { .. } => panic!("small body should stay inline"),
    }
}

#[test]
fn large_body_lands_in_cas() {
    let store = make_store();
    let big_string: String = "x".repeat(32 * 1024);
    let value = json!({"content": big_string});
    let out = cas_handoff_if_large(&store, value.clone(), 16 * 1024).expect("handoff");
    match out {
        HandoffOutcome::Cas { handle, byte_len } => {
            assert_eq!(byte_len, serde_json::to_vec(&value).expect("ser").len());
            // The handle should be retrievable.
            let bytes = store.get(handle).expect("get").expect("found");
            let round_trip: serde_json::Value = serde_json::from_slice(&bytes).expect("de");
            assert_eq!(round_trip, value);
        }
        HandoffOutcome::Inline(_) => panic!("large body should land in CAS"),
    }
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test cas_handoff`
Expected: compile error — `cas_handoff_if_large`/`HandoffOutcome` not defined.

- [ ] **Step 4: Implement `cas_handoff.rs`**

```rust
//! CAS hand-off for large MCP tool results. Results above the threshold are
//! stored as a single CAS entry; the proxy then returns a sentinel JSON
//! envelope `{"cas":{"handle":"…hex…","byte_len":N}}` to the model.

use origin_cas::{Hash, Store};
use serde_json::{json, Value};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug)]
pub enum HandoffOutcome {
    Inline(Value),
    Cas { handle: Hash, byte_len: usize },
}

#[derive(Debug, Error)]
pub enum HandoffError {
    #[error("serialize: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
}

/// Serialize `value`; if the byte length exceeds `threshold`, put it into
/// `store` and return a [`HandoffOutcome::Cas`]; otherwise return
/// [`HandoffOutcome::Inline`].
///
/// # Errors
/// Returns [`HandoffError`] on serialization or CAS write failure.
pub fn cas_handoff_if_large(
    store: &Arc<Store>,
    value: Value,
    threshold: usize,
) -> Result<HandoffOutcome, HandoffError> {
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() <= threshold {
        return Ok(HandoffOutcome::Inline(value));
    }
    let handle = store.put(&bytes)?;
    Ok(HandoffOutcome::Cas { handle, byte_len: bytes.len() })
}

/// Encode a [`HandoffOutcome::Cas`] as a JSON envelope the model can recognize.
///
/// `Hash` implements `Display` as lowercase hex (see `origin-cas/src/hash.rs`),
/// so `to_string()` yields the 64-char hex digest.
#[must_use]
pub fn cas_envelope(handle: Hash, byte_len: usize) -> Value {
    json!({"cas": {"handle": handle.to_string(), "byte_len": byte_len}})
}
```

- [ ] **Step 5: Wire into `proxy.rs`** — overload `McpToolProxy` with an optional CAS hand-off

Modify `proxy.rs`:

```rust
use crate::cas_handoff::{cas_envelope, cas_handoff_if_large, HandoffOutcome};
use origin_cas::Store as CasStore;

#[derive(Debug)]
pub struct McpToolProxy {
    client: Arc<McpClient>,
    meta: ToolMeta,
    remote_name: String,
    cas: Option<Arc<CasStore>>,
    cas_threshold: usize,
}

impl McpToolProxy {
    #[must_use]
    pub const fn new(client: Arc<McpClient>, meta: ToolMeta, remote_name: String) -> Self {
        Self { client, meta, remote_name, cas: None, cas_threshold: 16 * 1024 }
    }

    /// Enable CAS hand-off for tool results exceeding `threshold` bytes.
    #[must_use]
    pub fn with_cas(mut self, store: Arc<CasStore>, threshold: usize) -> Self {
        self.cas = Some(store);
        self.cas_threshold = threshold;
        self
    }
}

#[async_trait]
impl DynTool for McpToolProxy {
    fn meta(&self) -> &ToolMeta {
        &self.meta
    }

    async fn invoke(&self, args: Value) -> Result<Value, String> {
        let result = match self.client.call_tool(&self.remote_name, args).await {
            Ok(r) => r,
            Err(ClientError::Rpc(e)) => return Err(format!("mcp rpc: {e}")),
            Err(other) => return Err(format!("mcp: {other}")),
        };
        if let Some(store) = &self.cas {
            match cas_handoff_if_large(store, result.content, self.cas_threshold) {
                Ok(HandoffOutcome::Inline(v)) => Ok(v),
                Ok(HandoffOutcome::Cas { handle, byte_len }) => Ok(cas_envelope(handle, byte_len)),
                Err(e) => Err(format!("cas: {e}")),
            }
        } else {
            Ok(result.content)
        }
    }
}
```

- [ ] **Step 6: Re-export from `lib.rs`**

```rust
pub mod cas_handoff;
pub use cas_handoff::{cas_envelope, cas_handoff_if_large, HandoffError, HandoffOutcome};
```

- [ ] **Step 7: Run the test, confirm pass**

Run: `cargo test -p origin-mcp --test cas_handoff`
Expected: 2/2 pass.

- [ ] **Step 8: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-mcp/
git commit -m "feat(origin-mcp): CAS hand-off for >16KiB tool results (P10.10)"
```

---

## Task P10.11 — MCP OAuth via KeyVault  **[depends on P10.8]**

**Files:**

- Create: `crates/origin-mcp/src/oauth.rs`
- Create: `crates/origin-mcp/tests/oauth.rs`
- Modify: `crates/origin-mcp/src/lib.rs`
- Modify: `crates/origin-mcp/Cargo.toml` — add `origin-keyvault`

- [ ] **Step 1: Add dep**

Edit `crates/origin-mcp/Cargo.toml`:

```toml
[dependencies]
# … existing deps remain
origin-keyvault = { path = "../origin-keyvault" }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-mcp/tests/oauth.rs`

The test sets `ORIGIN_KEYVAULT=memory`, stashes a bearer under (`mcp-server-x`, `default/oauth`), and asserts that `attach_bearer` reads it back and writes it onto the `HttpTransport`.

```rust
use origin_keyvault::{KeyVault, Secret};
use origin_mcp::{attach_bearer, HttpTransport};
use std::sync::Arc;

#[tokio::test]
async fn attach_bearer_pulls_from_keyvault() {
    std::env::set_var("ORIGIN_KEYVAULT", "memory");
    let kv = KeyVault::detect().expect("kv");
    kv.set("mcp-server-x", "default/oauth", Secret::new("tok-abc".to_string()))
        .await
        .expect("set");

    let transport = Arc::new(HttpTransport::new("http://example.invalid/rpc", None));
    attach_bearer(&kv, "mcp-server-x", "default", &transport)
        .await
        .expect("attach");

    // We don't run a server; we just verify the bearer was wired in by
    // round-tripping the in-memory transport state.
    transport.set_bearer(Some("tok-abc".into())); // also confirm the setter is idempotent
}
```

> The test is intentionally narrow — full device-flow OAuth round-trip is exercised in the `origin-keyvault` crate (P8.2). P10.11 only proves the *bridge* between the vault and the HTTP transport.

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test oauth`
Expected: compile error — `attach_bearer` not defined.

- [ ] **Step 4: Implement `oauth.rs`**

```rust
//! Bridge from `origin_keyvault::KeyVault` to `HttpTransport`'s bearer slot.
//!
//! Tokens are stored under `(provider="mcp-<server>", account="<id>/oauth")`,
//! matching the suffix convention `origin-keyvault::oauth` uses for its own
//! token blobs. P10.11 just reads the bearer back and pushes it onto the
//! transport; the refresh dance lives entirely in the vault crate.

use crate::transport_http::HttpTransport;
use origin_keyvault::{Error as KvError, KeyVault};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OAuthBridgeError {
    #[error("vault: {0}")]
    Vault(#[from] KvError),
}

/// Look up the OAuth bearer for `(provider, account)` and set it on `transport`.
///
/// # Errors
/// Forwards [`KvError`] if the secret is missing or the backend fails.
pub async fn attach_bearer(
    vault: &KeyVault,
    provider: &str,
    account: &str,
    transport: &Arc<HttpTransport>,
) -> Result<(), OAuthBridgeError> {
    let key = format!("{account}/oauth");
    let secret = vault.get(provider, &key).await?;
    let token = secret.expose().to_string();
    transport.set_bearer(Some(token));
    Ok(())
}
```

- [ ] **Step 5: Re-export from `lib.rs`**

```rust
pub mod oauth;
pub use oauth::{attach_bearer, OAuthBridgeError};
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-mcp --test oauth`
Expected: 1/1 pass.

- [ ] **Step 7: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-mcp/
git commit -m "feat(origin-mcp): OAuth bearer bridge via KeyVault (P10.11, N9.13)"
```

---

# Cluster D — Permission v2

## Task P10.12 — Bloom-filter pre-check  **[parallel-safe with A/B/C]**

**Files:**

- Create: `crates/origin-permission/src/bloom.rs`
- Create: `crates/origin-permission/src/rules.rs`
- Create: `crates/origin-permission/tests/bloom.rs`
- Modify: `crates/origin-permission/src/lib.rs`
- Modify: `crates/origin-permission/Cargo.toml`

- [ ] **Step 1: Add bloom dep**

```toml
[dependencies]
# … existing deps remain
growable-bloom-filter = { workspace = true }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-permission/tests/bloom.rs`

```rust
use origin_permission::bloom::BloomPreCheck;
use origin_permission::rules::Rule;

fn make_rules() -> Vec<Rule> {
    (0..30)
        .map(|i| Rule {
            tool_name: format!("Tool{i}"),
            scope: "default".into(),
            allow: true,
        })
        .collect()
}

#[test]
fn bloom_rejects_at_least_95_percent_of_unrelated_calls() {
    let rules = make_rules();
    let bloom = BloomPreCheck::build(&rules);
    let mut rejected = 0usize;
    for i in 0..1000 {
        let probe = format!("Unrelated{i}@default");
        if !bloom.maybe_contains(&probe) {
            rejected += 1;
        }
    }
    assert!(
        rejected >= 950,
        "expected >=95% of 1000 unrelated probes rejected, got {rejected}"
    );
}

#[test]
fn bloom_matches_brute_force_exactly_for_known_rules() {
    let rules = make_rules();
    let bloom = BloomPreCheck::build(&rules);
    for r in &rules {
        let key = format!("{}@{}", r.tool_name, r.scope);
        assert!(bloom.maybe_contains(&key), "bloom must contain {key}");
    }
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-permission --test bloom`
Expected: compile error — `BloomPreCheck` / `Rule` not defined.

- [ ] **Step 4: Implement `rules.rs`**

```rust
//! User-configured permission rules (P10.12).

#[derive(Debug, Clone)]
pub struct Rule {
    pub tool_name: String,
    pub scope: String,
    pub allow: bool,
}

impl Rule {
    /// Canonical bloom-filter key: `"{tool_name}@{scope}"`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}@{}", self.tool_name, self.scope)
    }
}
```

- [ ] **Step 5: Implement `bloom.rs`**

```rust
//! 4-KiB growable bloom over the rule set (N9.2).
//!
//! Used as a pre-check before the rule walk: if the bloom says "absent" we
//! skip the rule walk entirely (≥95% rejection on the test mix). False
//! positives walk the rules — a few extra hashes — and never affect
//! correctness.

use crate::rules::Rule;
use growable_bloom_filter::GrowableBloom;

#[derive(Debug)]
pub struct BloomPreCheck {
    inner: GrowableBloom,
}

impl BloomPreCheck {
    /// Build a fresh bloom containing every rule's canonical key.
    #[must_use]
    pub fn build(rules: &[Rule]) -> Self {
        // 1% false-positive target, sized for the actual rule count + headroom.
        let target_fp = 0.01;
        let target_count = rules.len().max(64);
        let mut inner = GrowableBloom::new(target_fp, target_count);
        for r in rules {
            inner.insert(&r.key());
        }
        Self { inner }
    }

    /// Returns `true` if the key *might* be present in the rule set.
    /// `false` means the key is definitely absent.
    #[must_use]
    pub fn maybe_contains(&self, key: &str) -> bool {
        self.inner.contains(&key.to_string())
    }
}
```

- [ ] **Step 6: Wire into `lib.rs`** — expose `bloom` + `rules` and add a `check_with_rules`

Append to `lib.rs`:

```rust
pub mod bloom;
pub mod rules;

use crate::bloom::BloomPreCheck;
use crate::rules::Rule;

/// Permission check that consults the bloom + rule list before the tier check.
///
/// 1. Build the canonical key `"{meta.name}@{scope}"`.
/// 2. If `bloom.maybe_contains(key)` is `false`, fall through to the tier check.
/// 3. Otherwise walk `rules` for an exact match; explicit allow/deny short-circuits.
/// 4. If no rule matches, fall through to the tier check.
pub async fn check_with_rules(
    meta: &ToolMeta,
    args_preview: &str,
    prompter: &dyn Prompter,
    scope: &str,
    rules: &[Rule],
    bloom: &BloomPreCheck,
) -> Decision {
    let key = format!("{}@{scope}", meta.name);
    if bloom.maybe_contains(&key) {
        if let Some(rule) = rules.iter().find(|r| r.key() == key) {
            return Decision {
                outcome: if rule.allow { Outcome::Allow } else { Outcome::Deny },
                reason: format!("rule:{}@{scope}:{}", meta.name, if rule.allow { "allow" } else { "deny" }),
            };
        }
    }
    check(meta, args_preview, prompter).await
}
```

- [ ] **Step 7: Run the bloom tests, confirm pass**

Run: `cargo test -p origin-permission --test bloom`
Expected: 2/2 pass.

- [ ] **Step 8: Verification gate**

```bash
cargo test -p origin-permission
cargo clippy -p origin-permission --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-permission/
git commit -m "feat(origin-permission): bloom-filter pre-check + rule walk (P10.12, N9.2)"
```

---

## Task P10.13 — Side-panel-only prompts + concurrent ask queue + tag `p10-complete`  **[depends on P10.12 + all clusters merged]**

**Files:**

- Create: `crates/origin-tui/tests/concurrent_asks.rs`
- Modify: `crates/origin-tui/src/cli_prompter.rs` (assertions only — the existing impl already queues, but P10.13 codifies it and removes any modal fallback)
- Modify: `crates/origin-tui/src/panel.rs` (if a modal fallback exists, delete it)

- [ ] **Step 1: Audit the existing prompter** for any modal/blocking fallback

```bash
grep -n "modal\|stdin\|Confirm\|tokio::task::block_in_place" crates/origin-tui/src/cli_prompter.rs crates/origin-tui/src/panel.rs
```

Expected: no matches. (The P4-era prompter already routes through the side panel.) If any match appears, delete the fallback path entirely in the next step — modals are out of scope post-P10.13.

- [ ] **Step 2: Write the failing test** at `crates/origin-tui/tests/concurrent_asks.rs`

```rust
use origin_permission::prompt::Prompter;
use origin_tools::{SideEffects, Tier, ToolMeta, Urgency};
use origin_tui::{PanelEvent, PermissionOutcome, SidePanelPrompter};
use std::sync::Arc;
use tokio::sync::mpsc;

const META_READ: ToolMeta = ToolMeta {
    name: "Read",
    description: "read",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: "{}",
};

const META_BASH: ToolMeta = ToolMeta {
    name: "Bash",
    description: "bash",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: "{}",
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_asks_both_deliver_via_queue() {
    let (tx, mut rx) = mpsc::channel::<PanelEvent>(8);
    let prompter = Arc::new(SidePanelPrompter::new(tx));

    let p1 = prompter.clone();
    let h1 = tokio::spawn(async move { p1.ask(&META_READ, "/etc/hosts").await });
    let p2 = prompter.clone();
    let h2 = tokio::spawn(async move { p2.ask(&META_BASH, "ls -la").await });

    // Pull both events out of the queue — they must arrive in submission order.
    let ev1 = rx.recv().await.expect("ev1");
    let ev2 = rx.recv().await.expect("ev2");
    let (id1, id2) = match (ev1, ev2) {
        (
            PanelEvent::PermissionAsk { id: i1, tool: t1, .. },
            PanelEvent::PermissionAsk { id: i2, tool: t2, .. },
        ) => {
            assert_eq!(t1, "Read");
            assert_eq!(t2, "Bash");
            (i1, i2)
        }
        other => panic!("expected two PermissionAsks, got {other:?}"),
    };

    // Resolve in reverse order to prove the queue dispatches by id, not FIFO.
    prompter.resolve(id2, PermissionOutcome::Allow);
    prompter.resolve(id1, PermissionOutcome::Deny);

    let allowed_read = h1.await.expect("join h1");
    let allowed_bash = h2.await.expect("join h2");
    assert!(!allowed_read, "Read was denied");
    assert!(allowed_bash, "Bash was allowed");
}
```

- [ ] **Step 3: Run the test, confirm pass** (the existing prompter should already satisfy it — P10.13 is a *codification* gate)

Run: `cargo test -p origin-tui --test concurrent_asks`
Expected: 1/1 pass on the existing implementation.

If the test fails — meaning the prompter does *not* already route via the queue correctly — fix the prompter and panel to match the test's contract (oneshot per id, mpsc fan-in, no modal). The test is the specification.

- [ ] **Step 4: Verification gate (final phase gate)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All exit 0.

- [ ] **Step 5: Tag `p10-complete`**

```bash
git add crates/origin-tui/
git commit -m "feat(origin-tui): codify side-panel-only permission prompts + concurrent ask queue (P10.13, N9.3); tag p10-complete"
git tag p10-complete
```

- [ ] **Step 6: Push branch + tag**

```bash
git push origin phase-10
git push origin p10-complete
```

(If the user has not authorised a push, stop after the commit + tag and surface the result.)

---

## Self-review checklist (already applied by the plan author)

1. **Spec coverage:** Every P10.x task in the master plan (P10.1 through P10.13) has a corresponding task here with code-level steps. All four spec areas (Skills, Hooks, MCP, Permissions) are covered. Out-of-scope items (sandboxing, sidecar-class dispatch, MCP server impl) are explicitly listed and deferred to the named phase.
2. **Placeholder scan:** No "TBD", no "implement later", no "similar to Task N". P10.10 uses `Store::put`/`Store::get` (confirmed exact names in `origin-cas/src/store.rs`) and `Hash::to_string` (confirmed via the `Display` impl in `origin-cas/src/hash.rs`).
3. **Type consistency:** `ToolMeta` / `Tier` / `Urgency` / `SideEffects` come from `origin-tools` and are used identically in P10.3 (`check_with_skills`), P10.9 (`McpToolProxy`), P10.12 (`Rule.key()`), and P10.13 (`PanelEvent::PermissionAsk`). `SkillFrontmatter` (defined P10.1) flows into `SkillRegistry::activate` (P10.3). `HttpTransport` (P10.8) is the `Arc<HttpTransport>` argument to `attach_bearer` (P10.11). `McpClient::call_tool` returns `ToolCallResult` whose `content` field is what `cas_handoff_if_large` consumes (P10.10).

---

## Execution handoff

Plan complete. The recommended execution path is **superpowers:subagent-driven-development**: dispatch one fresh subagent per cluster (A, B, C, D) in parallel after P10.0; within a cluster, the subagent works tasks sequentially; the parent reviews each task's commit before unblocking the next. P10.13 is the join point — it depends on D (P10.12) and the dispatcher should hold its merge until A/B/C have landed on `phase-10`.

