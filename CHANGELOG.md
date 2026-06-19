# Changelog

All notable changes to `origin` will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) loosely;
versions correspond to phase milestones from the implementation plan.

## Unreleased

### Added
- **More package managers**: the release pipeline now publishes to **Homebrew** (tap `kantosaurus/origin`), **Scoop** (bucket `Kantosaurus/scoop-origin`), **Chocolatey**, **winget**, **AUR** (`origin-bin`), and a **Nix flake** (`nix profile install github:Kantosaurus/origin`), plus **cargo-binstall** metadata (`cargo binstall --git … origin-cli`). Each is a separate, non-blocking `release.yml` job that gates on its secret and skips cleanly until provisioned; `xtask release` stamps the new Scoop/Nix/Chocolatey manifests from the same per-target SHA-256 set as the existing Homebrew/winget/AUR templates. Setup (repos + secrets) is documented in [`docs/operations/package-managers.md`](docs/operations/package-managers.md).
- **Swarm focus view (TUI)**: while a swarm wave runs, **Tab** cycles a `❯` highlight through the sub-agents in the side panel and **Enter** drills the main pane into the selected agent's live full transcript (`⎇ viewing …` — assistant prose + tools streamed per agent); **Enter** again (or Tab + Enter) toggles back to the main origin conversation. The daemon streams each watched worker's output (`StreamEvent::SwarmAgentOutput`); the worker's assistant text is relayed via a per-worker ring subscriber (`stream_relay::relay_to_progress`).
- **Non-blocking background swarm**: `Task` now dispatches in the **background by default** (`background: true`) — the turn returns immediately so the main agent stays responsive while sub-agents run. Finished results are delivered automatically at the start of the next turn (a `<background-results>` prose block) or on demand via the new **`CollectTasks`** tool. Backed by a daemon-wide session-keyed `BackgroundJobs` registry + non-blocking `Coordinator::try_completion`/`evict`. Per-session cap `ORIGIN_SWARM_BG_MAX` (default 32, overflow falls back to blocking); global off-switch `ORIGIN_SWARM_BG=0`; per-call `background:false`.
- **`CollectTasks` builtin** (hot, `Tier::AutoAllowed`): gather finished background sub-agents in-turn + list those still running.
- **Unlimited swarm sub-agents by default**: the memory-`AdmissionGate` and runtime `Swarm` lane no longer impose a small fixed cap (Windows was capped at 3); the memory governor remains the real limiter on Linux. Re-cap with `ORIGIN_SWARM_MAX` / `ORIGIN_SWARM_LANE_MAX`.
- **Superpowers skills bundled** (`origin-skills`): 19 official superpowers skills embedded in the binary via `include_dir!` — workflow (brainstorming, writing/executing plans, goal), engineering discipline (TDD, systematic debugging, verification-before-completion), collaboration (requesting/receiving code review, subagent- & parallel-agent dispatch, worktrees, finishing a branch), and review/release (reviewing-security, investigating-performance-regressions, managing-dependencies, writing-commits-and-prs, writing-skills, using-superpowers). Override per skill at `~/.origin/skills/<name>/SKILL.md`. New `load_all(user_root)` merger lets user copies replace embedded ones by name.
- **`origin-browser` crate (new)**: dual-backend browser tool. `BrowserRouter` runs `agent-browser` as primary; on bot-detection (Cloudflare, reCAPTCHA, hCaptcha, PerimeterX, DataDome, Incapsula, Kasada, 4xx) transparently replays the verb against the vendored `CloakBrowser` sidecar and sticks to Cloak after two consecutive successes per session. Includes pure-Rust `WebFetch` (reqwest + readability + html2md) and `WebSearch` via Tavily (`TAVILY_API_KEY`).
- **Vendored CloakBrowser sidecar** (`vendor/cloak-browser/`): Node ≥18 stdio-JSON CLI (`cloak-cli.mjs`) wire-compatible with `agent-browser`'s snapshot/ref protocol so the router can swap backends mid-session.
- **`origin-tools` builtins**: `WebFetch`, `WebSearch`, `Browser` (all `Tier::RequiresPermission`).
- **Crate metadata**: every workspace crate now carries a `description` for crates.io; internal crates (`origin-bench`) marked `publish = false`; workspace `homepage` set.
- **Supply-chain CI** (`.github/workflows/audit.yml`): `cargo deny` gates RUSTSEC advisories, dependency bans (rustls-only TLS), and a crates.io-only source allow-list, on push/PR and a daily schedule. New root `deny.toml`.
- **Community health**: issue templates (`bug_report`, `feature_request`, `config`), `PULL_REQUEST_TEMPLATE.md`, `CODEOWNERS`, `.github/dependabot.yml` (cargo + github-actions + npm), and a root `.editorconfig`.
- **Coverage**: `ci.yml` now runs `cargo llvm-cov` and uploads an `lcov` artifact; added a stable-toolchain test lane alongside MSRV.

### Changed
- **Startup banner** is now the big block-letter `ORIGIN` wordmark **centered** in the viewport (was a small top-left wordmark); shown before the first prompt, then cleared for the compact top-chrome header.
- **Status readout never vanishes under the swarm panel**: the `model · ↑in ↓out · $cost` metrics are now the priority readout (they reserve their slot first and the phase clips to their left), so a narrowed status zone + the `⎇ N/M agents` phase can no longer push them off the row.
- **Docs** restructured into a plain-Markdown `docs/` tree (`architecture/`, `crates/`, `guides/`, `operations/`, `reference/`, `subsystems/`, `security/`); the mdbook site (`docs/site`) and its GitHub Pages publish were retired (the `Docs` CI now just validates manpages).
- **Canonical repo owner** normalized to `Kantosaurus/origin` across `Cargo.toml`, packaging templates (Homebrew/winget/AUR), the docs site, and quickstart (was inconsistently `wooainsley/origin`).
- **Linux packaging** now targets glibc/gnu (matching the release build) instead of musl in the Homebrew/AUR templates and the `xtask release` stamper.
- **CI hardening**: all GitHub Actions pinned to commit SHAs; least-privilege `permissions:`, `concurrency`, and `timeout-minutes` on every workflow; `--locked` builds/tests; docs site now deploys from `dev`. Added the `tool_use_parser` fuzz target to the nightly matrix.
- **npm packages** now ship `LICENSE` (and `NOTICE` in the main package) per Apache-2.0 §4; the vendored CloakBrowser sidecar is pinned to an immutable commit and documents its third-party deps (`vendor/cloak-browser/THIRD_PARTY.md`).
- **Docs**: the SDK guide's "Minimal Rust client" now reflects the real `origin-ipc` frame API instead of an unimplemented facade.
- **SPDX headers** added to all first-party `.rs` files; `REUSE.toml` + `LICENSES/Apache-2.0.txt` for REUSE compliance.
- **Community/security**: `GOVERNANCE.md`, `ROADMAP.md`, `SUPPORT.md`, an OpenSSF Scorecard workflow, Codecov upload, and an SBOM step on release; committed fuzz seed corpora for all six targets; winget manifests split into version/installer/locale.

### Fixed
- **Background sub-agents stayed `▸ running` in the swarm panel forever after finishing**. With the non-blocking background swarm (`Task { background: true }` by default), a dispatched worker streams a `"spawned"` `SwarmWorker` event but the turn returns without awaiting it — so, unlike the foreground path, no `"completed"`/`"failed"` event was ever emitted, and its panel row (and the `⎇ N/M agents` readout) showed it running indefinitely. The daemon now emits a terminal `SwarmWorker` event for each background job at the moment it is reaped — next-turn `<background-results>` delivery and the `CollectTasks` tool — keyed by the same stable worker id, so the row flips to `✔ completed` (or `✘ failed`, with the reason in `detail`). `TaskOutput.status == "completed"` maps to completed; every other terminal status reads as failed.
- **First-run discovery prompt no longer freezes the TUI when the agent asks an interactive question**. The auto-fired first-run prompt was `await`ed inline **before** the keyboard event loop started, so if that turn raised an `ask_user` / `AskUserQuestion` picker (the default brainstorming workflow actively drives one — typically a **multi-select** question), the picker rendered but every keystroke (↑/↓ navigation, space to toggle, Enter to confirm) was dropped, and the turn deadlocked because no `ChoiceDecision` could ever be sent. The first-run prompt now dispatches through the same non-blocking `spawn_prompt_turn` path as a hand-typed message, so the input loop is live from the first frame and the picker is fully responsive. The `interrupt_tx` slot is lifted into `run` and shared, so Ctrl+C also reaches the first-run turn.
- **Can't get back to the main agent after drilling into a sub-agent (swarm focus view)**. With 2+ sub-agents, the focus-view footer invites **Tab/Enter ↩ back**, but Tab moved the panel's *highlight* cursor onto a different agent while `swarm_toggle_focus` keyed its exit off the *highlighted* agent — so after a Tab, Enter compared the viewed id against the now-different selected id and **switched to viewing that other agent instead of exiting**, leaving the user hopping between sub-agents with no way back to the main origin view. `swarm_toggle_focus` now always returns to main whenever a sub-agent is focused (it keys off `swarm_view`, not the selection), Tab while focused also returns to main (matching the footer), and the `main.rs` Enter/Tab gate fires on `swarm_view || swarm_selected` so a focused user can always exit even if the highlight was cleared.
- **Prompt-cache 400 (`A maximum of 4 blocks with cache_control may be provided. Found 5`)** that killed long agentic turns. The agent loop's index-based per-turn cache breakpoints (`dynamic_message_markers`) were computed against `session.messages` but consumed by the Anthropic wire encoder against the post-`snapshot`/`expand`/`strip_orphan_tool_results` list; once a stranded/orphaned tool turn was stripped, those indices drifted onto extra blocks and the request exceeded Anthropic's 4-marker ceiling. The block-carried `cache_marker` (which rides with its block through every transform) is now the single source of truth, and the wire encoder hard-caps the request at 4 markers (keeping the latest, most cache-valuable ones) instead of merely warning. Adds `tests/cache_marker_ceiling.rs`.
- **`ask_user` / `AskUserQuestion` interactive picker now works in a normal TUI session**. It was gated on `permission_ask` (the `/permissions on` opt-in, default off), so the agent's interactive questions silently degraded to a "interactive prompting is not available" prose fallback. Decoupled via a per-request `interactive` flag the TUI always sets; the daemon now wires the choice registry on `interactive || permission_ask`.
- **Self-update fully lands on Windows**: applying a staged update now force-cycles the running daemon + supervisor onto the freshly-swapped binaries (instead of relying on a flaky mtime heuristic that left the daemon frozen at the old version). No more manual process-killing after an update.
- **Focused sub-agent transcript streams the agent's assistant prose**, not just its tool calls: the worker's token stream is relayed via a per-worker ring subscriber (`stream_relay::relay_to_progress`); previously assistant text never reached the focus view because workers set `event_tx` but not `relay_tx`.
- **Skill selector now scrolls through all skills**: pressing `/` previously truncated the suggestion list to the first 6 matches (alphabetically), so skills beyond that were unreachable. The full match list is now retained and the popup renders a scrolling window over it — arrowing up/down moves through every match, with `↑`/`↓` hints on the edge rows when there's more off-screen.

### Removed
- Internal `docs/superpowers/` planning and spec docs, and `docs/why-origin.md`, are no longer published in the repository; references to them were removed.

## 1.0.0 — 2026-06-17

### Added
- **Replay infrastructure** (`origin-replay`): `.origin-replay` bundle format (zstd-tar with manifest), `Recorder` trait with `Null`/`File` backends, `Frame` enum covering provider/IPC/CAS/clock/RNG events, virtual clock, seeded SplitMix64 RNG, opt-in `recorder` feature on `origin-provider`, `origin-ipc`, `origin-cas` via static `recorder_hook` registries.
- **Fuzz CI** (`.github/workflows/fuzz.yml`): nightly 5-target × 5-min cargo-fuzz matrix covering `ipc_frame` validator, `fastcdc_boundary` chunker, `anthropic_stream` + `openai_stream` SSE parsers, `streaming_json` rkyv decoder.
- **Migration** (`origin-migrate`, `origin import`): adapters for Claude Code (jsonl + SKILL.md), jcode (rusqlite reader), opencode (storage/*.json); idempotent content-hash dedupe via new `Store::{contains,insert}_migrated_{session,skill}` + V6 SQLite migration; `--dry-run` / `--apply` / `--json` modes.
- **Benchmarks** (`origin-bench`): 8-task fixed set, origin + generic subprocess runners, Markdown + JSON reports.
- **Docs site** (`docs/site/`): 11-chapter mdBook (intro/quickstart/architecture/configuration/providers/skills/hooks/mcp/migration/sdk/troubleshooting); `origin --tutorial` 7-step guided tour; clap_mangen manpages via `xtask manpages`.
- **Release engineering** (`.github/workflows/release.yml`): 6-target matrix build (glibc/gnu x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64/aarch64) with cosign keyless signing + SLSA build-provenance attestation; packaging templates for Homebrew (`packaging/homebrew/origin.rb.tmpl`), winget, AUR, cargo-binstall metadata; `xtask release` stamps `{{VERSION}}` + `{{SHA256_*}}` placeholders.

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
