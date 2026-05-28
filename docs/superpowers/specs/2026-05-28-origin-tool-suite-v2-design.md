# Origin Tool Suite v2 — Design

**Date:** 2026-05-28
**Status:** Approved, awaiting implementation plan
**Scope:** `crates/origin-tools` (primary), `crates/origin-daemon` (envelope wiring + RA bridge), new crate `crates/origin-lsp-client`
**Schema compatibility:** Free to break. Origin is pre-1.0 and tool schemas are model-facing; the system prompt is updated atomically with each tool change.

---

## Motivation

The current tool layer in `origin-tools` works but is per-tool: each builtin reads its inputs, executes, and returns `Result<T, String>`. Reliability, performance, and token-efficiency live inside each tool individually — or not at all. Concrete failures observed in production use:

- **`Edit` fails on CRLF files** when the model emits an LF needle. `edit.rs:11` does `contents.matches(old)` with no line-ending normalization. On Windows this is the single most common Edit failure mode.
- **`Read` returns the whole file** with no offset/limit and no line numbers. Burns tokens on large files.
- **`Grep` returns an unbounded `Vec<String>`** with no `head_limit`, no output mode (files-only/count), no context lines, no glob/type filter. A hot pattern can return thousands of lines.
- **`Bash` has no timeout, no background mode, no cwd**. Long-running commands hang the turn; the model cannot launch a dev server and keep working.
- **No `MultiEdit`** — N edits to one file requires N round-trips.
- **No diagnostics tool** — model must run `cargo check` (10-30s on this workspace) to see type errors. With persistent rust-analyzer this drops to <100ms.
- **No `ApplyPatch`** — large multi-file changes are awkward through Edit's unique-match constraint.
- **System prompt carries every tool's full schema** — schemas scale linearly with tool count, eating context budget.

This design addresses all of the above plus a class of latent failures (encoding, BOMs, atomicity, process supervision) through a single shared infrastructure layer that every tool inherits from.

## Goals & Non-goals

### Goals (KPI targets, measured before/after on a fixed 10-task set)

- **Tool-result tokens per turn**: ≥40% reduction. Drivers: `Read` offset/limit, `Grep` `head_limit` + `files_with_matches` default, output-CAS dedup, budget-aware result writer with eliding.
- **Tool-call round-trips per task**: ≥25% reduction. Drivers: `MultiEdit`, `ApplyPatch`, deferred-tool serving via `ToolSearch`.
- **Wall-clock turn latency**: ≥30% reduction for any task that touches diagnostics. Driver: persistent rust-analyzer.
- **Edit failure rate on Windows**: 0% from CRLF mismatch. Driver: `text_fmt` normaliser.

### Non-goals

- Backward compatibility with current schemas (explicitly waived).
- MCP-discovered tool reshaping. `DynTool` continues to work; it just runs through the same envelope.
- Multi-provider tool-call semantics — handled at the `origin-provider-*` layer, not here.
- The graph/memory/web/browser tools' internals — they keep their current implementations and inherit envelope wins.

## Constraints

- Per the project's novel-implementations rule (see `memory/feedback_novel_implementations.md`): every signature mechanism in this design must be novel, not a port from openclaude / jcode / opencode. Standard plumbing patterns are fine for non-signature parts.
- MSRV: pinned to Rust 1.83 (`memory/project_msrv_dep_pinning.md`). New deps must be checked for edition2024.
- Cross-platform: Windows and Linux are P0; macOS inherits the unix path.

---

## Architecture

A single new module — `tool_envelope` — sits between `dispatch` and every per-tool function. It orchestrates input canonicalisation, result-CAS lookup, budget-aware result building, streaming chunk dispatch, and process supervisor wiring. Every builtin and every `DynTool` flows through it.

```
agent.rs (origin-daemon)
  └── dispatch::invoke(name, args)
        └── tool_envelope::run(meta, args, ctx)        ← NEW
              ├── input canonicalisation (paths, regex, EOL normalisation)
              ├── result-CAS lookup (extends existing dispatch::Cache)
              ├── budget-aware ResultWriter (token cap + continuation handle)
              ├── streaming chunk channel (generalised from bash_tool_streaming)
              ├── process supervisor handle (for Bash/Monitor)
              └── per-tool fn(NormalizedCtx) -> EnvelopedResult
```

### New crate-internal modules (in `origin-tools`)

- `tool_envelope` — orchestration
- `budget_writer` — token-aware result builder with continuation sentinel
- `text_fmt` — EOL / encoding / BOM detector + normaliser
- `supervisor` — long-running process handles
- `ra_bridge` — trait `DiagnosticsHandle` (implemented daemon-side, like the existing `MemoryHandle`)

### New external crate

- `origin-lsp-client` — minimal stdio JSON-RPC LSP client used by the daemon's `DiagnosticsHandle` implementation to talk to rust-analyzer.

### Crates touched

- `origin-tools` (primary — new modules + rebuilt builtins + new builtins)
- `origin-daemon` (instantiate envelope, supervisor, and RA bridge; wire `chunk_tx` more generally)
- `origin-cli` (system-prompt regeneration to match new schemas; CLI surfacing of supervisor handles is out of scope for v2)

### Existing infrastructure that is preserved

- `dispatch::Cache` (input-keyed memoization with skiplist) — extended, not replaced; new output-CAS uses a parallel store keyed by result-body hash.
- `MEMOIZATION_SKIPLIST = ["Bash", "Edit", "Write"]` — preserved as-is for input-keyed cache; output-CAS skips Mutating tools by side_effects flag.
- `ToolMeta` + `inventory` registration — unchanged; envelope reads the existing meta.
- `MemoryHandle` trait pattern — `DiagnosticsHandle` mirrors it.

---

## Components

### 1. `text_fmt` — EOL/encoding/BOM normaliser

Single source of truth for "what does this file look like on disk". Used by every file-touching tool.

- `detect(bytes) -> Detected { eol: Eol, bom: Option<Bom>, encoding: Encoding, trailing_newline: bool }`
- `normalise_to_lf(bytes, detected) -> String` — produces canonical text the rest of the tool reasons about
- `denormalise(text, detected) -> Vec<u8>` — restores the file's original convention on write

**Novel mechanism:** mixed-EOL files (which exist in real repos, especially `.gitattributes` misconfigurations) keep a `Vec<EolKind>` indexed by **source-file line number** (one entry per `\n` boundary in the original bytes, in order) so `denormalise` restores per-line EOLs. Inserted lines inherit the EOL of the line immediately preceding the insertion point; deleted lines remove their entries. Source harnesses universally clobber mixed-EOL files to a single convention on write.

**Encodings supported:** UTF-8 (with and without BOM), UTF-16 LE/BE (BOM-detected only). Latin-1 / Windows-1252 detection deferred to v2.1 — for v2, non-UTF-8 without a BOM produces a structured `io.encoding` error rather than silent corruption.

### 2. `budget_writer` — token-aware result builder

Every tool builds its result by pushing into a `ResultWriter` initialised with a per-call token budget.

- Default budget: 25,000 tokens. Per-tool override via `ToolMeta::token_budget` (new field).
- Approximate token counting: `chars / 4` with a punctuation+whitespace hint. Exact tokenisation is too slow per call; we accept ±10% drift.
- On overflow, the writer emits a structured sentinel:
  ```json
  {
    "kind": "truncated",
    "emitted_tokens": 24987,
    "total_estimated": 91234,
    "continuation": { "tool": "Read", "args": { "file_path": "…", "offset": 1842 } }
  }
  ```
  The model receives a *resumable handle*, not a chopped string.

**Novel mechanism:** the writer tracks "elidable regions" before truncating useful content. Examples:
- `Read`: runs of blank lines (>3) elide to `<3 blank lines>`.
- `Grep`: matches whose path matches a noise pattern (e.g. lock files, generated code) elide before signal matches do.
- `Bash`: ANSI escape sequences strip to plain text; repeated identical lines collapse with a count.

Elision happens *first*; truncation with continuation handle only happens if elision still leaves us over budget. Source harnesses truncate by raw byte count.

### 3. `result_cas` — output-content dedup

Extends `dispatch::Cache` with a parallel store keyed by `blake3(serialised_result)`.

- After envelope builds the serialised result for any non-Mutating tool, hash it; store the body bytes in a session-scoped store; the agent emits `{tool_result_ref: "blake3:<hex>", bytes: N, preview: "<first 80 chars>"}` to itself for replay logging, but the actual `tool_result` block sent to the provider is the body bytes.
- Critically, **identical bodies serialise to byte-identical strings**. Anthropic's prompt cache hits when the same file is read twice in a turn — incremental token cost ~0 after the first read.
- The existing `MEMOIZATION_SKIPLIST` (Bash/Edit/Write) keeps working for input-keyed cache. Output-CAS additionally skips tools whose `SideEffects` is `Mutating`.

**Novel mechanism:** dedup at *output* granularity (not input). Combined with `budget_writer`'s deterministic serialisation, a `Read` followed by a `Grep` returning the same file slice serves both from one cache entry. Source harnesses dedup only at the request-key level (if at all).

### 4. `supervisor` — process handles

Long-running processes that the model launches with `Bash { run_in_background: true }`.

- `Supervisor::spawn(cmd, profile, timeout, log_cap) -> ProcessId` returns immediately with a `pid` handle.
- Ring-buffer (default 512 KiB) per process holds stdout and stderr interleaved. `Monitor { pid, since_byte }` tails it.
- Supports `kill(pid)`, `wait(pid)`, `signal(pid, sig)`, per-spawn `cwd`, per-spawn `env`.
- Timeout policy: SIGTERM → 2s grace → SIGKILL on unix; `TerminateProcess` on Windows.

**Novel mechanism:** ring-buffer indexed by "byte offset the model has already seen" rather than line count. Each `Monitor` call returns only-new bytes capped at the call's budget. No `tail -n` ambiguity, no duplicate bytes across polls. Combined with the budget writer, a long log stream is consumed in deterministic chunks.

### 5. `ra_bridge` — persistent rust-analyzer client

Trait `DiagnosticsHandle` in `origin-tools` (object-safe, mirrors `MemoryHandle`):

```rust
pub trait DiagnosticsHandle: Send + Sync + std::fmt::Debug {
    fn diagnostics(&self, path: Option<&Path>, severity: Severity)
        -> Result<Vec<Diagnostic>, DiagnosticsError>;
    fn notify_file_changed(&self, path: &Path, contents: &str);
}
```

`origin-daemon` provides the implementation, owning one long-lived rust-analyzer subprocess started lazily on first use and kept warm for the lifetime of the daemon process.

- First call: ~3-5s (RA initial workspace index).
- Subsequent calls: <100ms vs `cargo check`'s 10-30s on this workspace.
- Edit-class tools call `notify_file_changed` after every successful write, invalidating RA's in-memory file rather than the whole project graph.

**Novel mechanism:** RA stays warm across turns within a session. Coupled with origin's session model, the second turn of any session has zero cold-start cost for type queries. Source harnesses either don't expose LSP at all (most) or spawn a fresh RA per call.

**Distribution:** rust-analyzer is treated as a daemon-owned dependency, not a Cargo-build dependency (Cargo build scripts that download from the internet break offline builds and are widely discouraged). Two-tier resolution at daemon startup:

1. If `rust-analyzer` is on `PATH`, use it.
2. Else look in `$ORIGIN_CACHE/bin/rust-analyzer` (a per-user cache dir, e.g. `~/.cache/origin/bin` on linux/macOS, `%LOCALAPPDATA%\origin\bin` on Windows).
3. If still missing, the first `Diagnostics` call returns `subsystem.ra_unavailable` with a `hint` that includes the exact `origin daemon install-ra` CLI invocation to fetch and cache the platform-appropriate release into `$ORIGIN_CACHE/bin`. Release-channel binaries: shipped origin releases run `install-ra` once during postinstall so the typical user never sees this error.

This keeps `cargo build` hermetic, keeps the install footprint optional, and still gives users a "zero-setup" experience via the packaged release path.

---

## Data flow (one tool call, end-to-end)

Scenario: model calls `Edit { file_path: "crates/origin-cli/src/main.rs", old_string: "…", new_string: "…" }`.

1. **Daemon agent** receives the `tool_use` block, calls `dispatch::invoke("Edit", args)`.
2. **`tool_envelope::run`** takes over:
   - Loads `ToolMeta` (sandbox profile, side_effects, token budget).
   - **Input canonicalisation**: resolves `file_path` to absolute, canonicalises symlinks. `text_fmt::detect` reads the file once → `Detected { eol: Crlf, bom: None, encoding: Utf8 }`. Bytes normalised to LF in memory.
3. **Output-CAS check**: side_effects=Mutating → skipped. (Read/Grep/Glob/Diagnostics check here and may short-circuit with a `{tool_result_ref}`.)
4. **`edit_tool_v2`** runs against the LF-normalised text:
   - Counts matches of `old_string` (also LF-normalised) in normalised content.
   - 0 or >1 → returns structured error `{kind: "edit.no_match" | "edit.ambiguous", suggestions: [...]}` (envelope serialises).
   - 1 → splices; re-serialises through `text_fmt::denormalise` so the file goes back as CRLF byte-for-byte.
   - Atomic write: writes to `<path>.<pid>.tmp`, fsyncs, renames. Hard-link fallback on Windows when ACLs forbid rename-replace.
5. **`ResultWriter`** (budget = 4k tokens for Edit since its result is small):
   - Emits `{ok: true, hunks: [{file, before: "…", after: "…", line: 42}]}` — only the changed region, not the whole file.
   - If hunk > budget, elides middle of `before`/`after`.
6. **Envelope** records to `dispatch::Cache` (input-keyed) so the agent can show "(cached from turn N)" on repeat.
7. **Daemon** serialises envelope output into a `tool_result` block.
8. **`notify_file_changed`** fired to `DiagnosticsHandle` so a follow-up `Diagnostics` call gets re-analyzed results in <100ms.

**Streaming:** for fast tools (Edit, Read of small files) no streaming. For Bash/Grep/Diagnostics the same envelope drives chunks into the existing `chunk_tx` mpsc that `bash_tool_streaming` already uses, generalised to all tools.

**Cross-call lifecycle:**
- **Same session, same Read twice**: 2nd call → output-CAS hit → ~0 incremental tokens (prompt cache hits the body).
- **Bash launching a dev server**: returns immediately with `{pid, status: "started"}`. Subsequent `Monitor { pid, since_byte: N }` calls stream new output. Model can interleave other work.
- **Diagnostics after Edit**: Edit invalidates RA's in-memory file → next `Diagnostics` re-analyses incrementally in ~50ms.

---

## Tools

### Rebuilds (replace existing)

| Tool | Params | Notes |
|------|--------|-------|
| **`Read`** | `file_path, offset?, limit?, as?` | Line-numbered (`cat -n`) chunks. `as: image\|pdf\|text`. Default `limit=1000` lines (chosen to fit comfortably under the 25k default token budget at typical line widths; users can pass higher and the budget writer will elide/continue). Replaces `read_tool` (unbounded full-file). |
| **`Edit`** | `file_path, old_string, new_string, replace_all?` | CRLF-safe via `text_fmt`. Returns changed hunk only. `replace_all=true` permits multi-match. |
| **`Write`** | `file_path, content, force?` | Atomic write. Refuses overwrite of a file not Read this session unless `force=true`. Preserves prior EOL convention. |
| **`Grep`** | `pattern, path?, glob?, type?, output_mode?, head_limit?, -A?, -B?, -C?, -n?, multiline?` | `output_mode` default `files_with_matches`. `type` uses rg's type system. |
| **`Bash`** | `command, timeout?, cwd?, run_in_background?, env?` | Default timeout 120s, max 600s. `run_in_background=true` returns `{pid, status: "started"}`. `cwd` default = the daemon process's launch cwd (the directory `origin-daemon` was started from, which the CLI sets to the project root). |

### Net-new

| Tool | Params | Notes |
|------|--------|-------|
| **`MultiEdit`** | `file_path, edits: [{old, new, replace_all?}]` | Sequential, atomic per file. Single read + single write per call. |
| **`Glob`** | `pattern, path?, head_limit?` | Rebuild of `glob_tool`. Returns matches sorted by mtime DESC. `ignore::WalkBuilder` for gitignore awareness. |
| **`ApplyPatch`** | `patch` | Standard unified diff. Validates context lines; applies atomically across files; reports per-hunk outcome. |
| **`Monitor`** | `pid, since_byte?, max_bytes?, wait?` | Tails supervisor ring-buffer. `wait=true` long-polls up to 2s for new bytes. |
| **`Diagnostics`** | `path?, severity?` | LSP diagnostics from warm RA. Severity filter: `error\|warning\|hint`. |
| **`ToolSearch`** | `query, max_results?` | Lazy schema loading. Only the 11 hot tools are always-loaded in the system prompt; graph/web/browser/recall/mem/ask/task tools have name+1-line description loaded, full schema fetched on demand. |

### Retained as-is

`ask`, `mem`, `recall`, `task`, `web_fetch`, `web_search`, `browser`, `graph_query`, `graph_path`, `graph_explain`, `graph_summarize`, `graph_rebuild`. These work today and aren't on a hot path; they inherit envelope wins automatically.

---

## Error taxonomy

One enum at the envelope edge, serialised as `{kind: "<class>.<reason>", message, recoverable: bool, hint?}`.

| Class | Reasons |
|-------|---------|
| `io` | `not_found`, `permission`, `is_directory`, `not_utf8`, `encoding` |
| `edit` | `no_match`, `ambiguous` (with nearest-N matches), `read_required` |
| `bash` | `spawn_failed`, `timeout`, `killed` |
| `regex` | `invalid` (with column pointer) |
| `budget` | `truncated` (with continuation handle — not an error per se, uses the same envelope) |
| `subsystem` | `ra_unavailable`, `memory_unavailable` |
| `validation` | schema violations |

Model benefit: structured `kind` lets the agent loop pattern-match recoverable errors (`edit.ambiguous` → re-issue with more context; `budget.truncated` → call continuation) without LLM re-parsing prose.

---

## Testing strategy

This work is to be executed under `/test-driven-development`. Every component and tool lands with tests written first.

- **Per-tool unit tests** for every rebuilt and new tool. Existing test patterns in `crates/origin-tools/src/builtins/*` carry over.
- **CRLF regression suite** (canary for the screenshot bug): fixtures with LF, CRLF, CR, mixed line endings. `Edit`, `MultiEdit`, `Write`, `ApplyPatch` must succeed against all four and preserve the original convention byte-for-byte on write.
- **Encoding fixtures**: UTF-8 BOM, UTF-16 LE, UTF-16 BE, ambiguous Latin-1. Must either succeed or fail with a clear `io.encoding` error — never silent corruption.
- **Budget property tests** (proptest): synthesise outputs at 0.5×, 1×, 2×, 10× budget. Emitted bytes always within budget. Continuation handle always advances by `emitted_tokens`.
- **Supervisor tests**: timeout fires SIGTERM then SIGKILL. Kill is observable via Monitor. Ring-buffer wraps cleanly under load. Two parallel processes don't cross-contaminate buffers.
- **RA bridge integration tests**: ignored by default (require `rust-analyzer` binary present); CI runs nightly with the vendored binary.
- **KPI benchmark**: 10-task fixed corpus run before and after the migration to measure the four KPI targets.

---

## Rollout

Single PR per phase to keep diffs reviewable.

1. **Foundation**: `text_fmt`, `budget_writer`, envelope skeleton. No behaviour change yet — envelope is a passthrough.
2. **Rebuild file/shell tools**: `Read`, `Edit`, `Write`, `Grep`, `Glob`, `Bash` routed through envelope. Schemas change atomically with the system prompt.
3. **New tools**: `MultiEdit`, `ApplyPatch`, `Monitor`, `Diagnostics`, `ToolSearch`.
4. **Daemon wiring**: supervisor instance, RA bridge, deferred-tool serving for `ToolSearch`, `origin-lsp-client` crate.
5. **Cleanup**: remove deprecated functions and the old `bash_tool_streaming` path (replaced by the generalised envelope channel).

Each phase ships only after all of its tests pass. Each phase is independently revertable.

---

## Execution directives

Per user directive at brainstorming time:

- **Implementation plan generation** uses `/writing-plans` against this spec.
- **Plan execution** uses `/dispatching-parallel-agents`: phases are sequential by dependency, but tasks within a phase that touch independent files run in parallel (e.g. the five rebuilt tools in Phase 2 are independent of each other).
- **Each task implements TDD** per `/test-driven-development`: tests written first, then implementation, then refactor.
- **Each task closes with `/verification-before-completion`**: tests pass, `cargo clippy --workspace -- -D warnings` clean, `cargo fmt --check` clean. No claim of completion without observed evidence.

---

## Acceptance criteria

- All four KPI targets met or exceeded on the benchmark corpus.
- CRLF regression suite passes on Windows CI.
- `cargo clippy --workspace -- -D warnings` is clean.
- All existing `origin-tools` tests pass; new tests added per the testing strategy pass.
- System prompt size (token count of all always-loaded tool schemas) is reduced by ≥30% via `ToolSearch` deferral.
- The exact failure from the original screenshot (`Edit` on CRLF main.rs with LF needle) produces a successful edit in a reproduction test.

---

## Out of scope (deferred to a follow-up)

- Latin-1 / Windows-1252 encoding detection (only UTF-8 + BOM-detected UTF-16 in v2).
- MCP-discovered tool reshaping (continues to work unchanged through `DynTool`).
- CLI surfacing of supervisor process list (model can `Monitor` by pid; user-facing list is v2.1).
- A `Notebook`/Jupyter editor analogous to Claude Code's `NotebookEdit`. Origin doesn't currently target notebook workflows.
- Tool-call cost telemetry surfaced to the user.
