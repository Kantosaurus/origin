# Changelog

All notable changes to `origin` will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely;
versions correspond to phase milestones from the implementation plan.

## Phase 3 — CachePlanner + Speculative Dispatch + Recall (2026-05-19)

- New `origin-planner` crate: `Band` enum (Frozen/Sticky/Sliding/Volatile),
  `PrefixLedger` stability scoring with promote/demote thresholds,
  `CachePlanner::plan` four-band sort + boundary marker indices,
  `WireDecision::for_block` inline-vs-reference rule.
- `origin-provider-anthropic` emits `cache_control: ephemeral` at planned
  band boundaries; consults `WireDecision` to inline small handles or
  emit `<result handle:XXXXXXXX — N bytes>` references for large ones.
- New `origin-daemon::tool_use_parser` — SAX-style incremental JSON parser
  yielding `Field` events before the streaming `tool_use` block closes.
- Speculative dispatch: agent forks pure-tool tasks on the parser's first
  complete field; side-effecting tools (`Bash`, `Edit`, `Write`, MCP
  writes) stay sequential. New `TokenKind::ToolUseStart` event surfaces
  the assistant's tool_use block-start to the agent loop.
- New `Recall` builtin: inflates a CAS handle with optional Lines /
  Match / OutlineOnly region selector. Threaded `Option<Arc<Store>>`
  through the dispatch path.
- Session-scope memoization: `(tool_name, raw_input_bytes)` → blake3
  hash → CAS handle; cached results annotated `(cached from turn N)`.
  `Bash`/`Edit`/`Write` opt out via `MEMOIZATION_SKIPLIST`.
- `phase3_cache_warm_ratio` checkpoint test: 20-turn synthetic workload,
  warm pass asserts `cache_read_input_tokens > 0.5 × input_tokens`.

### Known limitations (deferred)

- Section→wire-block index for `cache_control` marker placement is a
  stub (always Volatile); full plumbing lands with N4.3 encoder codegen
  in Phase 11.
- Multi-tool-use concurrent block delta routing uses "most-recent
  parser" instead of Anthropic's `index` field; sequential tool_use
  works correctly. Full index routing lands with concurrent-tool
  support in Phase 7.
- `OutlineOnly` region selector returns a placeholder; the sidecar
  coroutine that emits structure summaries lands in Phase 6.
- Tool-input normalization is byte-equivalent only; canonical-form
  normalization (path canon, regex parse-equivalence) lands with
  N10.4 in Phase 10.
- `cargo fuzz` target for `tool_use_parser` is stubbed; full corpus +
  CI fuzz cycle lands with N10.10 in Phase 14.

### Test coverage at phase exit

- All previous tests still passing.
- New: 8 `origin-planner` tests (3 ledger + 2 planner + 3 decision).
- New: 6 `origin-daemon` tests (3 + 1 prop tool_use_parser, 1
  speculative_e2e, 1 memoization_e2e, 1 phase3_cache_warm_ratio).
- New: 3 `origin-tools` recall tests + 3 memoization tests.
- New: 1 Anthropic `cache_markers` test + 1 `handle_substitution` test.

## Phase 2 — Streaming + CAS + Ring Buffer (2026-05-19)

- `origin-cas` crate: blake3 Hash, FastCDC chunker, mmap pack files,
  three-tier (Hot LRU + Warm mmap + Cold zstd) Store, refcount/GC enumerator.
- `origin-stream` crate: single-producer multi-tail byte ring;
  rkyv-archived TokenEvent records.
- Anthropic provider gains native SSE → ring streaming.
- `Provider::chat_stream` added to trait; default impl wraps `chat`.
- Daemon: tool outputs land in CAS; `Block::ToolResult` carries 32-byte
  handle, not inline bytes. `expand_messages_for_wire` re-inflates on the
  outbound encode hop.
- CLI: live token streaming infrastructure into TUI; assistant turn buffer;
  token/cost status bar driven by Usage events.
- RAM soak: 1000-msg session, ΔRSS < 200 MiB.
- New SQLite migration V2: `cas_refs(hash, refcount, tier, last_access)`.

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
