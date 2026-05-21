# Changelog

All notable changes to `origin` will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely;
versions correspond to phase milestones from the implementation plan.

## Unreleased

### Added
- **Superpowers skills bundled** (`origin-skills`): 14 official superpowers skills (brainstorming, TDD, debugging, plans, code review, worktrees, etc.) embedded in the binary via `include_dir!`. Override per skill at `~/.origin/skills/<name>/SKILL.md`. New `load_all(user_root)` merger lets user copies replace embedded ones by name.
- **`origin-browser` crate (new)**: dual-backend browser tool. `BrowserRouter` runs `agent-browser` as primary; on bot-detection (Cloudflare, reCAPTCHA, hCaptcha, PerimeterX, DataDome, Incapsula, Kasada, 4xx) transparently replays the verb against the vendored `CloakBrowser` sidecar and sticks to Cloak after two consecutive successes per session. Includes pure-Rust `WebFetch` (reqwest + readability + html2md) and `WebSearch` via Tavily (`TAVILY_API_KEY`).
- **Vendored CloakBrowser sidecar** (`vendor/cloak-browser/`): Node ≥18 stdio-JSON CLI (`cloak-cli.mjs`) wire-compatible with `agent-browser`'s snapshot/ref protocol so the router can swap backends mid-session.
- **`origin-tools` builtins**: `WebFetch`, `WebSearch`, `Browser` (all `Tier::RequiresPermission`).

## 1.0.0 — 2026-06-17

### Added
- **Replay infrastructure** (`origin-replay`): `.origin-replay` bundle format (zstd-tar with manifest), `Recorder` trait with `Null`/`File` backends, `Frame` enum covering provider/IPC/CAS/clock/RNG events, virtual clock, seeded SplitMix64 RNG, opt-in `recorder` feature on `origin-provider`, `origin-ipc`, `origin-cas` via static `recorder_hook` registries.
- **Fuzz CI** (`.github/workflows/fuzz.yml`): nightly 5-target × 5-min cargo-fuzz matrix covering `ipc_frame` validator, `fastcdc_boundary` chunker, `anthropic_stream` + `openai_stream` SSE parsers, `streaming_json` rkyv decoder.
- **Migration** (`origin-migrate`, `origin import`): adapters for Claude Code (jsonl + SKILL.md), jcode (rusqlite reader), opencode (storage/*.json); idempotent content-hash dedupe via new `Store::{contains,insert}_migrated_{session,skill}` + V6 SQLite migration; `--dry-run` / `--apply` / `--json` modes.
- **Benchmarks** (`origin-bench`): 8-task fixed set, origin + generic subprocess runners, Markdown + JSON reports.
- **Docs site** (`docs/site/`): 11-chapter mdBook (intro/quickstart/architecture/configuration/providers/skills/hooks/mcp/migration/sdk/troubleshooting); `origin --tutorial` 7-step guided tour; clap_mangen manpages via `xtask manpages`.
- **Release engineering** (`.github/workflows/release.yml`): 6-target matrix build (musl x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64/aarch64) with cosign keyless signing + SLSA build-provenance attestation; packaging templates for Homebrew (`packaging/homebrew/origin.rb.tmpl`), winget, AUR, cargo-binstall metadata; `xtask release` stamps `{{VERSION}}` + `{{SHA256_*}}` placeholders.

### Gates
- Perf gate workflow asserts read-only task `wall_ms` worst ≤ 80 ms.
- Unsafe-audit workflow asserts `unsafe` only in `origin-cas`, `origin-tui`, `origin-ipc`.
- Security review signoff doc (`docs/security/p14-security-review.md`) for sandbox + KeyVault.
- Three migration paths validated by `crates/origin-migrate/tests/three_paths.rs`.

### Spec criteria
1. Deterministic replay + fuzz suite green: ✅ (`origin-replay`, `.github/workflows/fuzz.yml`).
2. Perf gates: ✅ (`.github/workflows/perf-gate.yml`).
3. Zero-unsafe in surface crates: ✅ (`.github/workflows/unsafe-audit.yml`).
4. Sandbox + KeyVault review: ⏳ (signoff doc landed; reviewer signature pending).
5. Three migration paths validated: ✅ (`tests/three_paths.rs`).

### Known follow-ups (out of GA scope)
- A.9–A.11 fuzz targets compile only under nightly Rust due to `anndists`/`ort-sys` transitive deps requiring `edition2024`; the GA build uses Rust 1.83 (MSRV) and the fuzz crate is excluded from the workspace. CI workflow runs against nightly.
- Apply-mode of `origin import` (vs dry-run) currently returns the same content-hash summary; persistent write through `apply_with_store` requires a CLI Store handle to be threaded — slated for a 1.0.x patch.

## Phase 13 — QUIC Remote IPC + Headless Polish (2026-05-20)

- New `origin-ipc::quic` transport: `QuicListener` / `QuicConnector` /
  `QuicConnection` over `quinn` + `rustls`. Identical wire framing to the
  local-socket transport so daemon dispatch is transport-agnostic. The
  `QuicConnection::Drop` impl detaches the underlying `quinn::Connection`
  onto a tokio task that awaits `Connection::closed()` so in-flight
  stream writes flush before teardown.
- New `origin-ipc::tls`: self-signed Ed25519 cert generation + SHA-256
  fingerprint helper. Peers pin by fingerprint; no PKI.
- New `origin-daemon::pairing`: 6-digit single-use pairing codes with
  TTL, bearer-token minting (`orb_` prefix, 24-byte random suffix),
  in-memory `BearerStore`, KeyVault persistence under
  `("origin-remote", <device>)`.
- Daemon IPC additions: `PairStart`, `PairRedeem`, `ListSessions`,
  `ResumeSession`, `RemoveSession`, `GetUsage`, `KeyringAdd`,
  `KeyringList`, `KeyringRemove` plus matching `StreamEvent`s
  (`PairCode`, `PairIssued`, `PairError`, `SessionsListed`,
  `UsageReport`, `KeyringAccounts`, `AdminOk`, `AdminError`).
- Daemon session_store: `list_summaries()` (id, created_at, title,
  model, message_count) and `delete(session_id)`. `Session.id` is now a
  `String` (was `MessageId`) to accept admin-supplied ids.
- New CLI subcommands:
  - `origin pair {start,redeem}` — pair a remote client; redeem opens a
    QUIC connection and prints the issued bearer.
  - `origin run [--json] [--remote <url>] [--bearer <t>] [--model <m>]
    <text>` — headless one-shot prompt. `--json` emits JSON-Lines per
    IPC event; default mode prints `text_delta` payloads concatenated.
    `--remote origin://host:port#fingerprint` routes through QUIC
    (CA loaded via `ORIGIN_REMOTE_CA_DER_FILE`).
  - `origin usage` — per-provider/per-model token totals from the
    metrics snapshot.
  - `origin sessions {ls,resume,rm}` — list/resume/remove persisted
    sessions.
  - `origin keyring {add,list,remove}` — manage KeyVault credentials;
    `add` reads the secret from stdin when the value is `-`.
- Workspace deps pinned: `quinn = "0.11"`, `rustls = "0.23"`,
  `rcgen = "0.13"`, `rustls-pemfile = "2"`, `x509-parser = "0.16"`,
  `sha2 = "0.10"`, `hex = "0.4"`. `origin-daemon` gains `rand = "0.8"`;
  `origin-cli` gains `hostname = "0.4"` + `url = "2"`.

### Test coverage at phase exit
- `origin-ipc`: 2 tls tests, 1 quic_smoke, 1 quic_concurrent.
- `origin-daemon`: 4 pairing_unit, 4 protocol_pair, 2 pairing_e2e,
  3 admin_ipc, 2 session_store_list, plus regressions exercised in
  the existing `account_switch` suite (extended for new variants).
- `origin-cli`: 1 run_help, 2 headless_stream, 3 pair_cli (incl. 2
  admin_url parsing tests), 3 admin_cli (--help surfaces),
  1 admin_e2e (fake daemon → sessions ls).

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
