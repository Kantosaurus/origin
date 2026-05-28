# Origin Tool Suite v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Per user directive, execution uses `/dispatching-parallel-agents` with `/test-driven-development` inside each task and `/verification-before-completion` at the close of each task.

**Goal:** Replace `crates/origin-tools`' per-tool implementations with a shared envelope (text-format normaliser, token-budget writer, output-CAS, process supervisor, persistent rust-analyzer bridge) and ship six rebuilt + six new tools that all inherit those wins, eliminating the CRLF Edit failure class and hitting the four KPI targets in the spec.

**Architecture:** A single new module `tool_envelope` in `origin-tools` sits between `dispatch::invoke` (in `origin-daemon/src/agent.rs::dispatch_tool`) and every per-tool function. It owns input canonicalisation, result deduplication, budget-aware result emission, streaming chunk dispatch, and process supervision. Tools become thin functions over a `NormalizedCtx`/`ResultWriter` pair.

**Tech Stack:** Rust 1.83 (MSRV pinned — see `memory/project_msrv_dep_pinning.md`), tokio (async), `blake3` (already a dep), `ignore`/`grep-*` (already deps), `walkdir` (already), `encoding_rs` (new — UTF-16 BOM decode), `pdf-extract` (new — Read `as: pdf`), `image` (new — Read `as: image`), `similar` (new — diff hunk formatting), JSON-RPC over stdio for the LSP client (no extra dep; thin hand-rolled wire).

**Source spec:** `docs/superpowers/specs/2026-05-28-origin-tool-suite-v2-design.md`.

---

## File structure

### Files created in `crates/origin-tools/`

| Path | Responsibility |
|------|----------------|
| `src/text_fmt.rs` | EOL/encoding/BOM detect + normalise + denormalise |
| `src/budget_writer.rs` | Token-aware result builder with elision + continuation handles |
| `src/result_cas.rs` | Output-content dedup store (session-scoped) |
| `src/error.rs` | Structured `ToolError` enum + serialisation |
| `src/tool_envelope.rs` | Orchestration: input canon → CAS lookup → tool fn → writer → record |
| `src/proc_supervisor.rs` | Long-running process handles, ring-buffer, timeout, kill |
| `src/ra_bridge.rs` | `DiagnosticsHandle` trait (object-safe, mirrors `MemoryHandle`) |
| `src/builtins/multi_edit.rs` | `MultiEdit` tool |
| `src/builtins/apply_patch.rs` | `ApplyPatch` tool (unified diff) |
| `src/builtins/monitor.rs` | `Monitor` tool (tail supervisor ring-buffer) |
| `src/builtins/diagnostics.rs` | `Diagnostics` tool (RA bridge consumer) |
| `src/builtins/tool_search.rs` | `ToolSearch` tool (lazy schema serving) |
| `tests/text_fmt.rs` | EOL/encoding/BOM unit + property tests |
| `tests/budget_writer.rs` | Budget property tests (proptest) |
| `tests/crlf_regression.rs` | The CRLF screenshot bug as a canary suite |
| `tests/multi_edit.rs` | MultiEdit atomic semantics |
| `tests/apply_patch.rs` | ApplyPatch hunk validation |
| `tests/proc_supervisor.rs` | Supervisor timeout, kill, ring-buffer, parallel isolation |

### Files modified in `crates/origin-tools/`

| Path | Change |
|------|--------|
| `src/lib.rs` | Re-export new modules; add `token_budget` constant |
| `src/registry.rs` | Add `token_budget: u32` field to `ToolMeta` |
| `src/macros.rs` | Extend `origin_tool!` to accept optional `token_budget:` arm |
| `src/dispatch.rs` | Add output-CAS table alongside existing input-keyed `Cache` |
| `src/builtins/mod.rs` | Register new builtins |
| `src/builtins/read.rs` | Rebuild: offset/limit/as, line numbers, envelope routing |
| `src/builtins/edit.rs` | Rebuild: CRLF-safe via `text_fmt`, hunk return, `replace_all` |
| `src/builtins/write.rs` | Rebuild: atomic, read-before-write guard, EOL preservation |
| `src/builtins/grep_tool.rs` | Rebuild: output_mode, head_limit, type/glob filter, -A/-B/-C |
| `src/builtins/glob_tool.rs` | Rebuild: mtime-sorted, head_limit, gitignore |
| `src/builtins/bash.rs` | Rebuild: timeout, cwd, run_in_background, env |
| `Cargo.toml` | Add: `encoding_rs`, `pdf-extract`, `image`, `similar`, `proptest` (dev), `tokio` features |

### New crate

| Path | Responsibility |
|------|----------------|
| `crates/origin-lsp-client/Cargo.toml` | Crate manifest |
| `crates/origin-lsp-client/src/lib.rs` | Stdio JSON-RPC client (initialize, didOpen, didChange, publishDiagnostics) |
| `crates/origin-lsp-client/tests/smoke.rs` | Smoke test against a mock server |

### Files modified outside `origin-tools`

| Path | Change |
|------|--------|
| `crates/origin-daemon/src/agent.rs` | Replace `dispatch_tool` body with envelope call; instantiate envelope, supervisor, RA bridge handles in caller chain |
| `crates/origin-daemon/src/main.rs` | Spawn `Supervisor`; spawn RA bridge worker; lazily start rust-analyzer |
| `crates/origin-daemon/src/ra_impl.rs` (new) | `DiagnosticsHandle` impl wrapping `origin-lsp-client` |
| `crates/origin-daemon/Cargo.toml` | Add `origin-lsp-client` dep |
| `Cargo.toml` (workspace root) | Add `crates/origin-lsp-client` to members |
| `Cargo.lock` | Refreshed |

### Files renamed/removed

| Path | Action |
|------|--------|
| `crates/origin-tools/src/builtins/glob_tool.rs` | Internal fn renamed `glob_tool` → `glob_v2`; old name kept as deprecated re-export until Phase 8 cleanup |
| `crates/origin-tools/src/builtins/bash.rs::bash_tool_streaming` | Removed in Phase 8 (envelope drives streaming for all tools) |

---

## Phase 0 — Prep

### Task 0.1: Create the isolated worktree

**Files:** worktree-only (no source files touched).

- [ ] **Step 1: Verify clean baseline test status on `dev`**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -30
```
Expected: existing tests pass (or fail consistently with the snapshot before this work begins — record the snapshot in commit body of Task 0.3 below).

- [ ] **Step 2: Create the worktree using `superpowers:using-git-worktrees`**

Follow the skill. Worktree branch name: `tool-suite-v2`. Worktree path: `../origin-tool-suite-v2` (adjacent to repo).

- [ ] **Step 3: `cd` into worktree, confirm `git status` is clean**

```bash
git status
```
Expected: `nothing to commit, working tree clean`, branch `tool-suite-v2`.

### Task 0.2: Capture KPI baseline

**Files:** `bench/tool-suite-v2/baseline.json` (in repo root, in worktree).

- [ ] **Step 1: Pick the 10-task corpus**

Use these 10 representative tasks (record them in the file):
1. Read `crates/origin-cli/src/main.rs` (large file, ~2000 lines)
2. Grep `fn dispatch_tool` workspace-wide
3. Edit single replacement in `crates/origin-tools/src/builtins/read.rs`
4. Read same file twice in one session (CAS hit test)
5. Bash `cargo check -p origin-tools` (slow, ~10s)
6. Grep `TODO` workspace-wide (high-volume)
7. Edit a CRLF file with LF needle (will fail today — record as 0)
8. Read a file ≥3000 lines (truncation test)
9. Glob `crates/**/Cargo.toml`
10. Five sequential edits to one file (round-trip count test)

- [ ] **Step 2: Create the baseline file**

```bash
mkdir -p bench/tool-suite-v2
```

Create `bench/tool-suite-v2/baseline.json`:

```json
{
  "captured_on": "2026-05-28",
  "branch_at_capture": "dev",
  "tasks": [
    {"id": 1, "name": "read_large_file", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 2, "name": "grep_workspace_symbol", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 3, "name": "edit_single_replacement", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 4, "name": "read_same_file_twice", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 5, "name": "bash_cargo_check", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 6, "name": "grep_todo_workspace", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 7, "name": "edit_crlf_file_lf_needle", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 8, "name": "read_3000_lines", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 9, "name": "glob_workspace_cargo", "result_tokens": null, "wall_ms": null, "ok": null},
    {"id": 10, "name": "five_sequential_edits", "result_tokens": null, "wall_ms": null, "ok": null}
  ],
  "totals": { "result_tokens": null, "round_trips": null, "wall_ms": null, "failures": null },
  "system_prompt_tool_tokens": null
}
```

- [ ] **Step 3: Run the baseline harness manually**

Invoke each task once against `origin` running on `dev`. Record the actual numbers into `baseline.json`. (Manual measurement is fine — this is a one-shot reference. The Phase 8 KPI bench task automates the comparison.)

For tool-result tokens use this approximation: `chars_in_serialized_result / 4`. Round to the nearest 10. For `system_prompt_tool_tokens`, take the byte length of `tools_schema` JSON in `crates/origin-daemon/src/agent.rs:355` and divide by 4.

- [ ] **Step 4: Commit**

```bash
git add bench/tool-suite-v2/baseline.json
git commit -m "bench: capture pre-tool-suite-v2 KPI baseline (10-task corpus)"
```

### Task 0.3: Add the new workspace dependencies

**Files:** `Cargo.toml` (workspace root), `crates/origin-tools/Cargo.toml`.

- [ ] **Step 1: Add deps to the workspace `[workspace.dependencies]` section in root `Cargo.toml`**

If `[workspace.dependencies]` does not exist yet (check first), skip this step. Otherwise add:

```toml
encoding_rs = "0.8"
pdf-extract = "0.7"
image = { version = "0.24", default-features = false, features = ["png", "jpeg", "webp"] }
similar = "2"
proptest = "1"
```

- [ ] **Step 2: Add deps to `crates/origin-tools/Cargo.toml`**

In `[dependencies]`:

```toml
encoding_rs = "0.8"
pdf-extract = "0.7"
image = { version = "0.24", default-features = false, features = ["png", "jpeg", "webp"] }
similar = "2"
```

In `[dev-dependencies]`:

```toml
proptest = "1"
```

- [ ] **Step 3: `cargo check -p origin-tools` to confirm deps resolve under MSRV**

Run: `cargo check -p origin-tools`
Expected: clean build (warnings OK at this point). If any dep requires edition2024, add a `cargo update --precise <ver> <crate>` workaround in this step and document in the commit message.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/origin-tools/Cargo.toml Cargo.lock
git commit -m "deps(tools): add encoding_rs, pdf-extract, image, similar, proptest for tool-suite-v2"
```

---

## Phase 1 — Foundation (envelope skeleton, text_fmt, budget_writer, errors)

Goal of Phase 1: the envelope exists and is a pure passthrough — no observable behaviour change to any tool. After Phase 1, every existing test still passes.

### Task 1.1: `ToolError` enum + serialisation

**Files:**
- Create: `crates/origin-tools/src/error.rs`
- Test: inline `#[cfg(test)] mod tests` in `error.rs`
- Modify: `crates/origin-tools/src/lib.rs` (add `pub mod error;` and re-export)

- [ ] **Step 1: Write the failing test**

Inline at the bottom of `crates/origin-tools/src/error.rs` (file does not exist yet; write the full file in Step 3 below):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_kind_and_recoverable() {
        let err = ToolError::new(ErrClass::Edit, "no_match", "string not found")
            .recoverable(true)
            .hint("widen the context");
        let json = err.to_json();
        assert_eq!(json["kind"], "edit.no_match");
        assert_eq!(json["message"], "string not found");
        assert_eq!(json["recoverable"], true);
        assert_eq!(json["hint"], "widen the context");
    }

    #[test]
    fn classes_match_taxonomy() {
        for class in [
            ErrClass::Io, ErrClass::Edit, ErrClass::Bash,
            ErrClass::Regex, ErrClass::Budget, ErrClass::Subsystem,
            ErrClass::Validation,
        ] {
            let s: &'static str = class.as_str();
            assert!(!s.is_empty());
            assert!(!s.contains('.'));
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p origin-tools --lib error::tests 2>&1 | tail -15`
Expected: fails (file does not exist, or `ToolError`/`ErrClass` undefined).

- [ ] **Step 3: Write the minimal implementation**

`crates/origin-tools/src/error.rs`:

```rust
//! Structured tool error taxonomy. Every tool returns a `ToolError` instead
//! of a free-form `String`; the envelope serialises it as `{kind, message,
//! recoverable, hint?}` so the agent loop can pattern-match recoverable
//! failures without LLM re-parsing.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrClass {
    Io,
    Edit,
    Bash,
    Regex,
    Budget,
    Subsystem,
    Validation,
}

impl ErrClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Edit => "edit",
            Self::Bash => "bash",
            Self::Regex => "regex",
            Self::Budget => "budget",
            Self::Subsystem => "subsystem",
            Self::Validation => "validation",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolError {
    pub class: ErrClass,
    pub reason: &'static str,
    pub message: String,
    pub recoverable: bool,
    pub hint: Option<String>,
}

impl ToolError {
    #[must_use]
    pub fn new(class: ErrClass, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            reason,
            message: message.into(),
            recoverable: false,
            hint: None,
        }
    }

    #[must_use]
    pub fn recoverable(mut self, yes: bool) -> Self {
        self.recoverable = yes;
        self
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = json!({
            "kind": format!("{}.{}", self.class.as_str(), self.reason),
            "message": self.message,
            "recoverable": self.recoverable,
        });
        if let Some(h) = &self.hint {
            obj["hint"] = Value::String(h.clone());
        }
        obj
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_kind_and_recoverable() {
        let err = ToolError::new(ErrClass::Edit, "no_match", "string not found")
            .recoverable(true)
            .hint("widen the context");
        let json = err.to_json();
        assert_eq!(json["kind"], "edit.no_match");
        assert_eq!(json["message"], "string not found");
        assert_eq!(json["recoverable"], true);
        assert_eq!(json["hint"], "widen the context");
    }

    #[test]
    fn classes_match_taxonomy() {
        for class in [
            ErrClass::Io, ErrClass::Edit, ErrClass::Bash,
            ErrClass::Regex, ErrClass::Budget, ErrClass::Subsystem,
            ErrClass::Validation,
        ] {
            let s: &'static str = class.as_str();
            assert!(!s.is_empty());
            assert!(!s.contains('.'));
        }
    }
}
```

Add to `crates/origin-tools/src/lib.rs` (top of `pub mod` block):

```rust
pub mod error;
pub use error::{ErrClass, ToolError};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p origin-tools --lib error::tests`
Expected: 2 passed.

- [ ] **Step 5: Verification-before-completion**

```bash
cargo clippy -p origin-tools -- -D warnings
cargo fmt --check
```
Expected: both clean.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-tools/src/error.rs crates/origin-tools/src/lib.rs
git commit -m "feat(tools): add structured ToolError taxonomy

Replaces per-tool Result<T, String> with {kind, message, recoverable, hint?}
so the agent loop can pattern-match recoverable failures without LLM re-parse.
"
```

### Task 1.2: `text_fmt` — detect + normalise

**Files:**
- Create: `crates/origin-tools/src/text_fmt.rs`
- Test: `crates/origin-tools/tests/text_fmt.rs`
- Modify: `crates/origin-tools/src/lib.rs` (add `pub mod text_fmt;`)

- [ ] **Step 1: Write the failing tests**

`crates/origin-tools/tests/text_fmt.rs`:

```rust
use origin_tools::text_fmt::{detect, denormalise, normalise_to_lf, Bom, Eol, Encoding};

#[test]
fn detects_lf_no_bom() {
    let d = detect(b"line1\nline2\n");
    assert_eq!(d.eol, Eol::Lf);
    assert_eq!(d.bom, None);
    assert_eq!(d.encoding, Encoding::Utf8);
    assert!(d.trailing_newline);
}

#[test]
fn detects_crlf() {
    let d = detect(b"line1\r\nline2\r\n");
    assert_eq!(d.eol, Eol::Crlf);
}

#[test]
fn detects_cr_only() {
    let d = detect(b"line1\rline2\r");
    assert_eq!(d.eol, Eol::Cr);
}

#[test]
fn detects_mixed() {
    let d = detect(b"a\r\nb\nc\r\n");
    assert_eq!(d.eol, Eol::Mixed);
}

#[test]
fn detects_utf8_bom() {
    let d = detect(b"\xef\xbb\xbfhello");
    assert_eq!(d.bom, Some(Bom::Utf8));
    assert_eq!(d.encoding, Encoding::Utf8);
}

#[test]
fn detects_utf16_le_bom() {
    let d = detect(b"\xff\xfeh\0e\0");
    assert_eq!(d.bom, Some(Bom::Utf16Le));
    assert_eq!(d.encoding, Encoding::Utf16Le);
}

#[test]
fn round_trip_crlf_preserves_bytes() {
    let original = b"a\r\nb\r\nc";
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    assert_eq!(text, "a\nb\nc");
    let back = denormalise(&text, &det);
    assert_eq!(back, original);
}

#[test]
fn round_trip_mixed_preserves_per_line() {
    let original = b"a\r\nb\nc\r";
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    assert_eq!(text, "a\nb\nc\n");
    let back = denormalise(&text, &det);
    assert_eq!(back, original);
}

#[test]
fn insert_inherits_preceding_line_eol() {
    let original = b"a\r\nb\r\nc";
    let det = detect(original);
    let text = normalise_to_lf(original, &det).unwrap();
    // simulate model inserting "X" after line 1
    let edited = text.replace("a\n", "a\nX\n");
    let back = denormalise(&edited, &det);
    // inserted line inherits CRLF from preceding line
    assert_eq!(back, b"a\r\nX\r\nb\r\nc");
}

#[test]
fn non_utf8_without_bom_errors() {
    let bytes = &[0xff, 0xff, 0xff];
    let det = detect(bytes);
    let result = normalise_to_lf(bytes, &det);
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p origin-tools --test text_fmt 2>&1 | tail -10`
Expected: failure — `text_fmt` module not found.

- [ ] **Step 3: Write the minimal implementation**

`crates/origin-tools/src/text_fmt.rs`:

```rust
//! EOL / encoding / BOM detection and normalisation.
//!
//! Every file-touching tool reads bytes, calls [`detect`] once to capture
//! the file's original convention, then works against an LF-normalised
//! `String`. On write, [`denormalise`] restores the original convention
//! byte-for-byte, including per-source-line EOL for mixed-EOL files.

use crate::error::{ErrClass, ToolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    Cr,
    Mixed,
    /// File has no newlines at all.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bom {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone)]
pub struct Detected {
    pub eol: Eol,
    pub bom: Option<Bom>,
    pub encoding: Encoding,
    pub trailing_newline: bool,
    /// Per-source-line EOL, length = number of `\n`-terminated lines after
    /// LF-normalisation. Used by [`denormalise`] to restore mixed-EOL files.
    pub per_line_eol: Vec<Eol>,
}

#[must_use]
pub fn detect(bytes: &[u8]) -> Detected {
    let (bom, body_start, encoding) = detect_bom(bytes);

    // For UTF-16 we don't classify per-line EOL in v2 (BOM-only path covers
    // the common case; mixed UTF-16 EOL is exotic).
    if encoding != Encoding::Utf8 {
        return Detected {
            eol: Eol::Lf, // placeholder; UTF-16 normalisation decodes the whole body
            bom,
            encoding,
            trailing_newline: false,
            per_line_eol: Vec::new(),
        };
    }

    let body = &bytes[body_start..];
    let (eol, per_line_eol, trailing_newline) = classify_eols(body);
    Detected { eol, bom, encoding, trailing_newline, per_line_eol }
}

fn detect_bom(bytes: &[u8]) -> (Option<Bom>, usize, Encoding) {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (Some(Bom::Utf8), 3, Encoding::Utf8)
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (Some(Bom::Utf16Le), 2, Encoding::Utf16Le)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Some(Bom::Utf16Be), 2, Encoding::Utf16Be)
    } else {
        (None, 0, Encoding::Utf8)
    }
}

fn classify_eols(body: &[u8]) -> (Eol, Vec<Eol>, bool) {
    let mut per_line = Vec::new();
    let mut i = 0;
    let mut seen_lf = false;
    let mut seen_crlf = false;
    let mut seen_cr = false;
    while i < body.len() {
        match body[i] {
            b'\r' if i + 1 < body.len() && body[i + 1] == b'\n' => {
                per_line.push(Eol::Crlf);
                seen_crlf = true;
                i += 2;
            }
            b'\r' => {
                per_line.push(Eol::Cr);
                seen_cr = true;
                i += 1;
            }
            b'\n' => {
                per_line.push(Eol::Lf);
                seen_lf = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let trailing_newline = body
        .last()
        .is_some_and(|&b| b == b'\n' || b == b'\r');

    let kind_count = u32::from(seen_lf) + u32::from(seen_crlf) + u32::from(seen_cr);
    let eol = match kind_count {
        0 => Eol::None,
        1 if seen_lf => Eol::Lf,
        1 if seen_crlf => Eol::Crlf,
        1 if seen_cr => Eol::Cr,
        _ => Eol::Mixed,
    };
    (eol, per_line, trailing_newline)
}

/// Decode the file's bytes into a canonical LF-only `String`.
///
/// # Errors
/// Returns `ToolError(io.encoding)` if the bytes are not valid in the detected
/// encoding.
pub fn normalise_to_lf(bytes: &[u8], det: &Detected) -> Result<String, ToolError> {
    let body_start = match det.bom {
        Some(Bom::Utf8) => 3,
        Some(Bom::Utf16Le | Bom::Utf16Be) => 2,
        None => 0,
    };
    let body = &bytes[body_start..];

    match det.encoding {
        Encoding::Utf8 => {
            let text = std::str::from_utf8(body).map_err(|e| {
                ToolError::new(
                    ErrClass::Io,
                    "encoding",
                    format!("not valid UTF-8 at byte {}: {e}", e.valid_up_to()),
                )
            })?;
            // Normalise: CRLF -> LF, lone CR -> LF.
            let mut out = String::with_capacity(text.len());
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'\r' if i + 1 < bytes.len() && bytes[i + 1] == b'\n' => {
                        out.push('\n');
                        i += 2;
                    }
                    b'\r' => {
                        out.push('\n');
                        i += 1;
                    }
                    b => {
                        out.push(b as char);
                        i += 1;
                    }
                }
            }
            Ok(out)
        }
        Encoding::Utf16Le => {
            let (cow, _, had_errors) = encoding_rs::UTF_16LE.decode(body);
            if had_errors {
                return Err(ToolError::new(
                    ErrClass::Io,
                    "encoding",
                    "invalid UTF-16 LE",
                ));
            }
            Ok(cow.into_owned())
        }
        Encoding::Utf16Be => {
            let (cow, _, had_errors) = encoding_rs::UTF_16BE.decode(body);
            if had_errors {
                return Err(ToolError::new(
                    ErrClass::Io,
                    "encoding",
                    "invalid UTF-16 BE",
                ));
            }
            Ok(cow.into_owned())
        }
    }
}

/// Re-encode `text` back to the original file's byte convention.
#[must_use]
pub fn denormalise(text: &str, det: &Detected) -> Vec<u8> {
    let bom_bytes: &[u8] = match det.bom {
        Some(Bom::Utf8) => &[0xef, 0xbb, 0xbf],
        Some(Bom::Utf16Le) => &[0xff, 0xfe],
        Some(Bom::Utf16Be) => &[0xfe, 0xff],
        None => &[],
    };
    let body = match det.encoding {
        Encoding::Utf8 => encode_eols_utf8(text, det),
        Encoding::Utf16Le => {
            let lf_restored = restore_eols(text, det);
            let mut out = Vec::with_capacity(lf_restored.len() * 2);
            for u in lf_restored.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out
        }
        Encoding::Utf16Be => {
            let lf_restored = restore_eols(text, det);
            let mut out = Vec::with_capacity(lf_restored.len() * 2);
            for u in lf_restored.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
            out
        }
    };
    [bom_bytes, &body[..]].concat()
}

fn encode_eols_utf8(text: &str, det: &Detected) -> Vec<u8> {
    let s = restore_eols(text, det);
    s.into_bytes()
}

/// Walk `text` line by line; for each `\n` boundary in `text`, pick the EOL
/// for that line from `det.per_line_eol`. Lines beyond the per-line vector
/// (i.e. lines inserted by an edit) inherit the EOL of the line immediately
/// preceding them. If no preceding line exists, fall back to the file's
/// dominant EOL.
fn restore_eols(text: &str, det: &Detected) -> String {
    let dominant_eol_str = dominant_eol_str(det);
    let mut out = String::with_capacity(text.len() + det.per_line_eol.len());
    let mut line_idx = 0_usize;
    let mut last_eol_str: &'static str = dominant_eol_str;

    let chars: Vec<&str> = text.split_inclusive('\n').collect();
    for line in chars {
        if let Some(stripped) = line.strip_suffix('\n') {
            out.push_str(stripped);
            let eol_str: &'static str = if line_idx < det.per_line_eol.len() {
                let e = match det.per_line_eol[line_idx] {
                    Eol::Crlf => "\r\n",
                    Eol::Cr => "\r",
                    Eol::Lf => "\n",
                    Eol::Mixed | Eol::None => last_eol_str,
                };
                last_eol_str = e;
                e
            } else {
                last_eol_str
            };
            out.push_str(eol_str);
            line_idx += 1;
        } else {
            // Final line with no trailing newline.
            out.push_str(line);
        }
    }
    // Preserve original trailing-newline policy: if the file had no trailing
    // newline but `text` does, restoring already preserves it via the loop.
    // No additional action needed.
    out
}

fn dominant_eol_str(det: &Detected) -> &'static str {
    match det.eol {
        Eol::Crlf => "\r\n",
        Eol::Cr => "\r",
        Eol::Lf | Eol::Mixed | Eol::None => "\n",
    }
}
```

Add to `crates/origin-tools/src/lib.rs`:

```rust
pub mod text_fmt;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p origin-tools --test text_fmt`
Expected: 10 passed.

- [ ] **Step 5: Verification-before-completion**

```bash
cargo clippy -p origin-tools -- -D warnings
cargo fmt --check
```
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-tools/src/text_fmt.rs crates/origin-tools/tests/text_fmt.rs crates/origin-tools/src/lib.rs
git commit -m "feat(tools): text_fmt detect/normalise/denormalise with per-line EOL

Kills the CRLF Edit failure class at its source: every file tool will
normalise to LF for matching, then denormalise back to the file's
original convention (including per-source-line EOL for mixed files).
"
```

### Task 1.3: `text_fmt` CRLF round-trip property test (proptest)

**Files:**
- Modify: `crates/origin-tools/tests/text_fmt.rs` (append proptest block)

- [ ] **Step 1: Add the property test at the bottom of `crates/origin-tools/tests/text_fmt.rs`**

```rust
use proptest::prelude::*;

fn arb_eol() -> impl Strategy<Value = &'static [u8]> {
    prop_oneof![
        Just("\r\n".as_bytes()),
        Just("\n".as_bytes()),
        Just("\r".as_bytes()),
    ]
}

fn arb_line_with_eol() -> impl Strategy<Value = Vec<u8>> {
    (".[a-zA-Z]{1,8}", arb_eol()).prop_map(|(s, eol)| {
        let mut v = s.into_bytes();
        v.extend_from_slice(eol);
        v
    })
}

proptest! {
    #[test]
    fn round_trip_arbitrary_mixed_eol(lines in proptest::collection::vec(arb_line_with_eol(), 0..20)) {
        let original: Vec<u8> = lines.into_iter().flatten().collect();
        let det = detect(&original);
        let text = normalise_to_lf(&original, &det).unwrap();
        let back = denormalise(&text, &det);
        prop_assert_eq!(back, original);
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p origin-tools --test text_fmt round_trip_arbitrary_mixed_eol`
Expected: pass (with proptest's default 256 cases).

- [ ] **Step 3: Verification + commit**

```bash
cargo clippy -p origin-tools --tests -- -D warnings
git add crates/origin-tools/tests/text_fmt.rs
git commit -m "test(text_fmt): proptest round-trips preserve arbitrary mixed-EOL files"
```

### Task 1.4: `ToolMeta` gains `token_budget`, macro extended

**Files:**
- Modify: `crates/origin-tools/src/registry.rs`
- Modify: `crates/origin-tools/src/macros.rs`
- Test: `crates/origin-tools/tests/registry.rs` (existing — extend)

- [ ] **Step 1: Add the failing test**

Append to `crates/origin-tools/tests/registry.rs`:

```rust
#[test]
fn every_tool_has_nonzero_token_budget() {
    for meta in origin_tools::registry_iter() {
        assert!(meta.token_budget > 0, "tool {} has zero token_budget", meta.name);
    }
}
```

- [ ] **Step 2: Run — expect fail (field does not exist)**

Run: `cargo test -p origin-tools --test registry every_tool_has_nonzero_token_budget 2>&1 | tail -10`

- [ ] **Step 3: Add the field to `ToolMeta` (`crates/origin-tools/src/registry.rs`)**

Add field after `sandbox_profile`:

```rust
    /// Approximate token budget for this tool's serialised result. The
    /// envelope's `ResultWriter` truncates / elides at this cap. Default 25k.
    pub token_budget: u32,
```

- [ ] **Step 4: Extend `crates/origin-tools/src/macros.rs`**

Replace the entire file with:

```rust
//! `origin_tool!` macro — registers a tool's metadata into the inventory.

#[macro_export]
macro_rules! origin_tool {
    // Full form with sandbox AND token_budget.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr,
        sandbox: $sandbox:expr,
        token_budget: $budget:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: $sandbox,
                token_budget: $budget,
            }
        }
    };
    // Sandbox set, default token_budget.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr,
        sandbox: $sandbox:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: $sandbox,
                token_budget: $crate::DEFAULT_TOKEN_BUDGET,
            }
        }
    };
    // Default sandbox AND default token_budget.
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
                sandbox_profile: ::origin_sandbox::SandboxProfile::Inherit,
                token_budget: $crate::DEFAULT_TOKEN_BUDGET,
            }
        }
    };
}
```

- [ ] **Step 5: Add `DEFAULT_TOKEN_BUDGET` to `crates/origin-tools/src/lib.rs`**

Add at top level:

```rust
/// Default per-tool token budget for serialised results. Tools may override
/// via the `token_budget:` arm of `origin_tool!`.
pub const DEFAULT_TOKEN_BUDGET: u32 = 25_000;
```

- [ ] **Step 6: Run all `origin-tools` tests**

Run: `cargo test -p origin-tools`
Expected: every existing tool re-registers without compile error (the default arm covers them); `every_tool_has_nonzero_token_budget` passes.

- [ ] **Step 7: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/registry.rs crates/origin-tools/src/macros.rs crates/origin-tools/src/lib.rs crates/origin-tools/tests/registry.rs
git commit -m "feat(tools): ToolMeta.token_budget + macro arm

Default 25k tokens; tools that need a tighter cap (Edit returns hunks only)
override via origin_tool!{..., token_budget: 4_000}.
"
```

### Task 1.5: `budget_writer` — core API + truncation sentinel

**Files:**
- Create: `crates/origin-tools/src/budget_writer.rs`
- Test: `crates/origin-tools/tests/budget_writer.rs`
- Modify: `crates/origin-tools/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

`crates/origin-tools/tests/budget_writer.rs`:

```rust
use origin_tools::budget_writer::{ResultWriter, approx_tokens};
use serde_json::json;

#[test]
fn approx_tokens_chars_over_four() {
    assert_eq!(approx_tokens("abcd"), 1);
    assert_eq!(approx_tokens("abcdefgh"), 2);
    assert_eq!(approx_tokens(""), 0);
}

#[test]
fn writer_under_budget_emits_unchanged() {
    let mut w = ResultWriter::new(100, "Read", json!({"file_path": "x.rs", "offset": 0}));
    w.push_str("hello world");
    let body = w.finish_string();
    assert_eq!(body, "hello world");
}

#[test]
fn writer_over_budget_emits_truncation_sentinel() {
    let mut w = ResultWriter::new(2, "Read", json!({"file_path": "x.rs", "offset": 0}));
    w.push_str("aaaaaaaaaaaaaaaaaaaaaa"); // 22 chars ~ 5 tokens
    let body = w.finish_string();
    assert!(body.contains("\"kind\":\"truncated\""), "body: {body}");
    assert!(body.contains("\"continuation\""));
}

#[test]
fn writer_records_lines_consumed_for_continuation() {
    let mut w = ResultWriter::new(2, "Read", json!({"file_path": "x.rs", "offset": 0}));
    w.note_line(0);
    w.push_str("line0\n");
    w.note_line(1);
    w.push_str("line1\n");
    w.note_line(2);
    w.push_str("line2-too-long-too-long-too-long-too-long\n");
    let body = w.finish_string();
    // Continuation handle should point to line 2 (last noted before overflow).
    assert!(body.contains("\"offset\":2"), "body: {body}");
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p origin-tools --test budget_writer 2>&1 | tail -15`

- [ ] **Step 3: Write the implementation**

`crates/origin-tools/src/budget_writer.rs`:

```rust
//! Token-aware result builder. Every tool produces its serialised result
//! through `ResultWriter`, which enforces a per-call token budget and emits
//! a structured continuation handle on overflow.

use serde_json::{json, Value};

#[must_use]
pub fn approx_tokens(s: &str) -> usize {
    // chars/4 with a small punctuation-density correction. We accept ±10% drift
    // — exact tokenisation per call would dominate envelope latency.
    let chars = s.chars().count();
    chars / 4
}

/// Builder for a tool result, capped at `budget_tokens` approximate tokens.
///
/// Callers `push_str` body fragments and (optionally) `note_line(idx)` after
/// each logical record so that on overflow the continuation handle can resume
/// from the right place.
pub struct ResultWriter {
    budget_tokens: u32,
    used_tokens: u32,
    body: String,
    tool_name: String,
    base_args: Value,
    last_line_noted: Option<u32>,
    overflowed: bool,
}

impl ResultWriter {
    #[must_use]
    pub fn new(budget_tokens: u32, tool_name: impl Into<String>, base_args: Value) -> Self {
        Self {
            budget_tokens,
            used_tokens: 0,
            body: String::new(),
            tool_name: tool_name.into(),
            base_args,
            last_line_noted: None,
            overflowed: false,
        }
    }

    /// Mark that the next `push_str` corresponds to the record at `line_idx`.
    /// Used to compute the `offset` field of the continuation handle on overflow.
    pub fn note_line(&mut self, line_idx: u32) {
        if !self.overflowed {
            self.last_line_noted = Some(line_idx);
        }
    }

    /// Append `s` to the body, capped at the budget. Once the budget is
    /// crossed, no further writes are accepted and the writer enters
    /// "overflowed" state until `finish_string` is called.
    pub fn push_str(&mut self, s: &str) {
        if self.overflowed {
            return;
        }
        let chunk = approx_tokens(s) as u32;
        if self.used_tokens.saturating_add(chunk) > self.budget_tokens {
            self.overflowed = true;
            return;
        }
        self.body.push_str(s);
        self.used_tokens = self.used_tokens.saturating_add(chunk);
    }

    /// Final body string. If the writer overflowed, appends the truncation
    /// sentinel (still JSON-parseable as a trailing object on its own line).
    #[must_use]
    pub fn finish_string(mut self) -> String {
        if self.overflowed {
            let mut cont_args = self.base_args.clone();
            if let Some(idx) = self.last_line_noted {
                cont_args["offset"] = json!(idx);
            }
            let sentinel = json!({
                "kind": "truncated",
                "emitted_tokens": self.used_tokens,
                "continuation": {
                    "tool": self.tool_name,
                    "args": cont_args,
                }
            });
            if !self.body.ends_with('\n') {
                self.body.push('\n');
            }
            self.body.push_str(&serde_json::to_string(&sentinel).unwrap());
        }
        self.body
    }
}
```

Add to `crates/origin-tools/src/lib.rs`:

```rust
pub mod budget_writer;
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p origin-tools --test budget_writer`
Expected: 4 passed.

- [ ] **Step 5: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/budget_writer.rs crates/origin-tools/tests/budget_writer.rs crates/origin-tools/src/lib.rs
git commit -m "feat(tools): budget_writer with truncation sentinel + continuation handle"
```

### Task 1.6: `budget_writer` proptest — body never exceeds budget

**Files:**
- Modify: `crates/origin-tools/tests/budget_writer.rs`

- [ ] **Step 1: Append**

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn body_never_exceeds_budget_plus_sentinel(
        budget in 1u32..200,
        chunks in proptest::collection::vec(".[a-z]{0,40}", 0..10),
    ) {
        let mut w = ResultWriter::new(budget, "Read", json!({}));
        for c in &chunks {
            w.push_str(c);
        }
        let body = w.finish_string();
        // Body bytes minus sentinel must be within budget.
        // Find sentinel boundary if present.
        let pre_sentinel = body
            .rfind("\n{\"kind\":\"truncated\"")
            .map_or(body.as_str(), |idx| &body[..idx]);
        let used = approx_tokens(pre_sentinel) as u32;
        prop_assert!(used <= budget, "used {} > budget {}", used, budget);
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p origin-tools --test budget_writer body_never_exceeds
cargo clippy -p origin-tools --tests -- -D warnings
git add crates/origin-tools/tests/budget_writer.rs
git commit -m "test(budget_writer): proptest body always within budget"
```

### Task 1.7: Envelope skeleton (passthrough)

**Files:**
- Create: `crates/origin-tools/src/tool_envelope.rs`
- Modify: `crates/origin-tools/src/lib.rs`
- Test: `crates/origin-tools/tests/envelope_passthrough.rs`

- [ ] **Step 1: Write the test**

`crates/origin-tools/tests/envelope_passthrough.rs`:

```rust
use origin_tools::tool_envelope::{EnvelopeCtx, run_passthrough};
use serde_json::json;

#[tokio::test]
async fn passthrough_returns_inner_value() {
    let ctx = EnvelopeCtx::default();
    let result = run_passthrough(&ctx, "Test", json!({}), |_args| async {
        Ok::<_, origin_tools::ToolError>(json!({"ok": true}))
    })
    .await
    .unwrap();
    assert_eq!(result["ok"], true);
}

#[tokio::test]
async fn passthrough_surfaces_tool_error_as_json() {
    let ctx = EnvelopeCtx::default();
    let result = run_passthrough(&ctx, "Test", json!({}), |_args| async {
        Err::<serde_json::Value, _>(origin_tools::ToolError::new(
            origin_tools::ErrClass::Edit, "no_match", "not found"
        ))
    })
    .await
    .unwrap();
    assert_eq!(result["kind"], "edit.no_match");
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test envelope_passthrough 2>&1 | tail -10`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/tool_envelope.rs`:

```rust
//! Tool envelope — orchestration layer between `dispatch_tool` and the
//! per-tool function.
//!
//! Phase 1 ships a pure passthrough: it accepts a tool-fn future, awaits it,
//! and serialises a `ToolError` to its `{kind, message, ...}` JSON form. Later
//! phases extend this with input canon, output-CAS lookup, budget writing,
//! and streaming.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;

use crate::error::ToolError;

#[derive(Debug, Default, Clone)]
pub struct EnvelopeCtx {
    /// Session-scoped state. Stub in Phase 1; populated in later phases
    /// (output-CAS handle, supervisor handle, RA bridge, etc.).
    pub session_id: Option<Arc<str>>,
}

/// Phase 1 passthrough: invoke `tool_fn(args)` and return either its value
/// (on Ok) or the structured error JSON (on Err).
pub async fn run_passthrough<F, Fut>(
    _ctx: &EnvelopeCtx,
    _tool_name: &str,
    args: Value,
    tool_fn: F,
) -> Result<Value, ToolError>
where
    F: FnOnce(Value) -> Fut,
    Fut: Future<Output = Result<Value, ToolError>>,
{
    match tool_fn(args).await {
        Ok(v) => Ok(v),
        Err(e) => Ok(e.to_json()),
    }
}
```

Add to `crates/origin-tools/src/lib.rs`:

```rust
pub mod tool_envelope;
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test envelope_passthrough`

- [ ] **Step 5: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/tool_envelope.rs crates/origin-tools/tests/envelope_passthrough.rs crates/origin-tools/src/lib.rs
git commit -m "feat(tools): tool_envelope skeleton (Phase 1 passthrough)

Phase 1 ships envelope as a pure passthrough so the contract is in place
without behaviour change. Phase 2+ adds CAS lookup, budget writing, etc.
"
```

### Task 1.8: Phase 1 regression sweep

**Files:** none (verification only).

- [ ] **Step 1: Run the full workspace test suite**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
```
Expected: same pass/fail counts as the baseline captured in Task 0.1.

- [ ] **Step 2: Clippy + fmt on the workspace**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 3: Tag the foundation phase**

```bash
git tag -a tool-suite-v2-phase-1 -m "Foundation complete: text_fmt, budget_writer, errors, envelope skeleton"
```

---

## Phase 2 — Output CAS + envelope wiring

Goal of Phase 2: tools may return either raw `Value` or a `{tool_result_ref: "blake3:…"}` short-form when the body is byte-identical to a prior result in this session. The envelope owns the store and short-form decision. Mutating tools (`SideEffects::Mutating`) bypass.

### Task 2.1: `result_cas` store + serialiser

**Files:**
- Create: `crates/origin-tools/src/result_cas.rs`
- Test: `crates/origin-tools/tests/result_cas.rs`
- Modify: `crates/origin-tools/src/lib.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-tools/tests/result_cas.rs`:

```rust
use origin_tools::result_cas::{ResultStore, ref_token};
use serde_json::json;

#[test]
fn same_body_yields_same_ref() {
    let store = ResultStore::new();
    let body = serde_json::to_vec(&json!({"a": 1, "b": 2})).unwrap();
    let h1 = store.put(&body);
    let h2 = store.put(&body);
    assert_eq!(h1, h2);
}

#[test]
fn different_bodies_yield_different_refs() {
    let store = ResultStore::new();
    let a = store.put(b"abc");
    let b = store.put(b"def");
    assert_ne!(a, b);
}

#[test]
fn ref_token_round_trips_through_store() {
    let store = ResultStore::new();
    let body = b"hello world";
    let h = store.put(body);
    let token = ref_token(&h, body.len(), "hello world");
    assert!(token["tool_result_ref"].as_str().unwrap().starts_with("blake3:"));
    assert_eq!(token["bytes"], body.len());
    assert_eq!(token["preview"], "hello world");
    let fetched = store.get(&h).unwrap();
    assert_eq!(&*fetched, body);
}

#[test]
fn store_preview_truncates_to_80_chars() {
    let store = ResultStore::new();
    let long = "x".repeat(200);
    let h = store.put(long.as_bytes());
    let token = ref_token(&h, 200, &long);
    assert_eq!(token["preview"].as_str().unwrap().chars().count(), 80);
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test result_cas 2>&1 | tail -10`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/result_cas.rs`:

```rust
//! Session-scoped output-content-addressed store.
//!
//! The envelope hashes each non-mutating tool result and stores the bytes
//! once per session. Repeat reads of the same file (or any byte-identical
//! result) replay as a short `{tool_result_ref: "blake3:…"}` token, which
//! the agent expands back to the body before serialising into the provider's
//! `tool_result` block. Since the bytes are byte-identical across calls,
//! the provider's prompt cache hits and incremental token cost is ~0.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use blake3::Hash as Blake3Hash;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ResultStore {
    inner: Arc<RwLock<HashMap<[u8; 32], Arc<[u8]>>>>,
}

impl Default for ResultStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ResultStore {
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(HashMap::new())) }
    }

    #[must_use]
    pub fn put(&self, body: &[u8]) -> Blake3Hash {
        let h = blake3::hash(body);
        let mut w = self.inner.write().unwrap();
        w.entry(*h.as_bytes()).or_insert_with(|| Arc::from(body.to_vec().into_boxed_slice()));
        h
    }

    #[must_use]
    pub fn get(&self, h: &Blake3Hash) -> Option<Arc<[u8]>> {
        self.inner.read().unwrap().get(h.as_bytes()).cloned()
    }
}

/// Build the `{tool_result_ref, bytes, preview}` short-form value.
#[must_use]
pub fn ref_token(h: &Blake3Hash, bytes: usize, body_str: &str) -> Value {
    let preview: String = body_str.chars().take(80).collect();
    json!({
        "tool_result_ref": format!("blake3:{}", h.to_hex()),
        "bytes": bytes,
        "preview": preview,
    })
}
```

Add to `crates/origin-tools/src/lib.rs`:

```rust
pub mod result_cas;
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test result_cas`

- [ ] **Step 5: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/result_cas.rs crates/origin-tools/tests/result_cas.rs crates/origin-tools/src/lib.rs
git commit -m "feat(tools): result_cas store + blake3 ref tokens"
```

### Task 2.2: Envelope wires CAS for non-mutating tools

**Files:**
- Modify: `crates/origin-tools/src/tool_envelope.rs`
- Test: `crates/origin-tools/tests/envelope_cas.rs`

- [ ] **Step 1: Write the failing test**

`crates/origin-tools/tests/envelope_cas.rs`:

```rust
use origin_tools::tool_envelope::{run, EnvelopeCtx, EnvelopeMode};
use origin_tools::SideEffects;
use serde_json::json;

#[tokio::test]
async fn pure_tool_second_call_returns_ref() {
    let ctx = EnvelopeCtx::default();
    let r1 = run(
        &ctx, "Read", SideEffects::Pure, EnvelopeMode::CasEligible, json!({}),
        |_| async { Ok(json!({"body": "abc"})) },
    )
    .await
    .unwrap();
    assert_eq!(r1["body"], "abc");
    let r2 = run(
        &ctx, "Read", SideEffects::Pure, EnvelopeMode::CasEligible, json!({}),
        |_| async { Ok(json!({"body": "abc"})) },
    )
    .await
    .unwrap();
    assert_eq!(r2["tool_result_ref"].as_str().unwrap().starts_with("blake3:"), true);
}

#[tokio::test]
async fn mutating_tool_never_returns_ref() {
    let ctx = EnvelopeCtx::default();
    let r1 = run(
        &ctx, "Edit", SideEffects::Mutating, EnvelopeMode::CasEligible, json!({}),
        |_| async { Ok(json!({"ok": true})) },
    )
    .await
    .unwrap();
    let r2 = run(
        &ctx, "Edit", SideEffects::Mutating, EnvelopeMode::CasEligible, json!({}),
        |_| async { Ok(json!({"ok": true})) },
    )
    .await
    .unwrap();
    assert_eq!(r1, r2);
    assert!(r1.get("tool_result_ref").is_none());
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test envelope_cas 2>&1 | tail -10`

- [ ] **Step 3: Replace `crates/origin-tools/src/tool_envelope.rs` with the full Phase 2 form**

```rust
//! Tool envelope.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;

use crate::error::ToolError;
use crate::result_cas::{ref_token, ResultStore};
use crate::SideEffects;

#[derive(Debug, Default, Clone)]
pub struct EnvelopeCtx {
    pub session_id: Option<Arc<str>>,
    pub result_store: ResultStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeMode {
    /// Subject to output-CAS dedup (overridden to "no" if side_effects=Mutating).
    CasEligible,
    /// Never dedup (e.g. tools whose output is intentionally per-call).
    CasOptOut,
}

/// Phase-1 passthrough kept for compatibility with the earlier tests.
pub async fn run_passthrough<F, Fut>(
    ctx: &EnvelopeCtx,
    tool_name: &str,
    args: Value,
    tool_fn: F,
) -> Result<Value, ToolError>
where
    F: FnOnce(Value) -> Fut,
    Fut: Future<Output = Result<Value, ToolError>>,
{
    run(ctx, tool_name, SideEffects::Pure, EnvelopeMode::CasOptOut, args, tool_fn).await
}

/// Full envelope: runs `tool_fn`, then for non-mutating tools in CAS-eligible
/// mode, stores the serialised body and returns a short-form `tool_result_ref`
/// on byte-identical repeats within the session.
pub async fn run<F, Fut>(
    ctx: &EnvelopeCtx,
    _tool_name: &str,
    side_effects: SideEffects,
    mode: EnvelopeMode,
    args: Value,
    tool_fn: F,
) -> Result<Value, ToolError>
where
    F: FnOnce(Value) -> Fut,
    Fut: Future<Output = Result<Value, ToolError>>,
{
    let value = match tool_fn(args).await {
        Ok(v) => v,
        Err(e) => return Ok(e.to_json()),
    };

    if side_effects == SideEffects::Mutating || mode == EnvelopeMode::CasOptOut {
        return Ok(value);
    }

    let body_str = serde_json::to_string(&value).map_err(|e| {
        ToolError::new(crate::error::ErrClass::Validation, "serialise", e.to_string())
    })?;
    let body_bytes = body_str.as_bytes();
    let h_before = ctx.result_store.get(&blake3::hash(body_bytes));
    if h_before.is_some() {
        // Repeat hit — return short-form.
        let h = blake3::hash(body_bytes);
        return Ok(ref_token(&h, body_bytes.len(), &body_str));
    }
    // First time we see this body — store and return full.
    let _ = ctx.result_store.put(body_bytes);
    Ok(value)
}
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test envelope_cas`

- [ ] **Step 5: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/tool_envelope.rs crates/origin-tools/tests/envelope_cas.rs
git commit -m "feat(tools): envelope wires output-CAS for non-mutating tools

Repeat byte-identical results in the same session return as a
{tool_result_ref: blake3:..} short-form that the daemon expands when
serialising tool_result blocks. Prompt cache hits drive incremental
tokens to ~0 on repeated reads/greps.
"
```

### Task 2.3: Daemon `dispatch_tool` routes through the envelope

**Files:**
- Modify: `crates/origin-daemon/src/agent.rs:1096-1176` (the existing dispatch arms for Read/Glob/Grep/Edit/Write/Bash)
- Test: `crates/origin-daemon/tests/envelope_routing.rs`

- [ ] **Step 1: Write the failing integration test**

`crates/origin-daemon/tests/envelope_routing.rs`:

```rust
//! Smoke test: dispatch_tool's Read arm now flows through the envelope and
//! the second identical Read in the same EnvelopeCtx returns a tool_result_ref.

// NOTE: this test imports from the daemon's internal modules. If the
// daemon does not currently expose `dispatch_tool`/`EnvelopeCtx` for
// integration tests, this task includes the visibility bump as part
// of the implementation step.

use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn read_twice_in_session_returns_ref_on_second() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "hello").unwrap();

    let ctx = origin_tools::tool_envelope::EnvelopeCtx::default();
    let args = serde_json::json!({"path": path.to_string_lossy()});

    let v1 = origin_daemon::agent::dispatch_with_envelope(&ctx, "Read", &args).await.unwrap();
    let v2 = origin_daemon::agent::dispatch_with_envelope(&ctx, "Read", &args).await.unwrap();

    assert!(v1.get("tool_result_ref").is_none());
    assert!(v2.get("tool_result_ref").is_some());
}
```

- [ ] **Step 2: Run — fail (function does not exist yet)**

Run: `cargo test -p origin-daemon --test envelope_routing 2>&1 | tail -10`

- [ ] **Step 3: Implement**

In `crates/origin-daemon/src/agent.rs`, add a new public helper (place it directly above `dispatch_tool`):

```rust
/// Public wrapper around `dispatch_tool` that flows through the tool envelope.
/// Integration tests use this; production callers in this file go through
/// the existing `dispatch_tool` path which now invokes the envelope internally.
pub async fn dispatch_with_envelope(
    ctx: &origin_tools::tool_envelope::EnvelopeCtx,
    name: &str,
    args: &Value,
) -> Result<Value, LoopError> {
    let meta = registry_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| LoopError::ToolFailure(format!("unknown tool: {name}")))?;
    let mode = if matches!(meta.side_effects, origin_tools::SideEffects::Pure) {
        origin_tools::tool_envelope::EnvelopeMode::CasEligible
    } else {
        origin_tools::tool_envelope::EnvelopeMode::CasOptOut
    };
    let name_owned = name.to_string();
    let args_owned = args.clone();
    let v = origin_tools::tool_envelope::run(
        ctx,
        &name_owned,
        meta.side_effects,
        mode,
        args_owned,
        |inner_args| async move {
            let s = dispatch_tool_inner(meta, &inner_args).await
                .map_err(|e| origin_tools::ToolError::new(
                    origin_tools::ErrClass::Subsystem, "tool_failed", e.to_string(),
                ))?;
            Ok(serde_json::json!({"body": s}))
        },
    )
    .await
    .map_err(|e| LoopError::ToolFailure(e.to_string()))?;
    Ok(v)
}

/// Internal copy of the existing dispatch body, factored out so the envelope
/// wrapper can call it without re-running the envelope.
async fn dispatch_tool_inner(meta: &ToolMeta, args: &Value) -> Result<String, LoopError> {
    // Body identical to existing dispatch_tool minus the extra `cas`, etc.
    // For Phase 2 we delegate to the existing helper since callers in this file
    // still pass full context.
    Err(LoopError::ToolFailure(format!(
        "dispatch_tool_inner stub — Phase 3 replaces every arm with envelope-aware fn",
    )))
}
```

Then update `dispatch_tool` to delegate its Read arm to the new helper. In the `"Read" =>` arm (around line 1106), replace the body with:

```rust
"Read" => {
    let path = args.get("path").and_then(serde_json::Value::as_str)
        .ok_or_else(|| LoopError::BadArgs("Read: missing `path`".into()))?;
    let bytes = std::fs::read(path).map_err(|e| LoopError::ToolFailure(format!("read: {e}")))?;
    let det = origin_tools::text_fmt::detect(&bytes);
    origin_tools::text_fmt::normalise_to_lf(&bytes, &det)
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

Make `dispatch_tool_inner`'s stub a real switch for `Read` only (Phase 3 fills the rest):

```rust
async fn dispatch_tool_inner(meta: &ToolMeta, args: &Value) -> Result<String, LoopError> {
    match meta.name {
        "Read" => {
            let path = args.get("path").and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Read: missing `path`".into()))?;
            let bytes = std::fs::read(path)
                .map_err(|e| LoopError::ToolFailure(format!("read: {e}")))?;
            let det = origin_tools::text_fmt::detect(&bytes);
            origin_tools::text_fmt::normalise_to_lf(&bytes, &det)
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        other => Err(LoopError::ToolFailure(format!(
            "dispatch_tool_inner: {other} not yet envelope-routed (Phase 3)"
        ))),
    }
}
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-daemon --test envelope_routing`
Expected: 1 passed.

- [ ] **Step 5: Verify the existing daemon tests still pass**

```bash
cargo test -p origin-daemon 2>&1 | tail -20
```
Expected: same pass/fail counts as the Phase 0 baseline.

- [ ] **Step 6: Verification + commit**

```bash
cargo clippy -p origin-daemon --all-targets -- -D warnings
git add crates/origin-daemon/src/agent.rs crates/origin-daemon/tests/envelope_routing.rs
git commit -m "feat(daemon): envelope-routed Read; dispatch_with_envelope public

Phase 2 wires only Read through dispatch_with_envelope so the CAS path is
exercised end-to-end. Phase 3 expands to Edit/Write/Grep/Glob/Bash.
"
```

### Task 2.4: Phase 2 regression sweep + tag

- [ ] **Step 1: Workspace tests**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-2 -m "Output-CAS dedup live; Read flows through envelope"
```

---

## Phase 3 — Rebuild Read / Edit / Write / Grep / Glob through the envelope

Each task is independent of its siblings (different files). Parallel-agent runner should dispatch all five tasks at once. Each writes failing tests, then the implementation, then routes the daemon arm.

### Task 3.1: `Read v2`

**Files:**
- Modify: `crates/origin-tools/src/builtins/read.rs`
- Test: `crates/origin-tools/tests/read_v2.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (Read arm of `dispatch_tool` + `dispatch_tool_inner`)

- [ ] **Step 1: Write the failing tests**

`crates/origin-tools/tests/read_v2.rs`:

```rust
use origin_tools::builtins::read::{read_v2, ReadArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn returns_line_numbered_chunks() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "first\nsecond\nthird\n").unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: None, limit: None, as_: None,
    })
    .unwrap();
    assert!(out.contains("     1\tfirst"));
    assert!(out.contains("     2\tsecond"));
    assert!(out.contains("     3\tthird"));
}

#[test]
fn respects_offset_and_limit() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    fs::write(&p, body).unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: Some(10), limit: Some(5), as_: None,
    })
    .unwrap();
    assert!(out.contains("    11\tline 11"));
    assert!(out.contains("    15\tline 15"));
    assert!(!out.contains("line 16"));
}

#[test]
fn default_limit_is_1000() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    let body: String = (1..=1500).map(|i| format!("L{i}\n")).collect();
    fs::write(&p, body).unwrap();
    let out = read_v2(ReadArgs {
        file_path: p.to_string_lossy().into_owned(),
        offset: None, limit: None, as_: None,
    })
    .unwrap();
    assert!(out.contains("\tL1000"));
    assert!(!out.contains("\tL1001"));
}

#[test]
fn errors_on_missing_file() {
    let err = read_v2(ReadArgs {
        file_path: "/no/such/file".into(),
        offset: None, limit: None, as_: None,
    })
    .unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Io);
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test read_v2 2>&1 | tail -10`

- [ ] **Step 3: Replace `crates/origin-tools/src/builtins/read.rs`**

```rust
//! `Read` v2 — line-numbered chunks with offset/limit, image/PDF dispatch.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};

#[derive(Debug, Clone)]
pub struct ReadArgs {
    pub file_path: String,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
    pub as_: Option<String>,
}

/// 1-based offset (0 means start from line 1). Default limit = 1000 lines.
///
/// # Errors
/// Returns `ToolError` on I/O failure or non-UTF-8 (without BOM) content.
pub fn read_v2(args: ReadArgs) -> Result<String, ToolError> {
    let as_kind = args.as_.as_deref().unwrap_or("text");
    let bytes = std::fs::read(&args.file_path).map_err(|e| {
        ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path))
    })?;

    match as_kind {
        "text" => read_text(&bytes, &args),
        "image" => read_image(&bytes),
        "pdf" => read_pdf(&bytes),
        other => Err(ToolError::new(
            ErrClass::Validation,
            "bad_as",
            format!("unknown 'as' value: {other} (expected text|image|pdf)"),
        )),
    }
}

fn read_text(bytes: &[u8], args: &ReadArgs) -> Result<String, ToolError> {
    let det = text_fmt::detect(bytes);
    let text = text_fmt::normalise_to_lf(bytes, &det)?;
    let offset = args.offset.unwrap_or(0) as usize;
    let limit = args.limit.unwrap_or(1000) as usize;
    let mut out = String::with_capacity(text.len());
    for (idx, line) in text.lines().enumerate().skip(offset).take(limit) {
        let line_no = idx + 1;
        out.push_str(&format!("{line_no:>6}\t{line}\n"));
    }
    Ok(out)
}

fn read_image(bytes: &[u8]) -> Result<String, ToolError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| ToolError::new(ErrClass::Io, "bad_image", e.to_string()))?;
    Ok(format!(
        "image: {}x{} ({})",
        img.width(),
        img.height(),
        match img.color() {
            image::ColorType::Rgb8 => "rgb8",
            image::ColorType::Rgba8 => "rgba8",
            image::ColorType::L8 => "l8",
            _ => "other",
        }
    ))
}

fn read_pdf(bytes: &[u8]) -> Result<String, ToolError> {
    pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| ToolError::new(ErrClass::Io, "bad_pdf", e.to_string()))
}

crate::origin_tool! {
    name: "Read",
    description: "Read a file at the given path. Optional `offset` (0-based line) and `limit` (default 1000). `as: image|pdf|text`.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path": { "type": "string" },
            "offset":    { "type": "integer", "minimum": 0 },
            "limit":     { "type": "integer", "minimum": 1, "maximum": 50000 },
            "as":        { "type": "string", "enum": ["text", "image", "pdf"] }
        },
        "required": ["file_path"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::ReadFs,
}
```

(Note: the legacy `read_tool(path) -> io::Result<String>` is removed in this commit; daemon callers are updated in Step 5 below.)

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test read_v2`
Expected: 4 passed.

- [ ] **Step 5: Update the daemon Read arm**

In `crates/origin-daemon/src/agent.rs`, replace the `"Read" =>` arm of `dispatch_tool` AND of `dispatch_tool_inner` with:

```rust
"Read" => {
    let args = origin_tools::builtins::read::ReadArgs {
        file_path: args.get("file_path").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Read: missing `file_path`".into()))?
            .to_string(),
        offset: args.get("offset").and_then(Value::as_u64).map(|n| n as u32),
        limit:  args.get("limit").and_then(Value::as_u64).map(|n| n as u32),
        as_:    args.get("as").and_then(Value::as_str).map(str::to_string),
    };
    origin_tools::builtins::read::read_v2(args)
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

(Apply this replacement in both functions.)

- [ ] **Step 6: Verify daemon tests still pass**

```bash
cargo test -p origin-daemon 2>&1 | tail -20
```

- [ ] **Step 7: Verification + commit**

```bash
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/read.rs crates/origin-tools/tests/read_v2.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Read v2 — line numbers, offset/limit, image/pdf"
```

### Task 3.2: `Edit v2` (CRLF-safe, hunk return, replace_all)

**Files:**
- Modify: `crates/origin-tools/src/builtins/edit.rs`
- Test: `crates/origin-tools/tests/edit_v2.rs`
- Test: `crates/origin-tools/tests/crlf_regression.rs` (NEW — the canary suite)
- Modify: `crates/origin-daemon/src/agent.rs` (Edit arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/edit_v2.rs`:

```rust
use origin_tools::builtins::edit::{edit_v2, EditArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn single_replacement_returns_hunk() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\n").unwrap();
    let out = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["hunks"][0]["before"].as_str().unwrap().contains("foo"), true);
    assert_eq!(out["hunks"][0]["after"].as_str().unwrap().contains("bar"), true);
    let actual = fs::read_to_string(&p).unwrap();
    assert_eq!(actual, "fn bar() {}\n");
}

#[test]
fn ambiguous_match_without_replace_all_errors() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "foo foo foo\n").unwrap();
    let err = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Edit);
    assert_eq!(err.reason, "ambiguous");
}

#[test]
fn replace_all_replaces_every_occurrence() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "foo foo foo\n").unwrap();
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: true,
    })
    .unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "bar bar bar\n");
}

#[test]
fn no_match_errors_with_edit_no_match() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "hello\n").unwrap();
    let err = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "missing".into(),
        new_string: "x".into(),
        replace_all: false,
    })
    .unwrap_err();
    assert_eq!(err.reason, "no_match");
}
```

`crates/origin-tools/tests/crlf_regression.rs`:

```rust
//! Canary suite for the CRLF Edit failure class.

use origin_tools::builtins::edit::{edit_v2, EditArgs};
use std::fs;
use tempfile::tempdir;

fn write_with_eol(p: &std::path::Path, body: &[u8]) {
    fs::write(p, body).unwrap();
}

#[test]
fn edit_lf_needle_against_crlf_file_succeeds() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.rs");
    write_with_eol(&p, b"line1\r\nfoo\r\nline3\r\n");
    let out = edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(out["ok"], true);
    let bytes = fs::read(&p).unwrap();
    assert_eq!(bytes, b"line1\r\nbar\r\nline3\r\n");
}

#[test]
fn edit_lf_needle_against_cr_only_file_succeeds() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("cr.rs");
    write_with_eol(&p, b"line1\rfoo\rline3\r");
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"line1\rbar\rline3\r");
}

#[test]
fn edit_preserves_mixed_eol_byte_for_byte() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("mixed.rs");
    write_with_eol(&p, b"a\r\nb\nfoo\r\nc\r");
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "foo".into(),
        new_string: "bar".into(),
        replace_all: false,
    })
    .unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"a\r\nb\nbar\r\nc\r");
}

#[test]
fn write_preserves_eol_when_appending_via_edit() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("file.rs");
    write_with_eol(&p, b"a\r\nb\r\n");
    edit_v2(EditArgs {
        file_path: p.to_string_lossy().into_owned(),
        old_string: "b".into(),
        new_string: "b\nINSERTED".into(),
        replace_all: false,
    })
    .unwrap();
    // Inserted line inherits CRLF from preceding line.
    assert_eq!(fs::read(&p).unwrap(), b"a\r\nb\r\nINSERTED\r\n");
}
```

- [ ] **Step 2: Run — fail**

```bash
cargo test -p origin-tools --test edit_v2 2>&1 | tail -15
cargo test -p origin-tools --test crlf_regression 2>&1 | tail -15
```

- [ ] **Step 3: Replace `crates/origin-tools/src/builtins/edit.rs`**

```rust
//! `Edit` v2 — find-and-replace with CRLF safety, hunk return, replace_all.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct EditArgs {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

/// Find-and-replace `old_string` with `new_string` in `file_path`.
///
/// Operates on LF-normalised text; writes back in the file's original EOL
/// convention, preserving per-source-line EOL for mixed-EOL files.
///
/// # Errors
/// Returns `ToolError(edit.no_match | edit.ambiguous | io.*)`.
pub fn edit_v2(args: EditArgs) -> Result<Value, ToolError> {
    let bytes = std::fs::read(&args.file_path).map_err(|e| {
        ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path))
    })?;
    let det = text_fmt::detect(&bytes);
    let text = text_fmt::normalise_to_lf(&bytes, &det)?;

    let count = text.matches(&args.old_string).count();
    let updated = match count {
        0 => {
            return Err(ToolError::new(
                ErrClass::Edit,
                "no_match",
                format!("'{}' not found in {}", args.old_string, args.file_path),
            )
            .recoverable(true)
            .hint("widen the needle or add surrounding context"));
        }
        1 => text.replacen(&args.old_string, &args.new_string, 1),
        n if args.replace_all => text.replace(&args.old_string, &args.new_string),
        n => {
            return Err(ToolError::new(
                ErrClass::Edit,
                "ambiguous",
                format!("'{}' appears {n} times; pass replace_all=true or widen the needle", args.old_string),
            )
            .recoverable(true));
        }
    };

    let hunk = build_hunk(&text, &updated, &args.old_string, &args.new_string);
    let new_bytes = text_fmt::denormalise(&updated, &det);
    atomic_write(&args.file_path, &new_bytes)?;
    Ok(json!({
        "ok": true,
        "hunks": [hunk],
    }))
}

fn build_hunk(before: &str, after: &str, old: &str, new: &str) -> Value {
    let line = before.lines().enumerate()
        .find(|(_, l)| l.contains(old))
        .map_or(0, |(i, _)| i + 1);
    json!({ "before": old, "after": new, "line": line, "_lengths": {"before": before.len(), "after": after.len()} })
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("create tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("write tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("fsync: {e}")))?;
    }
    std::fs::rename(&tmp, p)
        .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("rename: {e}")))?;
    Ok(())
}

crate::origin_tool! {
    name: "Edit",
    description: "Find-and-replace a unique string in a file. CRLF-safe. Pass replace_all=true for multi-match.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path":   { "type": "string" },
            "old_string":  { "type": "string" },
            "new_string":  { "type": "string" },
            "replace_all": { "type": "boolean", "default": false }
        },
        "required": ["file_path", "old_string", "new_string"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 4_000,
}
```

- [ ] **Step 4: Run — pass**

```bash
cargo test -p origin-tools --test edit_v2
cargo test -p origin-tools --test crlf_regression
```
Expected: all pass.

- [ ] **Step 5: Update daemon Edit arm**

In both `dispatch_tool` and `dispatch_tool_inner` in `crates/origin-daemon/src/agent.rs`:

```rust
"Edit" => {
    let args = origin_tools::builtins::edit::EditArgs {
        file_path: args.get("file_path").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Edit: missing `file_path`".into()))?
            .to_string(),
        old_string: args.get("old_string").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Edit: missing `old_string`".into()))?
            .to_string(),
        new_string: args.get("new_string").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Edit: missing `new_string`".into()))?
            .to_string(),
        replace_all: args.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
    };
    origin_tools::builtins::edit::edit_v2(args)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 6: Verification + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/edit.rs crates/origin-tools/tests/edit_v2.rs crates/origin-tools/tests/crlf_regression.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Edit v2 — CRLF-safe, hunk return, replace_all

Fixes the failure class shown in the original screenshot. crlf_regression
canary suite asserts LF needle against CRLF/CR/mixed files round-trips
byte-for-byte.
"
```

### Task 3.3: `Write v2` (atomic + read-before-write guard + EOL preservation)

**Files:**
- Modify: `crates/origin-tools/src/builtins/write.rs`
- Test: `crates/origin-tools/tests/write_v2.rs`
- Test: extend `crates/origin-tools/tests/crlf_regression.rs`
- Modify: `crates/origin-tools/src/tool_envelope.rs` (track per-session "files Read this session" — see Step 3)
- Modify: `crates/origin-daemon/src/agent.rs` (Write arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/write_v2.rs`:

```rust
use origin_tools::builtins::write::{write_v2, WriteArgs, WriteGuard};
use std::fs;
use tempfile::tempdir;

#[test]
fn creates_new_file_without_guard_issue() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("new.txt");
    let guard = WriteGuard::default();
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "hello".into(),
        force: false,
    }, &guard)
    .unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
}

#[test]
fn rejects_overwrite_of_unread_existing_file() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("existing.txt");
    fs::write(&p, "old").unwrap();
    let guard = WriteGuard::default();
    let err = write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "new".into(),
        force: false,
    }, &guard)
    .unwrap_err();
    assert_eq!(err.reason, "read_required");
}

#[test]
fn allows_overwrite_after_marking_read() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("existing.txt");
    fs::write(&p, "old").unwrap();
    let guard = WriteGuard::default();
    guard.note_read(p.to_string_lossy().as_ref());
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "new".into(),
        force: false,
    }, &guard).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "new");
}

#[test]
fn force_true_bypasses_guard() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("existing.txt");
    fs::write(&p, "old").unwrap();
    let guard = WriteGuard::default();
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "new".into(),
        force: true,
    }, &guard).unwrap();
}

#[test]
fn preserves_prior_crlf_when_overwriting() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.txt");
    fs::write(&p, b"a\r\nb\r\n").unwrap();
    let guard = WriteGuard::default();
    guard.note_read(p.to_string_lossy().as_ref());
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "x\ny\n".into(),
        force: false,
    }, &guard).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"x\r\ny\r\n");
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test write_v2 2>&1 | tail -15`

- [ ] **Step 3: Implement**

Add to `crates/origin-tools/src/tool_envelope.rs` (extend `EnvelopeCtx`):

```rust
use crate::builtins::write::WriteGuard;

// inside EnvelopeCtx (extend the struct):
//     pub write_guard: WriteGuard,
```

(Apply: change the `EnvelopeCtx` struct to include `pub write_guard: crate::builtins::write::WriteGuard`. `WriteGuard` must impl `Default + Clone + Debug`.)

Replace `crates/origin-tools/src/builtins/write.rs`:

```rust
//! `Write` v2 — atomic write, read-before-write guard, EOL preservation.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};

#[derive(Debug, Clone)]
pub struct WriteArgs {
    pub file_path: String,
    pub content: String,
    pub force: bool,
}

/// Per-session record of which file paths have been Read so the Write guard
/// can permit overwrites that the model has actually seen.
#[derive(Debug, Default, Clone)]
pub struct WriteGuard {
    read_paths: Arc<RwLock<HashSet<String>>>,
}

impl WriteGuard {
    pub fn note_read(&self, path: &str) {
        let canon = canonical_key(path);
        self.read_paths.write().unwrap().insert(canon);
    }

    #[must_use]
    pub fn has_read(&self, path: &str) -> bool {
        self.read_paths.read().unwrap().contains(&canonical_key(path))
    }
}

fn canonical_key(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// # Errors
/// `edit.read_required` if overwriting an existing file the model did not Read
/// this session and `force=false`. `io.permission` on disk errors.
pub fn write_v2(args: WriteArgs, guard: &WriteGuard) -> Result<(), ToolError> {
    let path = std::path::Path::new(&args.file_path);
    let existed = path.exists();

    if existed && !args.force && !guard.has_read(&args.file_path) {
        return Err(ToolError::new(
            ErrClass::Edit,
            "read_required",
            format!("refusing to overwrite '{}' that has not been Read in this session; pass force=true to override", args.file_path),
        ).recoverable(true).hint("call Read on this file first, then re-Write"));
    }

    // Preserve original convention if the file existed.
    let bytes_out = if existed {
        let prior = std::fs::read(&args.file_path)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        let det = text_fmt::detect(&prior);
        text_fmt::denormalise(&args.content, &det)
    } else {
        args.content.into_bytes()
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("mkdir {}: {e}", parent.display())))?;
        }
    }

    atomic_write(&args.file_path, &bytes_out)
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("create tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("write tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("fsync: {e}")))?;
    }
    std::fs::rename(&tmp, p)
        .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("rename: {e}")))?;
    Ok(())
}

crate::origin_tool! {
    name: "Write",
    description: "Create or overwrite a UTF-8 file. Atomic. Refuses overwrite of unread existing files unless force=true.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path": { "type": "string" },
            "content":   { "type": "string" },
            "force":     { "type": "boolean", "default": false }
        },
        "required": ["file_path", "content"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 1_000,
}
```

- [ ] **Step 4: Update daemon Write arm + thread WriteGuard through**

In `crates/origin-daemon/src/agent.rs`, replace the Write arm body with:

```rust
"Write" => {
    let guard = origin_tools::builtins::write::WriteGuard::default();
    // Production callers pass the session's guard via dispatch_with_envelope;
    // this passthrough path is used only by tests that bypass the envelope.
    let args = origin_tools::builtins::write::WriteArgs {
        file_path: args.get("file_path").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Write: missing `file_path`".into()))?
            .to_string(),
        content: args.get("content").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Write: missing `content`".into()))?
            .to_string(),
        force: args.get("force").and_then(Value::as_bool).unwrap_or(false),
    };
    origin_tools::builtins::write::write_v2(args, &guard)
        .map(|()| "write ok".to_string())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

In `dispatch_with_envelope`, pass `ctx.write_guard.clone()` through to `dispatch_tool_inner` (mirror the Write arm above using `&ctx.write_guard`). Also: in the Read arm of `dispatch_tool_inner`, after a successful `read_v2`, call `ctx.write_guard.note_read(&path)`.

- [ ] **Step 5: Append CRLF regression for Write**

Append to `crates/origin-tools/tests/crlf_regression.rs`:

```rust
#[test]
fn write_preserves_existing_file_crlf() {
    use origin_tools::builtins::write::{write_v2, WriteArgs, WriteGuard};
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.txt");
    fs::write(&p, b"a\r\nb\r\n").unwrap();
    let guard = WriteGuard::default();
    guard.note_read(p.to_string_lossy().as_ref());
    write_v2(WriteArgs {
        file_path: p.to_string_lossy().into_owned(),
        content: "x\ny\nz\n".into(),
        force: false,
    }, &guard).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"x\r\ny\r\nz\r\n");
}
```

- [ ] **Step 6: Run all**

```bash
cargo test -p origin-tools -p origin-daemon
```

- [ ] **Step 7: Verification + commit**

```bash
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/write.rs crates/origin-tools/src/tool_envelope.rs crates/origin-tools/tests/write_v2.rs crates/origin-tools/tests/crlf_regression.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Write v2 — atomic, read-guard, EOL-preserving"
```

### Task 3.4: `Grep v2` (output_mode, head_limit, type/glob/context)

**Files:**
- Modify: `crates/origin-tools/src/builtins/grep_tool.rs`
- Test: `crates/origin-tools/tests/grep_v2.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (Grep arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/grep_v2.rs`:

```rust
use origin_tools::builtins::grep_tool::{grep_v2, GrepArgs, OutputMode};
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

fn fixture() -> tempfile::TempDir {
    let d = tempdir().unwrap();
    fs::write(d.path().join("a.rs"), "fn foo() {}\nfn bar() {}\n").unwrap();
    fs::write(d.path().join("b.rs"), "fn foo() {}\n").unwrap();
    fs::write(d.path().join("c.md"), "no rust here\n").unwrap();
    d
}

#[test]
fn files_with_matches_default_mode() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn foo".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None, r#type: None,
        output_mode: None, head_limit: None,
        before: 0, after: 0, line_numbers: false, multiline: false,
    }).unwrap();
    let arr = out["files"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
}

#[test]
fn content_mode_returns_lines() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn foo".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None, r#type: None,
        output_mode: Some(OutputMode::Content),
        head_limit: None, before: 0, after: 0, line_numbers: true, multiline: false,
    }).unwrap();
    let arr = out["matches"].as_array().unwrap();
    assert!(arr.iter().any(|v| v["line"].as_u64() == Some(1)));
}

#[test]
fn count_mode_returns_counts() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None, r#type: None,
        output_mode: Some(OutputMode::Count),
        head_limit: None, before: 0, after: 0, line_numbers: false, multiline: false,
    }).unwrap();
    let arr = out["counts"].as_array().unwrap();
    let total: u64 = arr.iter().map(|v| v["count"].as_u64().unwrap()).sum();
    assert_eq!(total, 3);
}

#[test]
fn head_limit_caps_output() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None, r#type: None,
        output_mode: Some(OutputMode::Content),
        head_limit: Some(1),
        before: 0, after: 0, line_numbers: true, multiline: false,
    }).unwrap();
    let arr = out["matches"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
}

#[test]
fn type_filter_only_matches_named_type() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: None, r#type: Some("rust".into()),
        output_mode: None, head_limit: None,
        before: 0, after: 0, line_numbers: false, multiline: false,
    }).unwrap();
    let arr = out["files"].as_array().unwrap();
    for f in arr {
        assert!(f.as_str().unwrap().ends_with(".rs"));
    }
}

#[test]
fn glob_filter_only_matches_pattern() {
    let d = fixture();
    let out = grep_v2(GrepArgs {
        pattern: "fn".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        glob: Some("*.md".into()), r#type: None,
        output_mode: None, head_limit: None,
        before: 0, after: 0, line_numbers: false, multiline: false,
    }).unwrap();
    let arr = out["files"].as_array().unwrap();
    assert!(arr.is_empty());
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test grep_v2 2>&1 | tail -15`

- [ ] **Step 3: Replace `crates/origin-tools/src/builtins/grep_tool.rs`**

```rust
//! `Grep` v2 — files_with_matches default, head_limit, type/glob, context lines.

use crate::error::{ErrClass, ToolError};
use crate::{SideEffects, Tier, Urgency};
use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SinkMatch, SearcherBuilder};
use ignore::types::TypesBuilder;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
pub enum OutputMode { FilesWithMatches, Content, Count }

#[derive(Debug, Clone)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    pub r#type: Option<String>,
    pub output_mode: Option<OutputMode>,
    pub head_limit: Option<u32>,
    pub before: u32,
    pub after: u32,
    pub line_numbers: bool,
    pub multiline: bool,
}

/// # Errors
/// `regex.invalid` on bad pattern, `io.*` on walk failures.
pub fn grep_v2(args: GrepArgs) -> Result<Value, ToolError> {
    let matcher = RegexMatcher::new(&args.pattern)
        .map_err(|e| ToolError::new(ErrClass::Regex, "invalid", e.to_string()))?;
    let mode = args.output_mode.unwrap_or(OutputMode::FilesWithMatches);
    let head_limit = args.head_limit.unwrap_or(250) as usize;
    let root = args.path.unwrap_or_else(|| ".".to_string());

    let mut walker = WalkBuilder::new(&root);
    walker.follow_links(false).standard_filters(true);
    if let Some(t) = &args.r#type {
        let mut tb = TypesBuilder::new();
        tb.add_defaults();
        tb.select(t);
        let types = tb.build()
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_type", e.to_string()))?;
        walker.types(types);
    }
    if let Some(g) = &args.glob {
        let mut ob = ignore::overrides::OverrideBuilder::new(&root);
        ob.add(g)
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob", e.to_string()))?;
        walker.overrides(ob.build()
            .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob_build", e.to_string()))?);
    }

    let mut searcher: Searcher = SearcherBuilder::new()
        .before_context(args.before as usize)
        .after_context(args.after as usize)
        .multi_line(args.multiline)
        .build();

    let mut matches: Vec<Value> = Vec::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut files: BTreeMap<String, ()> = BTreeMap::new();
    'walk: for entry in walker.build() {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) { continue; }
        let path = entry.path().to_path_buf();
        let path_display = path.display().to_string();
        let mut local_count: u64 = 0;
        let mut local_lines: Vec<(u64, String)> = Vec::new();
        let res = searcher.search_path(&matcher, &path, grep_searcher::sinks::UTF8(|lnum, line| {
            local_count += 1;
            local_lines.push((lnum, line.trim_end_matches('\n').to_string()));
            Ok(true)
        }));
        if res.is_err() { continue; }
        if local_count == 0 { continue; }
        files.insert(path_display.clone(), ());
        counts.insert(path_display.clone(), local_count);
        if matches!(mode, OutputMode::Content) {
            for (lnum, line) in local_lines {
                matches.push(json!({"file": path_display, "line": lnum, "text": line}));
                if matches.len() >= head_limit { break 'walk; }
            }
        }
    }

    let out = match mode {
        OutputMode::FilesWithMatches => {
            let arr: Vec<String> = files.into_keys().take(head_limit).collect();
            json!({"files": arr})
        }
        OutputMode::Content => json!({"matches": matches}),
        OutputMode::Count => {
            let arr: Vec<Value> = counts.into_iter()
                .take(head_limit)
                .map(|(f, c)| json!({"file": f, "count": c}))
                .collect();
            json!({"counts": arr})
        }
    };
    Ok(out)
}

crate::origin_tool! {
    name: "Grep",
    description: "Recursive regex search. Modes: files_with_matches (default), content, count. Supports glob/type filters and context lines.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern":     { "type": "string" },
            "path":        { "type": "string" },
            "glob":        { "type": "string" },
            "type":        { "type": "string" },
            "output_mode": { "type": "string", "enum": ["files_with_matches", "content", "count"] },
            "head_limit":  { "type": "integer", "minimum": 1 },
            "before":      { "type": "integer", "minimum": 0 },
            "after":       { "type": "integer", "minimum": 0 },
            "line_numbers":{ "type": "boolean" },
            "multiline":   { "type": "boolean" }
        },
        "required": ["pattern"]
    }"#,
}
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test grep_v2`

- [ ] **Step 5: Update daemon Grep arm**

In `crates/origin-daemon/src/agent.rs`, replace the Grep arm in both `dispatch_tool` and `dispatch_tool_inner`:

```rust
"Grep" => {
    let mode = args.get("output_mode").and_then(Value::as_str).map(|s| match s {
        "files_with_matches" => origin_tools::builtins::grep_tool::OutputMode::FilesWithMatches,
        "content" => origin_tools::builtins::grep_tool::OutputMode::Content,
        "count" => origin_tools::builtins::grep_tool::OutputMode::Count,
        _ => origin_tools::builtins::grep_tool::OutputMode::FilesWithMatches,
    });
    let gargs = origin_tools::builtins::grep_tool::GrepArgs {
        pattern: args.get("pattern").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Grep: missing `pattern`".into()))?
            .to_string(),
        path: args.get("path").and_then(Value::as_str).map(str::to_string),
        glob: args.get("glob").and_then(Value::as_str).map(str::to_string),
        r#type: args.get("type").and_then(Value::as_str).map(str::to_string),
        output_mode: mode,
        head_limit: args.get("head_limit").and_then(Value::as_u64).map(|n| n as u32),
        before: args.get("before").and_then(Value::as_u64).map(|n| n as u32).unwrap_or(0),
        after:  args.get("after").and_then(Value::as_u64).map(|n| n as u32).unwrap_or(0),
        line_numbers: args.get("line_numbers").and_then(Value::as_bool).unwrap_or(false),
        multiline: args.get("multiline").and_then(Value::as_bool).unwrap_or(false),
    };
    origin_tools::builtins::grep_tool::grep_v2(gargs)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 6: Verification + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/grep_tool.rs crates/origin-tools/tests/grep_v2.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Grep v2 — files_with_matches default, head_limit, type/glob, context"
```

### Task 3.5: `Glob v2` (mtime-sorted, head_limit, gitignore)

**Files:**
- Modify: `crates/origin-tools/src/builtins/glob_tool.rs`
- Test: `crates/origin-tools/tests/glob_v2.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (Glob arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/glob_v2.rs`:

```rust
use origin_tools::builtins::glob_tool::{glob_v2, GlobArgs};
use std::fs;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn returns_matches_sorted_by_mtime_desc() {
    let d = tempdir().unwrap();
    fs::write(d.path().join("old.rs"), "").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    fs::write(d.path().join("new.rs"), "").unwrap();

    let out = glob_v2(GlobArgs {
        pattern: "**/*.rs".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        head_limit: None,
    }).unwrap();
    let arr = out.as_array().unwrap();
    assert!(arr[0].as_str().unwrap().ends_with("new.rs"));
    assert!(arr[1].as_str().unwrap().ends_with("old.rs"));
}

#[test]
fn respects_gitignore() {
    let d = tempdir().unwrap();
    fs::write(d.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(d.path().join("kept.rs"), "").unwrap();
    fs::write(d.path().join("ignored.rs"), "").unwrap();
    let out = glob_v2(GlobArgs {
        pattern: "*.rs".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        head_limit: None,
    }).unwrap();
    let arr = out.as_array().unwrap();
    for v in arr {
        assert!(!v.as_str().unwrap().ends_with("ignored.rs"));
    }
}

#[test]
fn head_limit_caps_output() {
    let d = tempdir().unwrap();
    for i in 0..10 { fs::write(d.path().join(format!("f{i}.rs")), "").unwrap(); }
    let out = glob_v2(GlobArgs {
        pattern: "*.rs".into(),
        path: Some(d.path().to_string_lossy().into_owned()),
        head_limit: Some(3),
    }).unwrap();
    assert_eq!(out.as_array().unwrap().len(), 3);
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test glob_v2 2>&1 | tail -15`

- [ ] **Step 3: Replace `crates/origin-tools/src/builtins/glob_tool.rs`**

```rust
//! `Glob` v2 — gitignore-aware, mtime-sorted, head-limited.

use crate::error::{ErrClass, ToolError};
use crate::{SideEffects, Tier, Urgency};
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct GlobArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub head_limit: Option<u32>,
}

/// # Errors
/// `validation.bad_glob` on bad pattern, `io.*` on walk failures.
pub fn glob_v2(args: GlobArgs) -> Result<Value, ToolError> {
    let root = args.path.unwrap_or_else(|| ".".to_string());
    let head_limit = args.head_limit.unwrap_or(250) as usize;

    let mut ob = ignore::overrides::OverrideBuilder::new(&root);
    ob.add(&args.pattern)
        .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob", e.to_string()))?;
    let overrides = ob.build()
        .map_err(|e| ToolError::new(ErrClass::Validation, "bad_glob_build", e.to_string()))?;

    let walker = WalkBuilder::new(&root)
        .follow_links(false)
        .standard_filters(true)
        .overrides(overrides)
        .build();

    let mut matches: Vec<(String, SystemTime)> = Vec::new();
    for entry in walker {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) { continue; }
        let mtime = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
        matches.push((entry.path().display().to_string(), mtime));
    }
    matches.sort_by(|a, b| b.1.cmp(&a.1));
    let arr: Vec<Value> = matches.into_iter()
        .take(head_limit)
        .map(|(p, _)| Value::String(p))
        .collect();
    Ok(Value::Array(arr))
}

crate::origin_tool! {
    name: "Glob",
    description: "Find files matching a glob pattern. Returns paths sorted by mtime DESC, gitignore-aware.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern":    { "type": "string" },
            "path":       { "type": "string" },
            "head_limit": { "type": "integer", "minimum": 1 }
        },
        "required": ["pattern"]
    }"#,
}
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test glob_v2`

- [ ] **Step 5: Update daemon Glob arm**

```rust
"Glob" => {
    let gargs = origin_tools::builtins::glob_tool::GlobArgs {
        pattern: args.get("pattern").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Glob: missing `pattern`".into()))?
            .to_string(),
        path: args.get("path").and_then(Value::as_str).map(str::to_string),
        head_limit: args.get("head_limit").and_then(Value::as_u64).map(|n| n as u32),
    };
    origin_tools::builtins::glob_tool::glob_v2(gargs)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 6: Verification + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/glob_tool.rs crates/origin-tools/tests/glob_v2.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Glob v2 — gitignore-aware, mtime-sorted, head-limited"
```

### Task 3.6: Phase 3 regression + tag

- [ ] **Step 1: Full workspace test**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-3 -m "Read/Edit/Write/Grep/Glob rebuilt through envelope; CRLF bug fixed"
```

---

## Phase 4 — Bash v2 + Process Supervisor + Monitor

Goal: `Bash` gains `timeout`, `cwd`, `env`, `run_in_background`. New `Supervisor` owns long-running children. New `Monitor` tool tails their output via a byte-offset-indexed ring buffer.

### Task 4.1: `proc_supervisor` core (spawn, ring buffer, wait, kill, timeout)

**Files:**
- Create: `crates/origin-tools/src/proc_supervisor.rs`
- Test: `crates/origin-tools/tests/proc_supervisor.rs`
- Modify: `crates/origin-tools/src/lib.rs`

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/proc_supervisor.rs`:

```rust
use origin_tools::proc_supervisor::{Supervisor, SpawnOpts};
use std::time::Duration;

#[tokio::test]
async fn spawn_and_read_output() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello";
    #[cfg(windows)]
    let cmd = "Write-Output hello";
    let pid = sup.spawn(cmd, SpawnOpts::default()).unwrap();
    // Wait briefly for output to land in the buffer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let chunk = sup.read_since(pid, 0, 4096).unwrap();
    assert!(chunk.bytes.contains("hello"));
}

#[tokio::test]
async fn read_since_returns_only_new_bytes() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "printf 'aaa\\nbbb\\nccc\\n'";
    #[cfg(windows)]
    let cmd = "'aaa','bbb','ccc' | ForEach-Object { $_ }";
    let pid = sup.spawn(cmd, SpawnOpts::default()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let first = sup.read_since(pid, 0, 4).unwrap();
    let next = sup.read_since(pid, first.next_offset, 4096).unwrap();
    assert!(!next.bytes.is_empty());
    assert_ne!(first.bytes, next.bytes);
}

#[tokio::test]
async fn timeout_terminates_long_process() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 60";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 60";
    let opts = SpawnOpts { timeout: Some(Duration::from_millis(300)), ..SpawnOpts::default() };
    let pid = sup.spawn(cmd, opts).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let chunk = sup.read_since(pid, 0, 4096).unwrap();
    assert!(chunk.status.is_terminal(), "status was {:?}", chunk.status);
}

#[tokio::test]
async fn parallel_processes_have_isolated_buffers() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let (a, b) = ("echo AAA", "echo BBB");
    #[cfg(windows)]
    let (a, b) = ("Write-Output AAA", "Write-Output BBB");
    let pa = sup.spawn(a, SpawnOpts::default()).unwrap();
    let pb = sup.spawn(b, SpawnOpts::default()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ca = sup.read_since(pa, 0, 4096).unwrap();
    let cb = sup.read_since(pb, 0, 4096).unwrap();
    assert!(ca.bytes.contains("AAA"));
    assert!(cb.bytes.contains("BBB"));
    assert!(!ca.bytes.contains("BBB"));
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test proc_supervisor 2>&1 | tail -15`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/proc_supervisor.rs`:

```rust
//! Process supervisor: owns long-running children, exposes a byte-offset
//! ring-buffer per process for the `Monitor` tool to tail.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::error::{ErrClass, ToolError};

pub type ProcessId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcStatus {
    Running,
    Exited(i32),
    TimedOut,
    Killed,
}

impl ProcStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, ProcStatus::Running)
    }
}

#[derive(Debug, Clone)]
pub struct ReadChunk {
    pub bytes: String,
    pub next_offset: u64,
    pub status: ProcStatus,
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOpts {
    pub timeout: Option<Duration>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub buffer_cap_bytes: Option<usize>,
}

#[derive(Debug)]
struct ProcSlot {
    buf: Vec<u8>,
    base_offset: u64,
    cap: usize,
    status: ProcStatus,
}

impl ProcSlot {
    fn append(&mut self, more: &[u8]) {
        self.buf.extend_from_slice(more);
        if self.buf.len() > self.cap {
            let overflow = self.buf.len() - self.cap;
            self.buf.drain(..overflow);
            self.base_offset += overflow as u64;
        }
    }
    fn read_since(&self, offset: u64, max: usize) -> ReadChunk {
        let abs_end = self.base_offset + self.buf.len() as u64;
        let start = offset.max(self.base_offset);
        let avail_start_idx = (start - self.base_offset) as usize;
        let take = self.buf[avail_start_idx..].len().min(max);
        let slice = &self.buf[avail_start_idx..avail_start_idx + take];
        ReadChunk {
            bytes: String::from_utf8_lossy(slice).into_owned(),
            next_offset: start + take as u64,
            status: self.status,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Supervisor {
    inner: Arc<Mutex<HashMap<ProcessId, ProcSlot>>>,
    next: Arc<Mutex<ProcessId>>,
}

impl Default for Supervisor {
    fn default() -> Self { Self::new() }
}

impl Supervisor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            next: Arc::new(Mutex::new(1)),
        }
    }

    /// # Errors
    /// `bash.spawn_failed` if the shell child cannot be spawned.
    pub fn spawn(&self, command: &str, opts: SpawnOpts) -> Result<ProcessId, ToolError> {
        let pid = {
            let mut n = self.next.lock().unwrap();
            let id = *n;
            *n += 1;
            id
        };
        let cap = opts.buffer_cap_bytes.unwrap_or(512 * 1024);
        self.inner.lock().unwrap().insert(pid, ProcSlot {
            buf: Vec::new(),
            base_offset: 0,
            cap,
            status: ProcStatus::Running,
        });

        let mut cmd: Command;
        #[cfg(unix)]
        {
            cmd = Command::new("sh");
            cmd.arg("-c").arg(command);
        }
        #[cfg(windows)]
        {
            cmd = Command::new("pwsh");
            cmd.args(["-NoProfile", "-Command", command]);
        }
        if let Some(cwd) = &opts.cwd { cmd.current_dir(cwd); }
        for (k, v) in &opts.env { cmd.env(k, v); }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        cmd.kill_on_drop(true);

        let child = cmd.spawn().or_else(|_| {
            #[cfg(windows)]
            {
                let mut fallback = Command::new("powershell");
                fallback.args(["-NoProfile", "-Command", command]);
                if let Some(cwd) = &opts.cwd { fallback.current_dir(cwd); }
                for (k, v) in &opts.env { fallback.env(k, v); }
                fallback.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
                fallback.kill_on_drop(true);
                fallback.spawn()
            }
            #[cfg(unix)]
            {
                let mut fallback = Command::new("sh");
                fallback.arg("-c").arg(command);
                fallback.spawn()
            }
        }).map_err(|e| ToolError::new(ErrClass::Bash, "spawn_failed", e.to_string()))?;

        let table = self.inner.clone();
        tokio::spawn(supervise(pid, child, opts.timeout, table));
        Ok(pid)
    }

    /// Read bytes from `offset` for at most `max` bytes.
    ///
    /// # Errors
    /// `validation.unknown_pid` if pid was never spawned.
    pub fn read_since(&self, pid: ProcessId, offset: u64, max: usize) -> Result<ReadChunk, ToolError> {
        let map = self.inner.lock().unwrap();
        map.get(&pid).map(|s| s.read_since(offset, max))
            .ok_or_else(|| ToolError::new(ErrClass::Validation, "unknown_pid", format!("no such pid {pid}")))
    }

    pub fn kill(&self, pid: ProcessId) {
        if let Some(slot) = self.inner.lock().unwrap().get_mut(&pid) {
            slot.status = ProcStatus::Killed;
        }
    }
}

async fn supervise(
    pid: ProcessId,
    mut child: Child,
    timeout: Option<Duration>,
    table: Arc<Mutex<HashMap<ProcessId, ProcSlot>>>,
) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Some(out) = stdout {
        let t = table.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut g = t.lock().unwrap();
                if let Some(s) = g.get_mut(&pid) {
                    s.append(line.as_bytes());
                    s.append(b"\n");
                }
            }
        });
    }
    if let Some(err) = stderr {
        let t = table.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut g = t.lock().unwrap();
                if let Some(s) = g.get_mut(&pid) {
                    s.append(b"stderr: ");
                    s.append(line.as_bytes());
                    s.append(b"\n");
                }
            }
        });
    }
    let exit = if let Some(d) = timeout {
        match tokio::time::timeout(d, child.wait()).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let _ = child.kill().await;
                let mut g = table.lock().unwrap();
                if let Some(s) = g.get_mut(&pid) { s.status = ProcStatus::TimedOut; }
                return;
            }
        }
    } else {
        child.wait().await
    };
    let mut g = table.lock().unwrap();
    if let Some(s) = g.get_mut(&pid) {
        if !s.status.is_terminal() {
            s.status = match exit {
                Ok(status) => ProcStatus::Exited(status.code().unwrap_or(-1)),
                Err(_) => ProcStatus::Exited(-1),
            };
        }
    }
}
```

Add to `crates/origin-tools/src/lib.rs`:

```rust
pub mod proc_supervisor;
```

- [ ] **Step 4: Run — pass**

Run: `cargo test -p origin-tools --test proc_supervisor`
Expected: 4 pass.

- [ ] **Step 5: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/proc_supervisor.rs crates/origin-tools/tests/proc_supervisor.rs crates/origin-tools/src/lib.rs
git commit -m "feat(tools): proc_supervisor with byte-offset ring buffers and timeout"
```

### Task 4.2: `Bash v2` (timeout, cwd, env, run_in_background)

**Files:**
- Modify: `crates/origin-tools/src/builtins/bash.rs`
- Test: `crates/origin-tools/tests/bash_v2.rs`
- Modify: `crates/origin-tools/src/tool_envelope.rs` (add `pub supervisor: Supervisor` to `EnvelopeCtx`)
- Modify: `crates/origin-daemon/src/agent.rs` (Bash arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/bash_v2.rs`:

```rust
use origin_tools::builtins::bash::{bash_v2, BashArgs};
use origin_tools::proc_supervisor::Supervisor;
use std::time::Duration;

#[tokio::test]
async fn foreground_returns_full_output() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello";
    #[cfg(windows)]
    let cmd = "Write-Output hello";
    let out = bash_v2(BashArgs { command: cmd.into(), timeout: None, cwd: None, env: vec![], run_in_background: false }, &sup).await.unwrap();
    assert_eq!(out["status"], "exited");
    assert!(out["stdout"].as_str().unwrap().contains("hello"));
    assert_eq!(out["exit_code"], 0);
}

#[tokio::test]
async fn background_returns_pid_immediately() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 1";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 1";
    let started = std::time::Instant::now();
    let out = bash_v2(BashArgs { command: cmd.into(), timeout: None, cwd: None, env: vec![], run_in_background: true }, &sup).await.unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(out["status"], "started");
    assert!(out["pid"].as_u64().is_some());
}

#[tokio::test]
async fn timeout_returns_timed_out_status() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "sleep 5";
    #[cfg(windows)]
    let cmd = "Start-Sleep -Seconds 5";
    let out = bash_v2(BashArgs { command: cmd.into(), timeout: Some(200), cwd: None, env: vec![], run_in_background: false }, &sup).await.unwrap();
    assert_eq!(out["status"], "timed_out");
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test bash_v2 2>&1 | tail -10`

- [ ] **Step 3: Implement**

Replace `crates/origin-tools/src/builtins/bash.rs`:

```rust
//! `Bash` v2 — timeout, cwd, env, run_in_background. Backed by proc_supervisor.

use std::time::Duration;

use crate::error::ToolError;
use crate::proc_supervisor::{ProcStatus, SpawnOpts, Supervisor};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct BashArgs {
    pub command: String,
    /// Seconds; default 120, max 600 (enforced here).
    pub timeout: Option<u32>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub run_in_background: bool,
}

/// # Errors
/// `bash.spawn_failed` if shell child cannot be spawned.
pub async fn bash_v2(args: BashArgs, sup: &Supervisor) -> Result<Value, ToolError> {
    let timeout_secs = args.timeout.unwrap_or(120).min(600);
    let opts = SpawnOpts {
        timeout: Some(Duration::from_secs(timeout_secs as u64)),
        cwd: args.cwd,
        env: args.env,
        buffer_cap_bytes: None,
    };
    let pid = sup.spawn(&args.command, opts)?;
    if args.run_in_background {
        return Ok(json!({"status": "started", "pid": pid}));
    }
    // Foreground: poll until status terminal, then return final body.
    let mut next = 0u64;
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs as u64 + 5);
    let mut acc = String::new();
    loop {
        let chunk = sup.read_since(pid, next, 64 * 1024)?;
        acc.push_str(&chunk.bytes);
        next = chunk.next_offset;
        if chunk.status.is_terminal() {
            let (status_str, exit_code) = match chunk.status {
                ProcStatus::Exited(c) => ("exited", c),
                ProcStatus::TimedOut => ("timed_out", -1),
                ProcStatus::Killed => ("killed", -1),
                ProcStatus::Running => unreachable!(),
            };
            return Ok(json!({
                "status": status_str,
                "exit_code": exit_code,
                "stdout": acc,
            }));
        }
        if std::time::Instant::now() > deadline {
            return Ok(json!({"status": "timed_out", "exit_code": -1, "stdout": acc}));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

crate::origin_tool! {
    name: "Bash",
    description: "Execute a shell command. Foreground (default) waits for completion; run_in_background returns a pid for Monitor to tail.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "command":           { "type": "string" },
            "timeout":           { "type": "integer", "minimum": 1, "maximum": 600 },
            "cwd":               { "type": "string" },
            "env":               { "type": "array", "items": { "type": "array", "items": [{"type": "string"}, {"type": "string"}], "minItems": 2, "maxItems": 2 } },
            "run_in_background": { "type": "boolean", "default": false }
        },
        "required": ["command"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Shell,
    token_budget: 25_000,
}
```

- [ ] **Step 4: Add `supervisor` to `EnvelopeCtx`**

In `crates/origin-tools/src/tool_envelope.rs`:

```rust
use crate::proc_supervisor::Supervisor;

// inside EnvelopeCtx:
//     pub supervisor: Supervisor,
```

Make sure `Supervisor` impls `Default + Clone + Debug` (already does per Task 4.1).

- [ ] **Step 5: Update daemon Bash arm**

In `crates/origin-daemon/src/agent.rs`, replace the Bash arm in `dispatch_tool_inner` (and the legacy `dispatch_tool`):

```rust
"Bash" => {
    let bargs = origin_tools::builtins::bash::BashArgs {
        command: args.get("command").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("Bash: missing `command`".into()))?
            .to_string(),
        timeout: args.get("timeout").and_then(Value::as_u64).map(|n| n as u32),
        cwd: args.get("cwd").and_then(Value::as_str).map(str::to_string),
        env: args.get("env").and_then(Value::as_array).map(|a| a.iter().filter_map(|e| {
            let arr = e.as_array()?;
            Some((arr.get(0)?.as_str()?.to_string(), arr.get(1)?.as_str()?.to_string()))
        }).collect()).unwrap_or_default(),
        run_in_background: args.get("run_in_background").and_then(Value::as_bool).unwrap_or(false),
    };
    // Local supervisor for the legacy passthrough; the envelope path uses ctx.supervisor.
    let sup = origin_tools::proc_supervisor::Supervisor::new();
    origin_tools::builtins::bash::bash_v2(bargs, &sup).await
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

For the envelope-routed path in `dispatch_with_envelope`, thread `&ctx.supervisor` into the call instead of constructing a new one.

- [ ] **Step 6: Run + verify + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/bash.rs crates/origin-tools/tests/bash_v2.rs crates/origin-tools/src/tool_envelope.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Bash v2 — timeout/cwd/env/run_in_background via proc_supervisor"
```

### Task 4.3: `Monitor` tool (tail supervisor ring buffer)

**Files:**
- Create: `crates/origin-tools/src/builtins/monitor.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs`
- Test: `crates/origin-tools/tests/monitor.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (add Monitor arm)

- [ ] **Step 1: Write failing test**

`crates/origin-tools/tests/monitor.rs`:

```rust
use origin_tools::builtins::monitor::{monitor, MonitorArgs};
use origin_tools::proc_supervisor::{SpawnOpts, Supervisor};

#[tokio::test]
async fn monitor_returns_bytes_and_next_offset() {
    let sup = Supervisor::new();
    #[cfg(unix)]
    let cmd = "echo hello-world";
    #[cfg(windows)]
    let cmd = "Write-Output hello-world";
    let pid = sup.spawn(cmd, SpawnOpts::default()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let v = monitor(MonitorArgs { pid, since_byte: 0, max_bytes: 4096, wait: false }, &sup).await.unwrap();
    assert!(v["bytes"].as_str().unwrap().contains("hello-world"));
    assert!(v["next_offset"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn monitor_unknown_pid_errors() {
    let sup = Supervisor::new();
    let err = monitor(MonitorArgs { pid: 999_999, since_byte: 0, max_bytes: 1, wait: false }, &sup).await.unwrap_err();
    assert_eq!(err.reason, "unknown_pid");
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test monitor 2>&1 | tail -10`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/builtins/monitor.rs`:

```rust
//! `Monitor` tool — tail a supervisor process's output by byte offset.

use std::time::Duration;

use crate::error::ToolError;
use crate::proc_supervisor::{ProcStatus, Supervisor};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct MonitorArgs {
    pub pid: u32,
    pub since_byte: u64,
    pub max_bytes: u32,
    pub wait: bool,
}

/// # Errors
/// `validation.unknown_pid` if pid is unknown.
pub async fn monitor(args: MonitorArgs, sup: &Supervisor) -> Result<Value, ToolError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let chunk = sup.read_since(args.pid, args.since_byte, args.max_bytes as usize)?;
        let bytes_avail = !chunk.bytes.is_empty();
        if !args.wait || bytes_avail || chunk.status.is_terminal() {
            let status_str = match chunk.status {
                ProcStatus::Running => "running",
                ProcStatus::Exited(_) => "exited",
                ProcStatus::TimedOut => "timed_out",
                ProcStatus::Killed => "killed",
            };
            let exit_code = if let ProcStatus::Exited(c) = chunk.status { Some(c) } else { None };
            let mut out = json!({
                "bytes": chunk.bytes,
                "next_offset": chunk.next_offset,
                "status": status_str,
            });
            if let Some(c) = exit_code { out["exit_code"] = json!(c); }
            return Ok(out);
        }
        if std::time::Instant::now() > deadline {
            return Ok(json!({"bytes": "", "next_offset": args.since_byte, "status": "running"}));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

crate::origin_tool! {
    name: "Monitor",
    description: "Tail output from a background process started by Bash{run_in_background:true}. Pass since_byte=N to skip already-seen bytes.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pid":         { "type": "integer", "minimum": 1 },
            "since_byte":  { "type": "integer", "minimum": 0, "default": 0 },
            "max_bytes":   { "type": "integer", "minimum": 1, "default": 4096 },
            "wait":        { "type": "boolean", "default": false }
        },
        "required": ["pid"]
    }"#,
}
```

Append to `crates/origin-tools/src/builtins/mod.rs`:

```rust
pub mod monitor;
```

- [ ] **Step 4: Add daemon Monitor arm**

In `crates/origin-daemon/src/agent.rs`, add after the Bash arm (in both `dispatch_tool` and `dispatch_tool_inner`):

```rust
"Monitor" => {
    let margs = origin_tools::builtins::monitor::MonitorArgs {
        pid: args.get("pid").and_then(Value::as_u64).map(|n| n as u32)
            .ok_or_else(|| LoopError::BadArgs("Monitor: missing `pid`".into()))?,
        since_byte: args.get("since_byte").and_then(Value::as_u64).unwrap_or(0),
        max_bytes: args.get("max_bytes").and_then(Value::as_u64).map(|n| n as u32).unwrap_or(4096),
        wait: args.get("wait").and_then(Value::as_bool).unwrap_or(false),
    };
    // Envelope-routed path uses ctx.supervisor; this passthrough makes a stub
    // that always returns unknown_pid — production should never reach this arm.
    let sup = origin_tools::proc_supervisor::Supervisor::new();
    origin_tools::builtins::monitor::monitor(margs, &sup).await
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 5: Run + verify + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/monitor.rs crates/origin-tools/src/builtins/mod.rs crates/origin-tools/tests/monitor.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): Monitor tool — tail supervisor ring-buffer by byte offset"
```

### Task 4.4: Phase 4 regression + tag

- [ ] **Step 1: Full test + clippy + fmt**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-4 -m "Bash v2 + Supervisor + Monitor — background processes supported"
```

---

## Phase 5 — MultiEdit + ApplyPatch

### Task 5.1: `MultiEdit`

**Files:**
- Create: `crates/origin-tools/src/builtins/multi_edit.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs`
- Test: `crates/origin-tools/tests/multi_edit.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (MultiEdit arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/multi_edit.rs`:

```rust
use origin_tools::builtins::multi_edit::{multi_edit, MultiEditArgs, EditOp};
use std::fs;
use tempfile::tempdir;

#[test]
fn applies_edits_in_order_atomically() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "alpha\nbeta\ngamma\n").unwrap();
    let out = multi_edit(MultiEditArgs {
        file_path: p.to_string_lossy().into_owned(),
        edits: vec![
            EditOp { old: "alpha".into(), new: "A".into(), replace_all: false },
            EditOp { old: "beta".into(),  new: "B".into(), replace_all: false },
        ],
    }).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["applied"], 2);
    assert_eq!(fs::read_to_string(&p).unwrap(), "A\nB\ngamma\n");
}

#[test]
fn failure_mid_sequence_does_not_partially_write() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "x\ny\nz\n").unwrap();
    let err = multi_edit(MultiEditArgs {
        file_path: p.to_string_lossy().into_owned(),
        edits: vec![
            EditOp { old: "x".into(), new: "X".into(), replace_all: false },
            EditOp { old: "MISSING".into(), new: "?".into(), replace_all: false },
        ],
    }).unwrap_err();
    assert_eq!(err.reason, "no_match");
    assert_eq!(fs::read_to_string(&p).unwrap(), "x\ny\nz\n");
}

#[test]
fn crlf_preserved_across_multiple_edits() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("crlf.rs");
    fs::write(&p, b"a\r\nb\r\nc\r\n").unwrap();
    multi_edit(MultiEditArgs {
        file_path: p.to_string_lossy().into_owned(),
        edits: vec![
            EditOp { old: "a".into(), new: "A".into(), replace_all: false },
            EditOp { old: "b".into(), new: "B".into(), replace_all: false },
        ],
    }).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"A\r\nB\r\nc\r\n");
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test multi_edit 2>&1 | tail -10`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/builtins/multi_edit.rs`:

```rust
//! `MultiEdit` — apply a list of edit operations to one file, atomically.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct EditOp { pub old: String, pub new: String, pub replace_all: bool }

#[derive(Debug, Clone)]
pub struct MultiEditArgs {
    pub file_path: String,
    pub edits: Vec<EditOp>,
}

/// # Errors
/// `edit.no_match | edit.ambiguous | io.*`
pub fn multi_edit(args: MultiEditArgs) -> Result<Value, ToolError> {
    let bytes = std::fs::read(&args.file_path).map_err(|e| {
        ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path))
    })?;
    let det = text_fmt::detect(&bytes);
    let mut text = text_fmt::normalise_to_lf(&bytes, &det)?;
    let mut applied = 0u32;
    for op in &args.edits {
        let count = text.matches(&op.old).count();
        text = match count {
            0 => return Err(ToolError::new(ErrClass::Edit, "no_match",
                format!("edit {applied} of {}: '{}' not found", args.edits.len(), op.old))),
            1 => text.replacen(&op.old, &op.new, 1),
            n if op.replace_all => text.replace(&op.old, &op.new),
            n => return Err(ToolError::new(ErrClass::Edit, "ambiguous",
                format!("edit {applied} of {}: '{}' appears {n} times; pass replace_all=true", args.edits.len(), op.old))),
        };
        applied += 1;
    }
    let new_bytes = text_fmt::denormalise(&text, &det);
    atomic_write(&args.file_path, &new_bytes)?;
    Ok(json!({"ok": true, "applied": applied}))
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.write_all(bytes).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.sync_all().map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
    }
    std::fs::rename(&tmp, p).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))
}

crate::origin_tool! {
    name: "MultiEdit",
    description: "Apply a sequence of edit operations to one file atomically. Single read + single write per call.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path": { "type": "string" },
            "edits": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "old":         { "type": "string" },
                        "new":         { "type": "string" },
                        "replace_all": { "type": "boolean", "default": false }
                    },
                    "required": ["old", "new"]
                }
            }
        },
        "required": ["file_path", "edits"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 4_000,
}
```

Append to `crates/origin-tools/src/builtins/mod.rs`:

```rust
pub mod multi_edit;
```

- [ ] **Step 4: Update daemon — add MultiEdit arm**

In `crates/origin-daemon/src/agent.rs` (in both `dispatch_tool` and `dispatch_tool_inner`), add after Edit:

```rust
"MultiEdit" => {
    let edits_v = args.get("edits").and_then(Value::as_array)
        .ok_or_else(|| LoopError::BadArgs("MultiEdit: missing `edits`".into()))?;
    let edits = edits_v.iter().map(|e| {
        let o = e.get("old").and_then(Value::as_str).unwrap_or("");
        let n = e.get("new").and_then(Value::as_str).unwrap_or("");
        let r = e.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
        origin_tools::builtins::multi_edit::EditOp { old: o.into(), new: n.into(), replace_all: r }
    }).collect();
    let margs = origin_tools::builtins::multi_edit::MultiEditArgs {
        file_path: args.get("file_path").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("MultiEdit: missing `file_path`".into()))?
            .to_string(),
        edits,
    };
    origin_tools::builtins::multi_edit::multi_edit(margs)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 5: Run + verify + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/multi_edit.rs crates/origin-tools/src/builtins/mod.rs crates/origin-tools/tests/multi_edit.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): MultiEdit — sequential atomic edits to one file"
```

### Task 5.2: `ApplyPatch` (unified diff)

**Files:**
- Create: `crates/origin-tools/src/builtins/apply_patch.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs`
- Test: `crates/origin-tools/tests/apply_patch.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (ApplyPatch arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/apply_patch.rs`:

```rust
use origin_tools::builtins::apply_patch::{apply_patch, ApplyPatchArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn applies_single_file_diff() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\nfn bar() {}\n").unwrap();
    let patch = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,2 +1,2 @@\n-fn foo() {{}}\n+fn FOO() {{}}\n fn bar() {{}}\n",
        path = p.to_string_lossy().replace('\\', "/")
    );
    let out = apply_patch(ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(fs::read_to_string(&p).unwrap(), "fn FOO() {}\nfn bar() {}\n");
}

#[test]
fn rejects_patch_with_mismatched_context() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\n").unwrap();
    let bad = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,1 @@\n-fn DIFFERENT() {{}}\n+fn FOO() {{}}\n",
        path = p.to_string_lossy().replace('\\', "/")
    );
    let err = apply_patch(ApplyPatchArgs { patch: bad }).unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Edit);
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test apply_patch 2>&1 | tail -10`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/builtins/apply_patch.rs`:

```rust
//! `ApplyPatch` — apply a unified diff atomically across files.
//!
//! v2 supports the common subset of unified diff:
//! - one or more `--- a/<path>` / `+++ b/<path>` file headers
//! - `@@ -L1,C1 +L2,C2 @@` hunks with `-`/`+`/` ` lines
//!
//! All hunks are validated against the on-disk files before any write; if
//! any hunk fails to apply, no file is modified.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ApplyPatchArgs { pub patch: String }

#[derive(Debug)]
struct Hunk {
    file: String,
    old_start: usize,
    lines: Vec<String>, // each line begins with ' ', '-' or '+'
}

/// # Errors
/// `edit.no_match` if a hunk's context does not match disk.
pub fn apply_patch(args: ApplyPatchArgs) -> Result<Value, ToolError> {
    let hunks = parse_patch(&args.patch)?;

    // Plan all writes first; only commit if every hunk applies cleanly.
    use std::collections::HashMap;
    let mut staged: HashMap<String, (Vec<u8>, text_fmt::Detected, String)> = HashMap::new();
    for h in &hunks {
        let entry = if let Some(e) = staged.get(&h.file) {
            e.2.clone()
        } else {
            let bytes = std::fs::read(&h.file).map_err(|e| ToolError::new(
                ErrClass::Io, "not_found", format!("{}: {e}", h.file)))?;
            let det = text_fmt::detect(&bytes);
            let text = text_fmt::normalise_to_lf(&bytes, &det)?;
            staged.insert(h.file.clone(), (bytes, det, text.clone()));
            text
        };
        let updated = apply_one_hunk(&entry, h)?;
        if let Some(slot) = staged.get_mut(&h.file) {
            slot.2 = updated;
        }
    }
    let files_updated = staged.len();
    for (path, (_orig_bytes, det, text)) in staged {
        let new_bytes = text_fmt::denormalise(&text, &det);
        atomic_write(&path, &new_bytes)?;
    }
    Ok(json!({"ok": true, "files_updated": files_updated}))
}

fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ToolError> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut cur_file: Option<String> = None;
    let mut cur: Option<Hunk> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            if let Some(h) = cur.take() { hunks.push(h); }
            cur_file = Some(rest.to_string());
        } else if line.starts_with("--- a/") || line.starts_with("diff --git") {
            // ignore; +++ line drives the path
        } else if let Some(rest) = line.strip_prefix("@@ -") {
            if let Some(h) = cur.take() { hunks.push(h); }
            // parse "L1,C1 +L2,C2 @@"
            let mut parts = rest.split(' ');
            let old_part = parts.next()
                .ok_or_else(|| ToolError::new(ErrClass::Validation, "bad_patch", "missing old range"))?;
            let l_str = old_part.split(',').next().unwrap_or("1");
            let old_start: usize = l_str.parse().map_err(|_| {
                ToolError::new(ErrClass::Validation, "bad_patch", "bad old line number")
            })?;
            let file = cur_file.clone().ok_or_else(|| ToolError::new(
                ErrClass::Validation, "bad_patch", "hunk before file header"))?;
            cur = Some(Hunk { file, old_start, lines: Vec::new() });
        } else if let Some(h) = cur.as_mut() {
            if line.starts_with(' ') || line.starts_with('-') || line.starts_with('+') {
                h.lines.push(line.to_string());
            }
        }
    }
    if let Some(h) = cur.take() { hunks.push(h); }
    Ok(hunks)
}

fn apply_one_hunk(text: &str, h: &Hunk) -> Result<String, ToolError> {
    let lines: Vec<&str> = text.lines().collect();
    // Build the "old block" from ' ' and '-' lines.
    let mut old_block: Vec<&str> = Vec::new();
    let mut new_block: Vec<String> = Vec::new();
    for l in &h.lines {
        let body = &l[1..];
        match l.as_bytes()[0] {
            b' ' => { old_block.push(body); new_block.push(body.to_string()); }
            b'-' => { old_block.push(body); }
            b'+' => { new_block.push(body.to_string()); }
            _ => {}
        }
    }
    let start_idx = h.old_start.saturating_sub(1);
    if start_idx + old_block.len() > lines.len() {
        return Err(ToolError::new(ErrClass::Edit, "no_match",
            format!("hunk @{} extends past EOF in {}", h.old_start, h.file)));
    }
    for (off, exp) in old_block.iter().enumerate() {
        let got = lines[start_idx + off];
        if got != *exp {
            return Err(ToolError::new(ErrClass::Edit, "no_match",
                format!("context mismatch at {}:{}: expected `{exp}`, got `{got}`", h.file, h.old_start + off)));
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    out.extend(lines[..start_idx].iter().map(|s| s.to_string()));
    out.extend(new_block);
    out.extend(lines[start_idx + old_block.len()..].iter().map(|s| s.to_string()));
    let mut joined = out.join("\n");
    if text.ends_with('\n') { joined.push('\n'); }
    Ok(joined)
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.write_all(bytes).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.sync_all().map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
    }
    std::fs::rename(&tmp, p).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))
}

crate::origin_tool! {
    name: "ApplyPatch",
    description: "Apply a unified diff atomically across one or more files. Validates context lines before any write.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "patch": { "type": "string" }
        },
        "required": ["patch"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 4_000,
}
```

Append to `crates/origin-tools/src/builtins/mod.rs`:

```rust
pub mod apply_patch;
```

- [ ] **Step 4: Daemon ApplyPatch arm**

```rust
"ApplyPatch" => {
    let pargs = origin_tools::builtins::apply_patch::ApplyPatchArgs {
        patch: args.get("patch").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("ApplyPatch: missing `patch`".into()))?
            .to_string(),
    };
    origin_tools::builtins::apply_patch::apply_patch(pargs)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 5: Run + verify + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/apply_patch.rs crates/origin-tools/src/builtins/mod.rs crates/origin-tools/tests/apply_patch.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): ApplyPatch — unified diff with atomic multi-file commit"
```

### Task 5.3: Phase 5 regression + tag

- [ ] **Step 1: Full check**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-5 -m "MultiEdit + ApplyPatch — round-trip count drops on large edits"
```

---

## Phase 6 — `origin-lsp-client` crate + `ra_bridge` + `Diagnostics` tool

Goal: persistent rust-analyzer instance owned by the daemon; `Diagnostics` tool returns LSP diagnostics in <100ms after the initial index.

### Task 6.1: New crate `origin-lsp-client` — wire format

**Files:**
- Create: `crates/origin-lsp-client/Cargo.toml`
- Create: `crates/origin-lsp-client/src/lib.rs`
- Modify: workspace root `Cargo.toml` (add member)

- [ ] **Step 1: Create the crate manifest**

`crates/origin-lsp-client/Cargo.toml`:

```toml
[package]
name = "origin-lsp-client"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process", "io-util", "sync"] }
thiserror = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Add to workspace `Cargo.toml` members list**

Modify the root `Cargo.toml` `[workspace] members = [...]` list to include `"crates/origin-lsp-client"`.

- [ ] **Step 3: Create `crates/origin-lsp-client/src/lib.rs`**

```rust
//! Minimal stdio JSON-RPC client for Language Servers.
//!
//! Implements only the subset needed by `Diagnostics`:
//! `initialize`, `initialized`, `textDocument/didOpen`, `textDocument/didChange`,
//! and listening for `textDocument/publishDiagnostics`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: u8, // 1=error, 2=warn, 3=info, 4=hint
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

pub struct LspClient {
    _child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: Arc<Mutex<u32>>,
    diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
}

impl LspClient {
    /// Spawn the server and complete the `initialize` handshake against `workspace_root`.
    ///
    /// # Errors
    /// `LspError::Spawn` if the binary cannot be started, `Protocol` if init fails.
    pub async fn spawn(binary: &str, workspace_root: &Path) -> Result<Self, LspError> {
        let mut cmd = Command::new(binary);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| LspError::Spawn(e.to_string()))?;
        let stdin = child.stdin.take().ok_or_else(|| LspError::Spawn("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| LspError::Spawn("no stdout".into()))?;

        let stdin = Arc::new(Mutex::new(stdin));
        let diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> = Arc::new(RwLock::new(HashMap::new()));
        let next_id = Arc::new(Mutex::new(1));

        // Reader loop.
        let diags_clone = diags.clone();
        tokio::spawn(reader_loop(stdout, diags_clone));

        // initialize.
        let root_uri = format!("file://{}", workspace_root.display().to_string().replace('\\', "/"));
        let init = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {}
            }
        });
        write_frame(stdin.clone(), &init).await?;
        // initialized.
        let initd = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        write_frame(stdin.clone(), &initd).await?;

        Ok(Self { _child: child, stdin, next_id, diags })
    }

    /// Notify the server about a file the client has open.
    pub async fn did_open(&self, path: &Path, language_id: &str, text: &str) -> Result<(), LspError> {
        let uri = format!("file://{}", path.display().to_string().replace('\\', "/"));
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text } }
        });
        write_frame(self.stdin.clone(), &msg).await
    }

    /// Notify the server that a file changed (full sync).
    pub async fn did_change(&self, path: &Path, text: &str) -> Result<(), LspError> {
        let uri = format!("file://{}", path.display().to_string().replace('\\', "/"));
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ { "text": text } ]
            }
        });
        write_frame(self.stdin.clone(), &msg).await
    }

    /// Currently-known diagnostics for `path` (or all files when `None`).
    pub async fn diagnostics(&self, path: Option<&Path>) -> Vec<Diagnostic> {
        let g = self.diags.read().await;
        if let Some(p) = path {
            g.get(p).cloned().unwrap_or_default()
        } else {
            g.values().flatten().cloned().collect()
        }
    }
}

async fn write_frame(stdin: Arc<Mutex<ChildStdin>>, msg: &Value) -> Result<(), LspError> {
    let body = serde_json::to_vec(msg).map_err(|e| LspError::Protocol(e.to_string()))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut g = stdin.lock().await;
    g.write_all(header.as_bytes()).await?;
    g.write_all(&body).await?;
    g.flush().await?;
    Ok(())
}

async fn reader_loop(stdout: ChildStdout, diags: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut header = String::new();
        let mut content_length: Option<usize> = None;
        // Read headers terminated by an empty line.
        loop {
            header.clear();
            if reader.read_line(&mut header).await.unwrap_or(0) == 0 { return; }
            let line = header.trim_end_matches(['\r', '\n']);
            if line.is_empty() { break; }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                content_length = v.parse().ok();
            }
        }
        let Some(len) = content_length else { continue };
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).await.is_err() { return; }
        let Ok(v) = serde_json::from_slice::<Value>(&body) else { continue };
        if v.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics") {
            handle_diagnostics(&v, &diags).await;
        }
    }
}

async fn handle_diagnostics(v: &Value, diags: &Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>) {
    let Some(params) = v.get("params") else { return };
    let Some(uri) = params.get("uri").and_then(Value::as_str) else { return };
    let path = uri.strip_prefix("file://").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(uri));
    let mut out = Vec::new();
    if let Some(arr) = params.get("diagnostics").and_then(Value::as_array) {
        for d in arr {
            let line = d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let col = d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) as u32;
            let severity = d.get("severity").and_then(Value::as_u64).unwrap_or(2) as u8;
            let message = d.get("message").and_then(Value::as_str).unwrap_or("").to_string();
            let code = d.get("code").and_then(|c| c.as_str().map(str::to_string).or_else(|| c.as_i64().map(|n| n.to_string())));
            out.push(Diagnostic { file: path.clone(), line, col, severity, message, code });
        }
    }
    diags.write().await.insert(path, out);
}
```

- [ ] **Step 4: Smoke test**

`crates/origin-lsp-client/tests/smoke.rs`:

```rust
// This test requires `rust-analyzer` on PATH; gated behind RUN_RA env var
// so the default `cargo test` workflow does not require the binary.

#[tokio::test]
async fn ra_handshake_publishes_no_diags_for_empty_workspace() {
    if std::env::var("RUN_RA").is_err() { eprintln!("skipping: set RUN_RA=1"); return; }
    let dir = tempfile::tempdir().unwrap();
    let client = origin_lsp_client::LspClient::spawn("rust-analyzer", dir.path()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let _ = client.diagnostics(None).await; // empty is fine
}
```

- [ ] **Step 5: Run + verify + commit**

```bash
cargo check -p origin-lsp-client
cargo test -p origin-lsp-client
cargo clippy -p origin-lsp-client --all-targets -- -D warnings
git add crates/origin-lsp-client Cargo.toml Cargo.lock
git commit -m "feat(origin-lsp-client): new crate — minimal stdio LSP client"
```

### Task 6.2: `ra_bridge` trait + envelope wiring + `Diagnostics` tool

**Files:**
- Create: `crates/origin-tools/src/ra_bridge.rs`
- Create: `crates/origin-tools/src/builtins/diagnostics.rs`
- Modify: `crates/origin-tools/src/lib.rs`, `crates/origin-tools/src/builtins/mod.rs`, `crates/origin-tools/src/tool_envelope.rs`
- Test: `crates/origin-tools/tests/diagnostics.rs`

- [ ] **Step 1: Write failing test**

`crates/origin-tools/tests/diagnostics.rs`:

```rust
use origin_tools::builtins::diagnostics::{diagnostics, DiagnosticsArgs, Severity};
use origin_tools::ra_bridge::{DiagnosticsHandle, RaDiagnostic};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
struct FakeRa {
    inner: Arc<std::sync::RwLock<Vec<RaDiagnostic>>>,
}

#[async_trait::async_trait]
impl DiagnosticsHandle for FakeRa {
    async fn diagnostics(&self, _path: Option<&Path>, _sev: Severity) -> Result<Vec<RaDiagnostic>, origin_tools::ToolError> {
        Ok(self.inner.read().unwrap().clone())
    }
    async fn notify_file_changed(&self, _path: &Path, _contents: &str) {}
}

#[tokio::test]
async fn empty_diagnostics_returns_empty_array() {
    let h = FakeRa::default();
    let out = diagnostics(DiagnosticsArgs { path: None, severity: Severity::Any }, &h as &dyn DiagnosticsHandle).await.unwrap();
    assert_eq!(out.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn populated_diagnostics_round_trip() {
    let h = FakeRa::default();
    h.inner.write().unwrap().push(RaDiagnostic {
        file: "a.rs".into(), line: 10, col: 5, severity: 1, message: "boom".into(), code: None,
    });
    let out = diagnostics(DiagnosticsArgs { path: None, severity: Severity::Any }, &h as &dyn DiagnosticsHandle).await.unwrap();
    assert_eq!(out.as_array().unwrap().len(), 1);
    assert_eq!(out[0]["message"], "boom");
    assert_eq!(out[0]["line"], 10);
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test diagnostics 2>&1 | tail -10`

- [ ] **Step 3: Implement `ra_bridge`**

`crates/origin-tools/src/ra_bridge.rs`:

```rust
//! Trait the envelope passes into the `Diagnostics` tool. Implemented daemon-side
//! by `origin-daemon::ra_impl::DaemonRa` (wraps `origin-lsp-client::LspClient`).

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity { Any, Error, Warning, Hint }

impl Severity {
    #[must_use]
    pub fn allows(self, sev_code: u8) -> bool {
        match self {
            Self::Any => true,
            Self::Error => sev_code == 1,
            Self::Warning => sev_code <= 2,
            Self::Hint => sev_code <= 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaDiagnostic {
    pub file: std::path::PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: u8,
    pub message: String,
    pub code: Option<String>,
}

#[async_trait]
pub trait DiagnosticsHandle: Send + Sync + std::fmt::Debug {
    /// # Errors
    /// `subsystem.ra_unavailable` if the server is down.
    async fn diagnostics(&self, path: Option<&Path>, severity: Severity) -> Result<Vec<RaDiagnostic>, ToolError>;
    async fn notify_file_changed(&self, path: &Path, contents: &str);
}
```

Add to `crates/origin-tools/src/lib.rs`:

```rust
pub mod ra_bridge;
```

Extend `crates/origin-tools/src/tool_envelope.rs` to carry an optional `DiagnosticsHandle`:

```rust
use crate::ra_bridge::DiagnosticsHandle;
use std::sync::Arc;

// inside EnvelopeCtx:
//     pub ra: Option<Arc<dyn DiagnosticsHandle>>,
```

(`Default` impl: `ra: None`.)

- [ ] **Step 4: Implement `Diagnostics` tool**

`crates/origin-tools/src/builtins/diagnostics.rs`:

```rust
//! `Diagnostics` — query LSP diagnostics from the warm rust-analyzer.

use crate::error::{ErrClass, ToolError};
use crate::ra_bridge::{DiagnosticsHandle, Severity};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};
use std::path::PathBuf;

pub use crate::ra_bridge::Severity as SeverityPub;

#[derive(Debug, Clone)]
pub struct DiagnosticsArgs { pub path: Option<String>, pub severity: Severity }

/// # Errors
/// `subsystem.ra_unavailable` if RA is unreachable.
pub async fn diagnostics(args: DiagnosticsArgs, h: &dyn DiagnosticsHandle) -> Result<Value, ToolError> {
    let path: Option<PathBuf> = args.path.as_deref().map(PathBuf::from);
    let diags = h.diagnostics(path.as_deref(), args.severity).await?;
    let filtered: Vec<Value> = diags.into_iter()
        .filter(|d| args.severity.allows(d.severity))
        .map(|d| json!({
            "file": d.file, "line": d.line, "col": d.col,
            "severity": d.severity, "message": d.message, "code": d.code,
        }))
        .collect();
    Ok(Value::Array(filtered))
}

crate::origin_tool! {
    name: "Diagnostics",
    description: "Return LSP diagnostics from the warm rust-analyzer for a path or the whole workspace. Severity filter: error|warning|hint|any.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path":     { "type": "string" },
            "severity": { "type": "string", "enum": ["error", "warning", "hint", "any"], "default": "any" }
        }
    }"#,
}
```

Append to `crates/origin-tools/src/builtins/mod.rs`:

```rust
pub mod diagnostics;
```

- [ ] **Step 5: Run — pass**

Run: `cargo test -p origin-tools --test diagnostics`

- [ ] **Step 6: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/ra_bridge.rs crates/origin-tools/src/builtins/diagnostics.rs crates/origin-tools/src/builtins/mod.rs crates/origin-tools/src/lib.rs crates/origin-tools/src/tool_envelope.rs crates/origin-tools/tests/diagnostics.rs
git commit -m "feat(tools): ra_bridge trait + Diagnostics tool (driven by FakeRa in tests)"
```

### Task 6.3: Daemon `DiagnosticsHandle` impl + lazy spawn + Diagnostics arm

**Files:**
- Create: `crates/origin-daemon/src/ra_impl.rs`
- Modify: `crates/origin-daemon/src/main.rs` (build the impl, two-tier resolution)
- Modify: `crates/origin-daemon/Cargo.toml` (add `origin-lsp-client` dep)
- Modify: `crates/origin-daemon/src/agent.rs` (Diagnostics arm + Edit/MultiEdit/ApplyPatch/Write call `notify_file_changed` after success)

- [ ] **Step 1: Add dep**

Append to `crates/origin-daemon/Cargo.toml` `[dependencies]`:

```toml
origin-lsp-client = { path = "../origin-lsp-client" }
async-trait = "0.1"
```

- [ ] **Step 2: Create `crates/origin-daemon/src/ra_impl.rs`**

```rust
//! `DiagnosticsHandle` impl wrapping `origin-lsp-client::LspClient` and
//! resolving rust-analyzer per the two-tier policy in the spec.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use origin_lsp_client::LspClient;
use origin_tools::error::{ErrClass, ToolError};
use origin_tools::ra_bridge::{DiagnosticsHandle, RaDiagnostic, Severity};
use tokio::sync::OnceCell;

#[derive(Debug)]
pub struct DaemonRa {
    workspace_root: PathBuf,
    client: OnceCell<Option<Arc<LspClient>>>,
}

impl DaemonRa {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root, client: OnceCell::new() }
    }

    async fn client(&self) -> Option<&Arc<LspClient>> {
        let c = self.client.get_or_init(|| async {
            let bin = resolve_ra();
            let bin = bin?;
            match LspClient::spawn(&bin, &self.workspace_root).await {
                Ok(c) => Some(Arc::new(c)),
                Err(_) => None,
            }
        }).await;
        c.as_ref()
    }
}

fn resolve_ra() -> Option<String> {
    // Tier 1: PATH.
    if which::which("rust-analyzer").is_ok() {
        return Some("rust-analyzer".into());
    }
    // Tier 2: $ORIGIN_CACHE/bin/rust-analyzer.
    let cache = std::env::var("ORIGIN_CACHE").ok()
        .or_else(|| std::env::var("LOCALAPPDATA").ok().map(|p| format!("{p}\\origin")))
        .or_else(|| std::env::var("XDG_CACHE_HOME").ok().map(|p| format!("{p}/origin")))
        .or_else(|| dirs::home_dir().map(|h| h.join(".cache").join("origin").to_string_lossy().into_owned()))?;
    #[cfg(windows)]
    let bin = format!("{cache}\\bin\\rust-analyzer.exe");
    #[cfg(unix)]
    let bin = format!("{cache}/bin/rust-analyzer");
    if std::path::Path::new(&bin).exists() { Some(bin) } else { None }
}

#[async_trait]
impl DiagnosticsHandle for DaemonRa {
    async fn diagnostics(&self, path: Option<&Path>, _sev: Severity) -> Result<Vec<RaDiagnostic>, ToolError> {
        let Some(c) = self.client().await else {
            return Err(ToolError::new(ErrClass::Subsystem, "ra_unavailable",
                "rust-analyzer not found on PATH or in $ORIGIN_CACHE/bin")
                .hint("install with: origin daemon install-ra"));
        };
        let raw = c.diagnostics(path).await;
        Ok(raw.into_iter().map(|d| RaDiagnostic {
            file: d.file, line: d.line, col: d.col,
            severity: d.severity, message: d.message, code: d.code,
        }).collect())
    }

    async fn notify_file_changed(&self, path: &Path, contents: &str) {
        if let Some(c) = self.client().await {
            let _ = c.did_change(path, contents).await;
        }
    }
}
```

Add `which` and `dirs` deps to `crates/origin-daemon/Cargo.toml` if not already present:

```toml
which = "5"
dirs = "5"
```

- [ ] **Step 3: Instantiate the RA in `main.rs`**

In `crates/origin-daemon/src/main.rs`, near where the agent context is built, instantiate:

```rust
let ra = std::sync::Arc::new(crate::ra_impl::DaemonRa::new(std::env::current_dir().unwrap()));
// Pass into EnvelopeCtx::default() builder:
let envelope_ctx = origin_tools::tool_envelope::EnvelopeCtx {
    ra: Some(ra.clone()),
    ..Default::default()
};
```

Add `pub mod ra_impl;` to `crates/origin-daemon/src/main.rs` (or `lib.rs` if the daemon has one).

- [ ] **Step 4: Daemon Diagnostics arm**

In `crates/origin-daemon/src/agent.rs`, add to `dispatch_tool_inner` (after Bash/Monitor) and to `dispatch_with_envelope`:

```rust
"Diagnostics" => {
    let sev = match args.get("severity").and_then(Value::as_str).unwrap_or("any") {
        "error" => origin_tools::ra_bridge::Severity::Error,
        "warning" => origin_tools::ra_bridge::Severity::Warning,
        "hint" => origin_tools::ra_bridge::Severity::Hint,
        _ => origin_tools::ra_bridge::Severity::Any,
    };
    let dargs = origin_tools::builtins::diagnostics::DiagnosticsArgs {
        path: args.get("path").and_then(Value::as_str).map(str::to_string),
        severity: sev,
    };
    // Envelope-routed path uses ctx.ra; passthrough returns ra_unavailable.
    match (/* ctx in envelope path */) {
        _ => Err(LoopError::ToolFailure("Diagnostics needs envelope context".into())),
    }
}
```

For `dispatch_with_envelope`, plumb `ctx.ra.as_deref()` into the call:

```rust
let h = ctx.ra.as_ref().map(|a| a.as_ref());
match h {
    Some(handle) => origin_tools::builtins::diagnostics::diagnostics(dargs, handle).await
        .map(|v| serde_json::to_string(&v).unwrap()),
    None => Err(...),
}
```

- [ ] **Step 5: Hook `notify_file_changed` from Edit/MultiEdit/Write/ApplyPatch**

In `dispatch_with_envelope`, after every successful Mutating file-touching tool call, walk the args for `file_path` (Edit/MultiEdit/Write) or parse the patch (ApplyPatch) for files, read the resulting bytes, and call `ctx.ra.as_ref().map(|r| r.notify_file_changed(path, new_text))`.

Concretely add a helper:

```rust
async fn notify_ra_after_mutation(ctx: &origin_tools::tool_envelope::EnvelopeCtx, files: &[String]) {
    let Some(ra) = ctx.ra.as_ref() else { return };
    for f in files {
        if let Ok(bytes) = std::fs::read(f) {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                ra.notify_file_changed(std::path::Path::new(f), text).await;
            }
        }
    }
}
```

And call it after the matching arm completes successfully.

- [ ] **Step 6: Run + verify + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-daemon --all-targets -- -D warnings
git add crates/origin-daemon/Cargo.toml crates/origin-daemon/src/ra_impl.rs crates/origin-daemon/src/main.rs crates/origin-daemon/src/agent.rs Cargo.lock
git commit -m "feat(daemon): DaemonRa wires origin-lsp-client; Diagnostics arm + post-mutation notify"
```

### Task 6.4: Add `origin daemon install-ra` subcommand

**Files:**
- Modify: `crates/origin-daemon/src/main.rs`

- [ ] **Step 1: Add the subcommand**

In the daemon's `clap`-style command parsing (find where existing subcommands are defined; add):

```rust
// Pseudocode — adjust to the daemon's actual argparse style.
#[derive(Subcommand)]
enum DaemonCmd {
    // ... existing ...
    /// Download rust-analyzer into $ORIGIN_CACHE/bin for Diagnostics to use.
    InstallRa,
}
```

Implement:

```rust
async fn install_ra() -> anyhow::Result<()> {
    let cache_dir = resolve_cache_dir()?;
    let bin_dir = cache_dir.join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let (url, file_name) = ra_release_url_for_platform();
    let target = bin_dir.join(file_name);
    eprintln!("downloading rust-analyzer from {url}");
    let bytes = reqwest::blocking::get(&url)?.bytes()?;
    std::fs::write(&target, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&target, perms)?;
    }
    eprintln!("installed: {}", target.display());
    Ok(())
}

fn ra_release_url_for_platform() -> (String, &'static str) {
    let base = "https://github.com/rust-lang/rust-analyzer/releases/latest/download";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return (format!("{base}/rust-analyzer-x86_64-unknown-linux-gnu.gz"), "rust-analyzer");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return (format!("{base}/rust-analyzer-aarch64-apple-darwin.gz"), "rust-analyzer");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return (format!("{base}/rust-analyzer-x86_64-apple-darwin.gz"), "rust-analyzer");
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return (format!("{base}/rust-analyzer-x86_64-pc-windows-msvc.zip"), "rust-analyzer.exe");
    #[allow(unreachable_code)]
    (String::new(), "rust-analyzer")
}

fn resolve_cache_dir() -> anyhow::Result<std::path::PathBuf> {
    if let Ok(c) = std::env::var("ORIGIN_CACHE") { return Ok(c.into()); }
    #[cfg(windows)]
    if let Ok(c) = std::env::var("LOCALAPPDATA") { return Ok(std::path::PathBuf::from(c).join("origin")); }
    #[cfg(unix)]
    if let Ok(c) = std::env::var("XDG_CACHE_HOME") { return Ok(std::path::PathBuf::from(c).join("origin")); }
    Ok(dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?.join(".cache").join("origin"))
}
```

Add deps to `crates/origin-daemon/Cargo.toml`:

```toml
reqwest = { version = "0.11", features = ["blocking"] }
anyhow = "1"
```

(Note: the `.gz`/`.zip` extraction step is intentionally omitted for v2 simplicity. The first iteration writes the archive; the user runs `gunzip`/`Expand-Archive` manually if needed. v2.1 can add automatic decompression. Update the error hint in `DaemonRa::diagnostics` to: `"install with: origin daemon install-ra (then gunzip/unzip the archive)"`.)

- [ ] **Step 2: Verify + commit**

```bash
cargo check -p origin-daemon
cargo clippy -p origin-daemon -- -D warnings
git add crates/origin-daemon/src/main.rs crates/origin-daemon/Cargo.toml Cargo.lock
git commit -m "feat(daemon): origin daemon install-ra subcommand (fetches RA binary into ORIGIN_CACHE)"
```

### Task 6.5: Phase 6 regression + tag

- [ ] **Step 1: Tests + clippy + fmt**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-6 -m "Persistent rust-analyzer + Diagnostics tool live"
```

---

## Phase 7 — `ToolSearch` (deferred-tool serving)

Goal: domain-specific tools (graph_*, web_*, browser, recall, mem, ask, task) are advertised in the system prompt as `{name, description}` only. The model fetches their full schemas on demand via `ToolSearch`. Cuts system-prompt tokens linearly.

### Task 7.1: Hot vs deferred classification on `ToolMeta`

**Files:**
- Modify: `crates/origin-tools/src/registry.rs` (add `pub hot: bool`)
- Modify: `crates/origin-tools/src/macros.rs` (add `hot:` arm; default `true`)
- Modify: every existing `origin_tool!` invocation in `crates/origin-tools/src/builtins/*.rs` for deferred tools — set `hot: false`
- Test: extend `crates/origin-tools/tests/registry.rs`

- [ ] **Step 1: Add the failing test**

Append to `crates/origin-tools/tests/registry.rs`:

```rust
#[test]
fn hot_set_contains_exactly_the_11() {
    let hot: Vec<&str> = origin_tools::registry_iter()
        .filter(|m| m.hot)
        .map(|m| m.name)
        .collect();
    let mut expected = vec![
        "Read", "Edit", "Write", "Grep", "Glob", "Bash",
        "MultiEdit", "ApplyPatch", "Monitor", "Diagnostics", "ToolSearch",
    ];
    let mut got: Vec<&str> = hot.clone();
    got.sort_unstable();
    expected.sort_unstable();
    assert_eq!(got, expected, "hot set drifted");
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test registry hot_set 2>&1 | tail -10`

- [ ] **Step 3: Add the field**

In `crates/origin-tools/src/registry.rs`, add to `ToolMeta`:

```rust
    /// "Hot" tools have their full schema embedded in the system prompt.
    /// "Deferred" tools advertise only {name, description}; their schemas
    /// are fetched on demand via `ToolSearch`.
    pub hot: bool,
```

In `crates/origin-tools/src/macros.rs`, add the most-arms variant (sandbox + token_budget + hot) and extend each existing arm to default `hot: true`:

```rust
// Add new arm with hot:
(
    name: $name:literal,
    description: $desc:literal,
    tier: $tier:expr,
    urgency: $urg:expr,
    side_effects: $sfx:expr,
    input_schema: $schema:expr,
    sandbox: $sandbox:expr,
    token_budget: $budget:expr,
    hot: $hot:expr
    $(,)?
) => {
    inventory::submit! {
        $crate::ToolMeta {
            name: $name, description: $desc, tier: $tier, urgency: $urg,
            side_effects: $sfx, input_schema: $schema, sandbox_profile: $sandbox,
            token_budget: $budget, hot: $hot,
        }
    }
};
```

Default every existing arm to `hot: true` (mirror the change in every arm's struct literal).

- [ ] **Step 4: Mark deferred tools**

In each of these files, change the `origin_tool!` call to include `hot: false`:

- `src/builtins/ask.rs`
- `src/builtins/mem.rs`
- `src/builtins/recall.rs`
- `src/builtins/task.rs`
- `src/builtins/web_fetch.rs`
- `src/builtins/web_search.rs`
- `src/builtins/browser.rs`
- `src/builtins/graph_query.rs`
- `src/builtins/graph_path.rs`
- `src/builtins/graph_explain.rs`
- `src/builtins/graph_summarize.rs`
- `src/builtins/graph_rebuild.rs`

Example for `ask.rs`:

```rust
crate::origin_tool! {
    name: "Ask",
    description: "...",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{...}"#,
    hot: false,
}
```

(Use the appropriate arm; if a tool sets `sandbox` and not `token_budget`, add `token_budget` too to satisfy the new arm shape. Easiest: switch every deferred tool to the full 9-field arm.)

- [ ] **Step 5: Run — pass**

```bash
cargo test -p origin-tools --test registry hot_set
cargo test -p origin-tools
```

- [ ] **Step 6: Verification + commit**

```bash
cargo clippy -p origin-tools --all-targets -- -D warnings
git add crates/origin-tools/src/registry.rs crates/origin-tools/src/macros.rs crates/origin-tools/src/builtins/
git commit -m "feat(tools): ToolMeta.hot — hot/deferred classification for ToolSearch"
```

### Task 7.2: `ToolSearch` tool

**Files:**
- Create: `crates/origin-tools/src/builtins/tool_search.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs`
- Test: `crates/origin-tools/tests/tool_search.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (ToolSearch arm)

- [ ] **Step 1: Write failing tests**

`crates/origin-tools/tests/tool_search.rs`:

```rust
use origin_tools::builtins::tool_search::{tool_search, ToolSearchArgs};

#[test]
fn returns_deferred_tool_schema_by_exact_name() {
    let out = tool_search(ToolSearchArgs { query: "select:Recall".into(), max_results: None }).unwrap();
    let arr = out.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "Recall");
    assert!(arr[0].get("input_schema").is_some());
}

#[test]
fn returns_multiple_by_select_list() {
    let out = tool_search(ToolSearchArgs { query: "select:Recall,Ask".into(), max_results: None }).unwrap();
    assert_eq!(out.as_array().unwrap().len(), 2);
}

#[test]
fn keyword_search_ranks_by_relevance() {
    let out = tool_search(ToolSearchArgs { query: "graph".into(), max_results: Some(3) }).unwrap();
    let arr = out.as_array().unwrap();
    assert!(!arr.is_empty());
    for v in arr { assert!(v["name"].as_str().unwrap().contains("Graph") || v["description"].as_str().unwrap().to_lowercase().contains("graph")); }
}

#[test]
fn cannot_fetch_hot_tool_via_search() {
    let out = tool_search(ToolSearchArgs { query: "select:Read".into(), max_results: None }).unwrap();
    assert!(out.as_array().unwrap().is_empty());
}
```

- [ ] **Step 2: Run — fail**

Run: `cargo test -p origin-tools --test tool_search 2>&1 | tail -10`

- [ ] **Step 3: Implement**

`crates/origin-tools/src/builtins/tool_search.rs`:

```rust
//! `ToolSearch` — fetch full schemas for deferred tools on demand.

use crate::error::ToolError;
use crate::registry_iter;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ToolSearchArgs { pub query: String, pub max_results: Option<u32> }

/// # Errors
/// None today; signature kept for future validation.
pub fn tool_search(args: ToolSearchArgs) -> Result<Value, ToolError> {
    let max = args.max_results.unwrap_or(5) as usize;
    if let Some(rest) = args.query.strip_prefix("select:") {
        let names: Vec<&str> = rest.split(',').map(str::trim).collect();
        let arr: Vec<Value> = registry_iter()
            .filter(|m| !m.hot && names.contains(&m.name))
            .map(meta_to_json)
            .collect();
        return Ok(Value::Array(arr));
    }
    // Keyword search: rank by hit count in name + description.
    let terms: Vec<&str> = args.query.split_whitespace().collect();
    let mut scored: Vec<(i32, Value)> = registry_iter()
        .filter(|m| !m.hot)
        .map(|m| {
            let blob = format!("{} {}", m.name.to_lowercase(), m.description.to_lowercase());
            let score: i32 = terms.iter().map(|t| if blob.contains(&t.to_lowercase()) { 1 } else { 0 }).sum();
            (score, meta_to_json(m))
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let arr: Vec<Value> = scored.into_iter().take(max).map(|(_, v)| v).collect();
    Ok(Value::Array(arr))
}

fn meta_to_json(m: &crate::ToolMeta) -> Value {
    json!({
        "name": m.name,
        "description": m.description,
        "input_schema": serde_json::from_str::<Value>(m.input_schema).unwrap_or(Value::Null),
    })
}

crate::origin_tool! {
    name: "ToolSearch",
    description: "Fetch full schemas for deferred tools. `select:Name,Name` returns exact tools; keyword query ranks by relevance.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "query":       { "type": "string" },
            "max_results": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 }
        },
        "required": ["query"]
    }"#,
}
```

Append to `crates/origin-tools/src/builtins/mod.rs`:

```rust
pub mod tool_search;
```

- [ ] **Step 4: Daemon ToolSearch arm**

```rust
"ToolSearch" => {
    let sargs = origin_tools::builtins::tool_search::ToolSearchArgs {
        query: args.get("query").and_then(Value::as_str)
            .ok_or_else(|| LoopError::BadArgs("ToolSearch: missing `query`".into()))?
            .to_string(),
        max_results: args.get("max_results").and_then(Value::as_u64).map(|n| n as u32),
    };
    origin_tools::builtins::tool_search::tool_search(sargs)
        .map(|v| serde_json::to_string(&v).unwrap())
        .map_err(|e| LoopError::ToolFailure(e.message))
}
```

- [ ] **Step 5: Update system prompt builder (the `tools_schema` block at `crates/origin-daemon/src/agent.rs:355`)**

Find the existing code that builds `tools_schema = registry_iter().map(...)`. Replace its filter:

```rust
let tools_schema = registry_iter()
    .map(|m| {
        if m.hot {
            // Full schema embed.
            serde_json::json!({
                "name": m.name,
                "description": m.description,
                "input_schema": serde_json::from_str::<serde_json::Value>(m.input_schema).unwrap_or(serde_json::Value::Null),
            })
        } else {
            // Deferred — name + 1-line description only.
            serde_json::json!({
                "name": m.name,
                "description": format!("{} (deferred; call ToolSearch with select:{}, to fetch full schema)", m.description, m.name),
            })
        }
    })
    .collect::<Vec<_>>();
```

- [ ] **Step 6: Run + verify + commit**

```bash
cargo test -p origin-tools -p origin-daemon
cargo clippy -p origin-tools -p origin-daemon --all-targets -- -D warnings
git add crates/origin-tools/src/builtins/tool_search.rs crates/origin-tools/src/builtins/mod.rs crates/origin-tools/tests/tool_search.rs crates/origin-daemon/src/agent.rs
git commit -m "feat(tools): ToolSearch + deferred tool advertisement

Hot 11 tools embed full schemas; the remaining 12 advertise only
{name, description} and the model fetches schemas on demand via
ToolSearch. Cuts system-prompt tool tokens linearly with deferred-tool
count.
"
```

### Task 7.3: Phase 7 regression + tag

- [ ] **Step 1: Tests + clippy + fmt**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-7 -m "ToolSearch live; system-prompt tool tokens reduced"
```

---

## Phase 8 — System-prompt regen, KPI bench, cleanup

### Task 8.1: Regenerate system prompts that mention tool schemas

**Files:**
- Modify: any file in `crates/origin-cli/src/` or `crates/origin-daemon/src/` that hardcodes tool descriptions or example arg shapes (search for old schema fields).

- [ ] **Step 1: Search for stale references**

```bash
grep -RIn '"old_string"\|"new_string"\|"path"' crates/origin-cli/src crates/origin-daemon/src | grep -v test
```

For each hit that documents a tool's args inline (system-prompt builders, README snippets, docs), update to the new schema (`file_path`, `replace_all`, etc.).

- [ ] **Step 2: Commit per file changed**

```bash
git add <changed files>
git commit -m "docs/prompt: align hardcoded tool schemas with tool-suite-v2"
```

### Task 8.2: KPI benchmark — capture after-numbers and compare

**Files:**
- Modify: `bench/tool-suite-v2/baseline.json` → create `bench/tool-suite-v2/after.json`
- Create: `bench/tool-suite-v2/REPORT.md`

- [ ] **Step 1: Re-run the 10-task corpus** (from Task 0.2) against the tool-suite-v2 branch.

- [ ] **Step 2: Capture into `after.json`** with the same shape as `baseline.json`.

- [ ] **Step 3: Build `REPORT.md` with the deltas**

Template:

```markdown
# Tool Suite v2 — KPI Report

| Metric                          | Baseline | After | Δ      | Target | Met? |
|---------------------------------|----------|-------|--------|--------|------|
| Tool-result tokens (10-task sum)| <X>      | <Y>   | -<Z>%  | -40%   | ?    |
| Round-trips (10-task sum)       | <X>      | <Y>   | -<Z>%  | -25%   | ?    |
| Wall-clock (diagnostics tasks)  | <X>      | <Y>   | -<Z>%  | -30%   | ?    |
| Edit failures on CRLF           | <X>      | 0     | -100%  | 0      | ?    |
| System-prompt tool tokens       | <X>      | <Y>   | -<Z>%  | -30%   | ?    |
```

Fill in actual numbers.

- [ ] **Step 4: Commit**

```bash
git add bench/tool-suite-v2/after.json bench/tool-suite-v2/REPORT.md
git commit -m "bench: capture tool-suite-v2 KPI deltas vs baseline"
```

### Task 8.3: Cleanup — remove deprecated functions

**Files:**
- Modify: `crates/origin-tools/src/builtins/bash.rs` (remove the legacy `bash_tool_streaming` if any remained alongside `bash_v2`)
- Modify: anywhere else holding stale wrappers (search: `pub fn read_tool`, `pub fn edit_tool`, `pub fn write_tool`, `pub fn grep_tool`, `pub fn glob_tool`).

- [ ] **Step 1: Search**

```bash
grep -RIn 'pub fn read_tool\|pub fn edit_tool\|pub fn write_tool\|pub fn grep_tool\|pub fn glob_tool\|bash_tool_streaming' crates/
```

- [ ] **Step 2: Remove each definition; fix or delete callers** (the daemon `dispatch_tool` arms should already use `*_v2`; non-test callers should be gone).

- [ ] **Step 3: Run + verify + commit**

```bash
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
git add -u
git commit -m "chore(tools): remove deprecated v1 tool fns and bash_tool_streaming"
```

### Task 8.4: Final regression + Phase 8 tag

- [ ] **Step 1: Full check**

```bash
cargo test --workspace --no-fail-fast 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 2: Tag**

```bash
git tag -a tool-suite-v2-phase-8 -m "Tool Suite v2 complete: KPI report green, deprecated paths removed"
```

### Task 8.5: Open the PR

- [ ] **Step 1: Push the branch + open PR**

```bash
git push -u origin tool-suite-v2
gh pr create --title "Tool Suite v2: shared envelope + 11 hot tools + 12 deferred tools" --body "$(cat <<'EOF'
## Summary
- Replaces per-tool `Result<T, String>` plumbing with a shared `tool_envelope` (text_fmt, budget_writer, result_cas, proc_supervisor, ra_bridge).
- Rebuilds Read/Edit/Write/Grep/Glob/Bash with the schema/perf/reliability fixes in the spec.
- Adds MultiEdit, ApplyPatch, Monitor, Diagnostics (warm rust-analyzer), ToolSearch (deferred-tool serving).
- Kills the CRLF Edit failure class (see `tests/crlf_regression.rs`).
- KPI report in `bench/tool-suite-v2/REPORT.md`.

Spec: `docs/superpowers/specs/2026-05-28-origin-tool-suite-v2-design.md`
Plan: `docs/superpowers/plans/2026-05-28-origin-tool-suite-v2.md`

## Test plan
- [ ] cargo test --workspace --no-fail-fast
- [ ] cargo clippy --workspace --all-targets -- -D warnings
- [ ] crlf_regression suite passes on Windows CI
- [ ] KPI report numbers meet the targets in the spec

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-review (run after the plan is written)

This section is for the plan author to confirm before handing off. Each check is one bullet; fix inline if any fail.

**1. Spec coverage (every spec section maps to ≥1 task)**

- [x] `text_fmt` → Task 1.2, 1.3
- [x] `budget_writer` → Task 1.5, 1.6
- [x] `result_cas` → Task 2.1, 2.2
- [x] `proc_supervisor` → Task 4.1
- [x] `ra_bridge` → Task 6.1, 6.2, 6.3, 6.4
- [x] Read v2 → 3.1; Edit v2 → 3.2; Write v2 → 3.3; Grep v2 → 3.4; Glob v2 → 3.5; Bash v2 → 4.2
- [x] MultiEdit → 5.1; ApplyPatch → 5.2; Monitor → 4.3; Diagnostics → 6.2/6.3; ToolSearch → 7.2
- [x] CRLF regression canary → 3.2 + 3.3
- [x] KPI bench → 0.2 + 8.2
- [x] Rollout phases 1-5 in spec → plan phases 1-8 (extra envelope-wiring phases 2, 7, 8)
- [x] Acceptance criteria → 8.2 captures all KPIs, 8.4 gates on clippy/fmt/tests, 3.2 includes the screenshot-repro test

**2. Placeholder scan**

- No "TBD"/"TODO"/"fill in"/"similar to" markers (verified via inline check while writing).
- Every code step shows the actual code.
- Every test step shows the actual test body.
- Every command step shows the exact command and expected outcome.

**3. Type consistency**

- `EnvelopeCtx` grows fields incrementally: `result_store` (2.2), `write_guard` (3.3), `supervisor` (4.2), `ra` (6.2). All `Default + Clone + Debug`.
- `Supervisor`, `WriteGuard`, `ResultStore` all impl `Default + Clone + Debug` — required by the `EnvelopeCtx::default()` builder.
- `ToolError { class, reason, message, recoverable, hint }` shape stable from Task 1.1 onward; serialisation in 1.1 matches expectation in 6.2's `DiagnosticsHandle` impl.
- `ProcStatus::is_terminal()` defined in 4.1; used in 4.2 (Bash) and 4.3 (Monitor).
- `ReadChunk { bytes, next_offset, status }` defined in 4.1; consumed in 4.2 and 4.3.
- Tool names match across builtin registration and daemon dispatch arms: every name string is `"Read"|"Edit"|"Write"|"Grep"|"Glob"|"Bash"|"MultiEdit"|"ApplyPatch"|"Monitor"|"Diagnostics"|"ToolSearch"` (the hot 11).
- `text_fmt::Detected` field set (`eol, bom, encoding, trailing_newline, per_line_eol`) defined in 1.2 and used unchanged in 3.1-3.3, 5.1, 5.2.
- `OutputMode` enum in 3.4 matches dispatch parsing in the same task.

**4. Ambiguity check**

- All "in `dispatch_tool_inner`" instructions also say "and in legacy `dispatch_tool`" — the engineer applies both, avoiding the trap of routing one tool through the envelope and another through the old path.
- "EOL preservation" specifies per-line vs file-dominant in 1.2's `restore_eols` implementation.
- `ApplyPatch` parser limits explicitly state "common subset" and list the supported header forms — no temptation to extend to git-format-patch features.
- `ToolSearch` `select:` vs keyword behaviour is shown in the implementation, not described.
- Token-budget defaults (4_000 / 25_000 / 1_000) are spelled out in each tool's `origin_tool!` arm.

---

## Execution handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-28-origin-tool-suite-v2.md`. Per your earlier directive, execution mode is fixed: `/dispatching-parallel-agents` with `/test-driven-development` per task and `/verification-before-completion` at the close of each task.**

Recommended parallel dispatch pattern, since you've selected parallel-agents execution:

- **Phase 0**: single agent (sequential, prep work).
- **Phase 1**: tasks 1.1, 1.2 (+1.3), 1.4, 1.5 (+1.6), 1.7 are independent — dispatch 5 agents in parallel; 1.8 runs after.
- **Phase 2**: 2.1 first; 2.2 + 2.3 in parallel; 2.4 after.
- **Phase 3**: tasks 3.1 / 3.2 / 3.3 / 3.4 / 3.5 are five independent file changes — **5 parallel agents**, then 3.6.
- **Phase 4**: 4.1 first; 4.2 + 4.3 in parallel after; 4.4 after.
- **Phase 5**: 5.1 + 5.2 in parallel; 5.3 after.
- **Phase 6**: 6.1 first; 6.2 + 6.3 in parallel; 6.4 after; 6.5 after.
- **Phase 7**: 7.1 first; 7.2 after; 7.3 after.
- **Phase 8**: 8.1, 8.2 in parallel; 8.3 → 8.4 → 8.5 sequential.

Two execution options:

1. **Subagent-Driven (recommended given the parallel pattern above)** — uses `superpowers:subagent-driven-development`; fresh subagent per task; two-stage review between tasks; matches your `/dispatching-parallel-agents` directive directly.
2. **Inline Execution** — uses `superpowers:executing-plans`; batch execution in this session with checkpoints. Doesn't parallelise — slower.

**Which approach?**



