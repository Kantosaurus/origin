# Changelog

All notable changes to `origin` will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely;
versions correspond to phase milestones from the implementation plan.

## [phase-1] — 2026-05-19

### Added

- **`origin-provider`** — canonical `Provider` trait + `ChatRequest`/`ChatResponse`/`Usage`/`ToolSchema`/`ProviderError`.
- **`origin-provider-anthropic`** — non-streaming `POST /v1/messages` implementation with wiremock-driven tests; auth-error → `ProviderError::Auth`; rate-limit → `ProviderError::RateLimit`.
- **`origin-tools`** — inventory-backed compile-time tool registry, `origin_tool!` macro, and five builtin tools:
  - `Read` (AutoAllowed)
  - `Glob` (AutoAllowed)
  - `Grep` (AutoAllowed; recursive)
  - `Edit` (RequiresPermission/Medium; errors on missing or ambiguous old_string)
  - `Bash` (RequiresPermission/High; PowerShell 7+ on Windows with powershell.exe fallback)
- **`origin-permission`** — tier-based check with pluggable `Prompter` trait; `AlwaysAllow`/`AlwaysDeny` test prompters.
- **`origin-daemon`** — `Session`, `run_loop`, `SessionStore`. Agent loop:
  1. Append user message.
  2. Call provider with current messages + tool schemas.
  3. Dispatch each `tool_use` block through permission + registry.
  4. Append `tool_result` blocks as a `Role::Tool` message.
  5. Repeat until no `tool_use`; cap at 25 turns.
  Sessions persisted to SQLite (rkyv-archived `Message` blobs in `body_inline`).
- **`origin-cli`** — Ratatui baseline TUI with scrollback + prompt input; Ctrl-C / Esc to quit.

### Dogfood result

- SKIPPED — `ANTHROPIC_API_KEY` not set in environment; `anthropic_smoke.rs` test exited successfully with "skipping live_smoke" message. Agent-loop integration tests (P1.10) already verify the loop wires together correctly with a scripted provider.

### Test coverage at phase exit

- 48 tests passing across the workspace.

### Known limitations (deferred)

- Non-streaming only (streaming arrives in P2 with the byte ring buffer).
- AlwaysAllow prompter — interactive prompts move to the TUI side panel in P4.
- Sessions stored inline in `body_inline` blobs (CAS arrives in P2).
- API key read from `ANTHROPIC_API_KEY` env var; KeyVault with OS keychain arrives in P8.

### MSRV-pinned transitive dependencies (Cargo.lock)

Across P1 several transitive deps needed precise version pins to stay compatible with Rust 1.83:

- `url` 2.5.0 (avoid `idna_adapter` 1.2.2 which uses edition2024)
- `wiremock` 0.6.2 (0.6.5 uses edition2024)
- `hyper-rustls` 0.27.3 (0.27.9 requires Rust 1.85)
- `grep-matcher` 0.1.7 / `grep-regex` 0.1.12 / `grep-searcher` 0.1.14 (later versions require edition2024)
- `darling` 0.20.11, `instability` 0.3.5, `unicode-segmentation` 1.12.0 (Ratatui transitive deps)

Aggregate pin count now warrants re-evaluating the MSRV bump policy.

## [phase-0] — 2026-05-19

### Added

- **Workspace scaffold** — Cargo workspace at `crates/*`, Rust 1.83.0 toolchain pin,
  workspace lint policy (`unsafe_code = forbid`, `unwrap_used = deny`,
  `clippy::pedantic + nursery` warns).
- **Cross-platform CI** — `.github/workflows/ci.yml` running fmt + clippy + test on
  Ubuntu, macOS, and Windows.
- **`origin-core`** — Pure type crate.
  - `Role` (User / Assistant / Tool / System) with rkyv archive + check_bytes.
  - `MessageId` (ULID wrapper) and `TurnIndex` (overflow-safe `checked_add`).
  - `Block` enum (Text / ToolUse / ToolResult / Thinking) with optional cache markers.
  - `Message` (role + Vec<Block>).
  - `ProviderCaps` IR scaffold (const-constructible).
- **`origin-ipc`** — IPC transport crate.
  - Wire frame with magic, 17-byte header, length-prefixed body, validation against
    truncation / bad magic / unknown kind / length mismatch.
  - Cross-platform local-socket transport (Unix domain sockets / Windows named pipes)
    via `interprocess` crate.
- **`origin-store`** — SQLite persistence with `refinery` migrations.
  - WAL mode, V1 schema for `sessions` + `messages`.
- **`origin-daemon`** + **`origin-cli`** (binary `origin`) — Echo handshake over the
  IPC transport, verified end-to-end on Windows.

### Architecture milestones reached

- Daemon process, IPC transport, and client binary separated by crate boundary —
  same code shape will support multi-client and future remote/desktop surfaces.
- All types are `rkyv::Archive` from day 1 so the same bytes can flow through IPC,
  storage, and in-memory ring buffers without serialize/deserialize hops.
- 18 tests passing across the workspace; property tests for frame encoding.
