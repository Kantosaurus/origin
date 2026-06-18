# Architecture Overview

> **Last reviewed against workspace version:** `0.9.8` (`[workspace.package] version` in `Cargo.toml`).

`origin` is a Rust-native agentic coding harness: a thin terminal client (`origin-cli`) plus a supervised, long-lived daemon (`origin-daemon`) that hosts LLM-driven coding sessions on your own machine. The two processes never share memory or call each other directly — they communicate exclusively over a single framed transport (`origin-ipc`) carrying archived (`rkyv`) intermediate-representation frames. This document is the canonical map of the workspace: it explains the daemon/CLI split, the two-runtime concurrency model, the archived IR that flows end-to-end, content-addressed storage, the request lifecycle, the full crate layout, and the cross-cutting engineering invariants that hold across all ~77 crates.

---

## System at a glance

`origin` runs on the same conceptual shape as Claude Code, jcode, and opencode — **model + tools + skills + hooks + permissions** — but it treats four performance characteristics as **first-class CI gates** rather than aspirational goals. It draws *attributes* from those harnesses while implementing every signature subsystem with an original mechanism.

### What origin is

- A **Cargo workspace** (`resolver = "2"`, `members = ["crates/*", "xtask"]`) of ~77 crates pinned to **Rust 1.83** and edition 2021, licensed Apache-2.0.
- A **clean two-process split**: `origin-daemon` hosts sessions (agent loop, goal driver, provider wiring, storage); `origin-cli` is a thin client that renders a TUI and supervises/auto-spawns the daemon.
- A harness whose **only** daemon ↔ CLI channel is `origin-ipc` — there is no side channel, no shared file, no direct function call across the boundary.

### The four performance KPIs (treated as CI gates)

| KPI | What it bounds | Why it is gated |
| --- | --- | --- |
| **Cold start** | Time from process launch to a usable session. | A harness you wait on is a harness you stop using; the `perf-gate` workflow asserts read-only tasks complete in ≤ 80 ms wall time in CI. |
| **Keystroke-to-pixel latency** | Time from a key event in the TUI to the rendered glyph. | The renderer must never be starved by a tool exec or sidecar job (see the two-runtime model). |
| **Steady RSS** | Resident memory under sustained use. | Long-lived daemons leak by default; CAS dedupe + tiered eviction keep the working set bounded. |
| **Cache hit rate** | Fraction of tool outputs / reads / embeddings served from CAS, and prompt-cache prefix stability. | Determines both latency and provider spend; `origin-planner` and `origin-cas` exist to protect it. |

### The "model + tools + skills + hooks + permissions" shape

Every turn the daemon assembles a request from five composable surfaces, each owned by its own crate(s):

- **Model** — a provider behind a uniform `Provider` trait, resolved from a catalog (`origin-provider*`).
- **Tools** — a registry of builtin and MCP tools (`origin-tools`, `origin-mcp`).
- **Skills** — embedding-indexed, lazily materialized top-K per turn (`origin-skills`, `origin-mem`).
- **Hooks** — typed lifecycle events dispatched through a pre-spawned shell pool (`origin-hooks`).
- **Permissions** — a tier-based gate with a pluggable prompter, hardened against command-line bypasses (`origin-permission`, `origin-cmdparse`, `origin-conseca`, `origin-policy`).

---

## The daemon/CLI split

`origin` deliberately splits the harness into two processes with a hard boundary between them.

```
            ┌──────────────────────────────┐                ┌──────────────────────────────────────┐
            │          origin-cli          │                │             origin-daemon              │
            │  (thin client + TUI)         │                │            (session host)              │
            │                              │                │                                        │
            │  • crossterm raw-mode TUI    │                │  • agent loop / goal driver            │
            │  • origin-tui cell renderer  │   origin-ipc   │  • provider factory + catalog          │
            │  • composer / input reduce   │ ◄════════════► │  • CAS + SQLite session store          │
            │  • daemon auto-spawn +       │  rkyv frames   │  • skills / memory / code-graph        │
            │    self-update supervisor    │  (local socket │  • swarm coordinator + sidecar         │
            │  • clap CLI subcommands      │   / QUIC+mTLS)  │  • permission + sandbox + hooks        │
            └──────────────────────────────┘                └──────────────────────────────────────┘
```

### `origin-cli` — the thin client (*"Terminal UI and CLI for the origin agent runtime"*)

The CLI owns the human-facing surface and nothing else load-bearing about a session:

- It drives a `current_thread` Tokio runtime on a dedicated thread with a 16 MiB stack (`RUNTIME_STACK_SIZE`), because the TUI's top-level future is one large inlined state machine that overflows Windows' default 1 MiB main-thread stack — the same reason the daemon hand-rolls its entrypoint instead of using `#[tokio::main]`.
- It parses the full `clap` command tree (`cli_def.rs`), which is intentionally housed in the library crate so `xtask manpages` can render man pages via `clap_mangen` without depending on the binary.
- It auto-spawns and supervises the daemon, applies staged self-updates (`apply_staged_if_present`), and kicks off a non-blocking background update worker.
- When `Cli::cmd` is `None`, it enters the interactive TUI; otherwise it dispatches a subcommand (`Run`, `Sessions`, `Keyring`, `Pair`, `Trace`, `Workflow`, `Team`, `Review`, …) and exits.

The interactive CLI opens a **fresh daemon connection per prompt**. This is why session-scoped overrides such as the active account live in process-global state on the client side (`SESSION_ACCOUNT`) and are stamped onto every `PromptRequest` rather than held on a connection.

### `origin-daemon` — the session host (*"Daemon hosting origin session, agent, and protocol services"*)

The daemon owns everything stateful. Its library (`lib.rs`) exposes a broad module map — `agent`, `goal_driver`, `session`, `session_store`, `protocol`, `provider_factory`, `stream_relay`, `skill_catalog`, `swarm_worker`, `remote_quic`, `scheduler`, `ambient`, `overnight`, `supervisor`, `hooks_runtime`, and more — and the binary (`main.rs`) wires them together in `daemon_setup`, which:

1. Opens the CAS `Store` (`hot_capacity: 256`, `warm_pack_target_bytes: 4 MiB`, `cold_zstd_level: 3`).
2. Detects the `KeyVault` (with an audit ring under `~/.origin/keyvault-audit`).
3. Builds the provider catalog + `ProviderFactory`, resolving the initial provider (auto-selecting `anthropic-oauth` over `anthropic` when only OAuth tokens are present).
4. Opens the SQLite `SessionStore`, the code-graph index, the memory subsystem, the sidecar pool, and the swarm `Coordinator`.
5. Loads the skill catalog, workflows catalog, and optional governance overlay.
6. Binds the IPC `Listener`, writes an instance-pid control file, optionally stands up the QUIC remote transport, the metrics endpoint, and the default-off autonomous loops (scheduler / ambient / overnight / webhook / mem-garden).
7. Enters the accept loop, spawning one handler task per connection.

### Why the split exists

- **Latency isolation.** The renderer and input loop live in the client; a slow provider call, a heavy tool exec, or a code-graph rebuild in the daemon cannot block a keystroke from reaching a pixel.
- **Lifetime decoupling.** The daemon outlives any single CLI invocation. `origin run "…"` connects, drains to completion, and exits; the warm session, CAS, and indexes persist. The `origin-supervisor` crate restarts the daemon on crash and resumes sessions across restarts.
- **A single auditable surface.** With one transport, the entire client↔host contract is the set of `FrameKind` frames and the `ClientMessage`/`StreamEvent` enums. The QUIC transport reuses that exact contract for remote clients, and the recorder taps it for deterministic replay.
- **Security containment.** Sandboxing, the permission gate, and the KeyVault all live daemon-side; the client never sees raw secrets and cannot bypass the gate.

---

## Two-runtime model

A naïve single Tokio runtime lets any task — a tool exec, a sidecar small-model job, a bulk code-graph build — contend for the same worker threads that drive the renderer relays and the IPC accept loop. `origin` rejects that. The daemon runs **two runtimes** and enforces a **task-class discipline** on every spawn.

### The split

The daemon's `main` does **not** use `#[tokio::main]`. It hand-rolls a two-runtime launcher (`runtime_launch::start`) on its own OS thread:

- A **control plane** runs on a `current_thread` runtime pinned to a dedicated `origin-ctrl` OS thread. It owns the cooperative-shutdown phase driver and the signal handler — work that must never be jostled by worker scheduling.
- A **worker pool** (`multi_thread`) gets `physical_cores − 1` workers and runs the async pipeline (`daemon_setup`, the accept loop, per-connection handlers).

`main` waits for the worker handle to be populated, hands `daemon_setup` to the worker pool via `spawn_blocking` + `Handle::block_on`, installs a cross-platform `ctrlc` handler that posts to an mpsc channel, and lands the phased `CooperativeShutdown` driver on the control core. The OS main thread holds the parquet trace guard so its `Drop` (flush) runs only after both runtimes tear down.

### The `spawn_in(class, fut)` discipline

`origin-runtime` (*"Task-class budgeting with per-class semaphores and a spawn_in helper"*) is the enforcement point. **Every** spawned future in the daemon goes through `spawn_in(TaskClass, fut)` — a clippy lint rejects bare `tokio::spawn`. Each class has a per-class semaphore sized relative to core count:

| `TaskClass` | Used for | Permit budget (`permits_for`) |
| --- | --- | --- |
| `Critical = 0` | Agent loop turns; provider HTTP/2; tool exec; swarm worker bodies; per-connection handlers. | `(cores * 4).max(8)` |
| `Realtime = 1` | Renderer ticks; IPC event dispatch; per-stream relays; the plan-bus bridge. | `(cores * 2).max(4)` |
| `Sidecar = 2` | Sidecar small-model jobs; MCP server clients; hook dispatch. | `cores.max(2)` |
| `Background = 3` | CAS GC; SQLite vacuum; memory idle consolidation. | `2` |
| `Bulk = 4` | Initial code-graph build; bulk MCP discovery. **Paused while any `Critical` permit is held** (via `BulkGate`). | (gated) |
| `Swarm = 5` | Swarm sub-agent worker bodies; an isolated pool so swarm concurrency neither starves nor is starved by `Sidecar`. | (isolated) |

The class taxonomy is documented in `crates/origin-runtime/src/class.rs`; lower numbers are more important. `spawn_in` additionally maintains a **`Critical` busy-counter** (an RAII `CriticalGuard`) so the `Bulk` gate can park bulk work whenever any `Critical` task is in flight.

### The problem it solves

A tool exec or a sidecar job **must not starve the renderer or the IPC accept loop.** By classifying tasks and budgeting them on separate semaphores — and by pausing `Bulk` whenever `Critical` work exists — `origin` guarantees that interactive paths (keystroke-to-pixel, IPC dispatch) keep their headroom even while heavy background work runs. The `Swarm` pool's deliberate separation from `Critical`/`Bulk` ensures that a parent agent awaiting a child sub-agent can never deadlock the pool.

---

## Archived IR (rkyv) end-to-end

`origin-core` (*"Core message types, IR, and provider capabilities"*) defines the **single intermediate representation** that flows through the whole system. The load-bearing types — `Message`, `Block`, and the role/tool-call shapes — derive `rkyv::{Archive, Serialize, Deserialize}` with `#[archive(check_bytes)]`, so **one byte buffer flows through IPC, SQLite blobs, and in-memory ring buffers without re-encoding on the hot path.**

### The IR shape (`origin-core::ir` / `origin-core::types`)

```rust
// Role: the conversational role of a message.
pub enum Role { User, Assistant, Tool, System }

// Block: the atomic unit of message content. (#[archive(check_bytes)])
pub enum Block {
    Text       { text: String, cache_marker: Option<CacheBoundary> },
    ToolUse    { id: String, name: String, input_json: Vec<u8>, cache_marker: Option<CacheBoundary> },
    ToolResult { tool_use_id: String, handle: Option<[u8; 32]>, inline: Option<Vec<u8>>, cache_marker: Option<CacheBoundary> },
    Thinking   { tokens: String, signature: Option<String> },
}

// Message: a role plus an ordered list of blocks.
pub struct Message { pub role: Role, pub blocks: Vec<Block> }
```

Three design points are worth calling out:

- **A `ToolResult` carries a `handle: Option<[u8; 32]>`, not the bytes.** That 32-byte array is a CAS hash. Large tool outputs are offloaded to content-addressed storage and referenced by handle; only small results are stored `inline`. This is the seam where the IR and CAS meet (see the next section).
- **`cache_marker: Option<CacheBoundary>`** (`Frozen` / `Sticky` / `Sliding`) lets the prompt-cache prefix planner annotate blocks with stability bands so the provider's prompt cache stays warm across turns.
- **`#[archive(check_bytes)]`** means a received buffer can be **validated in place** — bytes are checked against the archived layout without a full deserialize.

### Why archived, not JSON

Validating an archived buffer is roughly **~200 ns**, versus **~20 µs** to JSON-decode the same payload — a ~100× difference on a path that every IPC frame, every persisted transcript blob, and every ring-buffer fan-out traverses. Because the same archived bytes serve all three consumers, the daemon avoids re-encoding when it:

- reads a `ClientMessage` off an `origin-ipc` frame,
- stores a transcript turn as a SQLite blob via the session store,
- or fans streamed deltas out through `origin-stream` ring buffers.

The IR also carries a self-healing invariant: `origin-core` defensively enforces the provider's tool-pairing rule (every `ToolResult` must have a matching `ToolUse` in the preceding kept message), so a corrupted transcript — a reused session id, a compaction hole, a hand-edited store — degrades gracefully instead of triggering a hard `400` from the provider.

---

## Content-addressed everything

`origin-cas` (*"Content-addressed store with FastCDC chunking, mmap pack files, and three-tier storage"*) is the storage substrate beneath the IR. Tool outputs, file reads, embeddings, memory, and code-graph nodes are all addressed by the hash of their content and **deduped across turns, sessions, and swarm workers.**

### The three tiers

| Tier | Backing | Role |
| --- | --- | --- |
| **Hot** | In-memory LRU (`hot_capacity`, e.g. 256 entries for the main store). | Fast resident access to recently-touched blobs. |
| **Warm** | `mmap`-ed pack files (`warm_pack_target_bytes`, e.g. 4 MiB targets). | Zero-copy reads of packed blobs without a heap allocation per get. |
| **Cold** | `zstd`-compressed packs (`cold_zstd_level`, e.g. 3). | Long-tail storage; trades CPU for resident bytes. |

Eviction flows Hot → Warm → Cold; on shutdown the cooperative driver calls `flush_all` (not merely the warm-pending flush) so resident Hot bytes — including offloaded tool-result payloads — survive a restart and their transcript handles don't "cas miss".

### Chunking and dedupe

- **FastCDC content-defined chunking** (`chunker::{chunks, ChunkIter, ChunkRef}`) splits blobs at content-derived boundaries, so an edit in the middle of a file re-chunks only the affected region — neighbouring chunks keep their hashes and stay deduped.
- A **refcount + GC** layer (`refs::{RefTable, RefError}`) tracks live references so unreferenced chunks can be collected by a `Background`-class task.
- **`[u8; 32]` handles in the IR** are exactly CAS hashes (`hash::Hash`), so the IR and the store share one address space: a `Block::ToolResult { handle, .. }` is a direct pointer into CAS.

Because addressing is by content, **a file read or tool output produced by one swarm worker is automatically shared with every other worker, turn, and session that produces the same bytes** — the dedupe that protects the steady-RSS and cache-hit KPIs.

> The daemon opens a **second, isolated CAS root** for the code-graph subsystem (`{cas_root}/codegraph`) so graph indexing never contends with the session store.

---

## Request lifecycle / data flow

The following walkthrough traces a single interactive turn from keystroke to rendered tokens. Crate ownership is noted in parentheses.

1. **Keystroke** — the user types in the TUI. `crossterm` raw-mode events feed the input reducer (`reduce_editor` / `InputAction`), which updates the composer (`origin-tui::composer`). (`origin-cli`, `origin-tui`)
2. **Submit** — on submit, the CLI builds a `PromptRequest`, stamps the session account / thinking-token seed, wraps it as `ClientMessage::Prompt`, and **opens a fresh daemon connection** (`Connector` → `Connection`). (`origin-cli`)
3. **IPC frame** — the request is encoded as a length-prefixed `FrameKind::Request` frame (`MAGIC = "ORON"`, 64 MiB body cap) and written over the local socket / named pipe (or QUIC+mTLS for a remote daemon). (`origin-ipc`)
4. **Accept + dispatch** — the daemon's accept loop returns a `SharedConnection`; `spawn_handler_task` spawns a per-connection handler in `TaskClass::Critical`. The handler reads the frame body and decodes a `ClientMessage` (with a legacy `PromptRequest` fallback). (`origin-daemon`)
5. **Session bind** — the handler snapshots the active `Provider` (so a concurrent `/account` switch can't yank it mid-flight), builds a per-prompt Haiku-backed goal verifier, and rehydrates / creates the session transcript from the SQLite session store. (`origin-daemon`, `origin-store`)
6. **Context assembly** — skills are materialized top-K, memory is recalled, code-graph context is fetched, and the prompt-cache prefix planner annotates blocks with `CacheBoundary` bands. (`origin-skills`, `origin-mem`, `origin-codegraph`, `origin-planner`)
7. **Agent loop** — the daemon enters its loop: send the assembled `Message` history to the provider. (`origin-daemon::agent`)
8. **Provider call** — the active `Provider` issues a streaming HTTP/2 request (rustls-only TLS), running in `TaskClass::Critical`. (`origin-provider*`)
9. **Streamed tokens** — server-sent deltas are parsed, written into an `origin-stream` ring buffer, and relayed back to the connection as `FrameKind::Event` frames (`stream_relay::relay_to_connection`, `TaskClass::Realtime`). The TUI renders them via the cell-grid widget. (`origin-stream`, `origin-tui`)
10. **Tool calls** — when the model emits a `Block::ToolUse`, the daemon dispatches it through the tool registry. (`origin-tools`)
11. **Permission gate** — before any side effect, the call passes the tier-based permission engine; a Bash command is additionally analyzed by `origin-cmdparse` for bypass classes, and the optional per-prompt ConSeca / governance overlay can deny it. A blocking ask is resolved via the daemon-wide `PermissionRegistry` / `ChoiceRegistry` (so a decision arriving on a fresh connection can resolve an ask emitted on the busy one). A skill that omits `Bash` literally cannot shell out; sandboxing (landlock+seccomp / sandbox-exec / AppContainer) confines what does run. (`origin-permission`, `origin-cmdparse`, `origin-conseca`, `origin-policy`, `origin-sandbox`)
12. **CAS offload** — the tool result is written to CAS; large outputs become a `Block::ToolResult { handle: Some([u8; 32]), .. }` referencing the content hash, small ones stay `inline`. The freshly-produced handle is registered into the shared cache-band `Plan`. (`origin-cas`, `origin-planner`)
13. **Loop or finish** — the tool result is appended to the transcript and the loop iterates (back to step 7) until the model stops emitting tool calls; the turn's blocks are persisted as archived SQLite blobs. (`origin-daemon`, `origin-store`)
14. **Completion** — a final `StreamEvent` closes the turn; the CLI reconstructs the assistant text from the streamed deltas and the connection closes. (`origin-cli`)

### Sequence diagram

```mermaid
sequenceDiagram
    autonumber
    actor U as User
    participant T as TUI (origin-cli/origin-tui)
    participant I as origin-ipc (frames)
    participant D as Daemon handler (Critical)
    participant P as Provider (origin-provider*)
    participant G as Permission gate
    participant C as origin-cas

    U->>T: keystroke / submit
    T->>I: encode PromptRequest -> Request frame
    I->>D: read_frame_body -> ClientMessage::Prompt
    D->>D: snapshot provider, bind session, assemble context
    loop agent loop (until no tool calls)
        D->>P: streaming Messages request (HTTP/2, rustls)
        P-->>D: token deltas (SSE)
        D-->>I: StreamEvent (Event frames, Realtime relay)
        I-->>T: render deltas (cell-grid widget)
        opt model emits ToolUse
            D->>G: check tier + cmdparse + governance
            G-->>D: allow / deny / ask
            D->>D: execute tool (sandboxed)
            D->>C: write result bytes -> [u8;32] handle
            C-->>D: ToolResult{handle} appended to transcript
        end
    end
    D-->>I: final StreamEvent (turn complete)
    I-->>T: reconstruct assistant text; close connection
```

---

## The crate workspace (map)

The workspace contains ~77 crates (`crates/*` plus `xtask`; `crates/origin-daemon/fuzz` is excluded from the workspace). Each is linked to its per-crate reference page under `../crates/`. Descriptions below are pulled verbatim from each crate's `Cargo.toml` `description`.

### Core & IR

| Crate | Role |
| --- | --- |
| [`origin-core`](../crates/origin-core.md) | Core message types, IR, and provider capabilities for origin. |

### Transport & IPC

| Crate | Role |
| --- | --- |
| [`origin-ipc`](../crates/origin-ipc.md) | Framed local-socket and QUIC/TLS IPC transports for origin. |
| [`origin-stream`](../crates/origin-stream.md) | Single-producer multi-consumer byte ring with per-subscriber read cursors. |
| [`origin-resume-token`](../crates/origin-resume-token.md) | MAC-authenticated session resume token shared by the daemon and supervisor. |

### Storage, CAS & store

| Crate | Role |
| --- | --- |
| [`origin-cas`](../crates/origin-cas.md) | Content-addressed store with FastCDC chunking, mmap pack files, and three-tier storage. |
| [`origin-store`](../crates/origin-store.md) | SQLite-backed store for origin with embedded refinery migrations. |
| [`origin-alloc`](../crates/origin-alloc.md) | Per-component allocator arenas with a no-op default and opt-in jemalloc backend. |

### Providers

| Crate | Role |
| --- | --- |
| [`origin-provider`](../crates/origin-provider.md) | Provider trait with canonical request/response/usage/error types for origin. |
| [`origin-provider-anthropic`](../crates/origin-provider-anthropic.md) | Anthropic Messages API provider for origin. |
| [`origin-provider-bedrock`](../crates/origin-provider-bedrock.md) | SigV4-signed AWS Bedrock provider for origin (Anthropic InvokeModel). |
| [`origin-provider-gemini`](../crates/origin-provider-gemini.md) | Google Gemini generateContent provider for origin, with SSE streaming. |
| [`origin-provider-github`](../crates/origin-provider-github.md) | GitHub Copilot provider for origin with vault-sourced OAuth session tokens. |
| [`origin-provider-ollama`](../crates/origin-provider-ollama.md) | Ollama provider for origin with NDJSON streaming chat. |
| [`origin-provider-openai-compat`](../crates/origin-provider-openai-compat.md) | Generic OpenAI Chat-Completions wire client for OpenAI-shape providers. |
| [`origin-shimquirks`](../crates/origin-shimquirks.md) | OpenAI-compatibility shim per-backend request/response quirk handling. |
| [`origin-modeldiscovery`](../crates/origin-modeldiscovery.md) | Runtime model discovery: parse provider model listings, merge with builtin catalog, and cache with a TTL. |
| [`origin-router`](../crates/origin-router.md) | Model routing strategies (architect/editor split, phase-aware, scored, quota fallback) over fed-in health/latency. |
| [`origin-planner`](../crates/origin-planner.md) | Predictive prompt-cache prefix planner with stability bands and a prefix ledger. |
| [`origin-cost`](../crates/origin-cost.md) | Per-turn and cumulative USD cost + token accounting with prompt-cache economy awareness. |

### Agent, daemon & orchestration

| Crate | Role |
| --- | --- |
| [`origin-daemon`](../crates/origin-daemon.md) | Daemon hosting origin session, agent, and protocol services. |
| [`origin-runtime`](../crates/origin-runtime.md) | Task-class budgeting with per-class semaphores and a spawn_in helper. |
| [`origin-sidecar`](../crates/origin-sidecar.md) | Always-on small-model worker with a bounded queue and pooled workers. |
| [`origin-swarm`](../crates/origin-swarm.md) | Coordinator/worker protocol with completion reports. |
| [`origin-plan`](../crates/origin-plan.md) | CRDT op-log and deterministic fold for the shared swarm plan. |
| [`origin-goal`](../crates/origin-goal.md) | Goal driver with persistent completion conditions and an inline self-tag protocol. |
| [`origin-supervisor`](../crates/origin-supervisor.md) | Restarts origin-daemon on crash and resumes sessions across restarts. |
| [`origin-selfdev`](../crates/origin-selfdev.md) | Binary self-development: a safe-by-construction supervised hot-reload state machine with injected side effects, build queue, and reload-context resume. |
| [`origin-ambient`](../crates/origin-ambient.md) | Resource-aware always-on + overnight autonomous mode policy under an adaptive token budget. |
| [`origin-schedule`](../crates/origin-schedule.md) | Pure-logic scheduling, cron/interval/daily spec parsing, and trigger queue over millisecond timestamps. |
| [`origin-steering`](../crates/origin-steering.md) | Mid-execution steering hints queued and merged into the next agent turn. |
| [`origin-workflowgen`](../crates/origin-workflowgen.md) | Deterministic offline workflow authoring: decompose a natural-language goal into an ordered, validated skill pipeline. |

### Tools

| Crate | Role |
| --- | --- |
| [`origin-tools`](../crates/origin-tools.md) | Tool registry, macros, and builtin tools for origin. |
| [`origin-mcp`](../crates/origin-mcp.md) | Model Context Protocol client with stdio/HTTP-SSE transports and OAuth. |
| [`origin-browser`](../crates/origin-browser.md) | Dual-backend browser router with one-shot WebFetch and Tavily WebSearch. |
| [`origin-websearch`](../crates/origin-websearch.md) | Pluggable web search with offline-testable result parsers and an injected fetcher. |
| [`origin-multimodal`](../crates/origin-multimodal.md) | Image and PDF context ingestion: classify, extract text, and build content blocks. |
| [`origin-gmail`](../crates/origin-gmail.md) | First-class Gmail tool over Google OAuth2 (auth-code + refresh) with token-frugal metadata reads. |
| [`origin-editfmt`](../crates/origin-editfmt.md) | Model-tuned edit-format matrix: parsers and appliers for diff formats. |
| [`origin-postedit`](../crates/origin-postedit.md) | Post-edit lint/test/format policy with a builtin formatter table; decision logic only, execution is the caller's job. |
| [`origin-lsp-client`](../crates/origin-lsp-client.md) | Minimal stdio JSON-RPC Language Server client for diagnostics. |
| [`origin-lspfleet`](../crates/origin-lspfleet.md) | Registry and auto-install decisioning for 40+ language servers, plus diagnostic aggregation. |
| [`origin-vcs`](../crates/origin-vcs.md) | Agent-native git safety layer: shadow-git checkpoints, restore, rewind, and a lane/draft-patch model. |
| [`origin-scout`](../crates/origin-scout.md) | Read-only dependency-source research: shallow-clone planning and repo overview extraction for origin. |
| [`origin-watch`](../crates/origin-watch.md) | Editor-agnostic watcher that scans source files for AI-trigger comments. |
| [`origin-clipboard`](../crates/origin-clipboard.md) | Copy/paste web-chat mode: format context to paste and parse pasted edits. |
| [`origin-voice`](../crates/origin-voice.md) | Speech-to-text dictation config and transcript interleave policy. |

### Skills, memory & code-graph

| Crate | Role |
| --- | --- |
| [`origin-skills`](../crates/origin-skills.md) | Skills loader with embedding upsert and allowed-tools narrowing. |
| [`origin-mem`](../crates/origin-mem.md) | Conversation memory with ONNX MiniLM embeddings, HNSW search, and temporal-decay re-ranking. |
| [`origin-codegraph`](../crates/origin-codegraph.md) | Native code knowledge graph with tree-sitter extraction and SQLite index. |
| [`origin-repomap`](../crates/origin-repomap.md) | Personalized-PageRank repo map over a symbol graph, packed into a token budget. |
| [`origin-knowledge`](../crates/origin-knowledge.md) | Local knowledge index: full-text inverted index + cosine vector search, JSON-persistable. |
| [`origin-plugin`](../crates/origin-plugin.md) | Plugin packaging, manifest parsing, dependency resolution, and live cross-tool skill discovery for origin. |
| [`origin-hooks`](../crates/origin-hooks.md) | Pre-spawned shell pool and typed lifecycle event dispatch. |

### TUI & CLI

| Crate | Role |
| --- | --- |
| [`origin-cli`](../crates/origin-cli.md) | Terminal UI and CLI for the origin agent runtime. |
| [`origin-tui`](../crates/origin-tui.md) | Custom cell-grid terminal renderer with SIMD damage diffing and ANSI emit. |
| [`origin-ui-preview`](../crates/origin-ui-preview.md) | Hot-reload terminal preview of the origin harness UI/UX (themes, palette, ANSI chrome). |
| [`origin-mermaid`](../crates/origin-mermaid.md) | Dependency-free renderer for a useful subset of mermaid flowcharts to ASCII. |
| [`origin-outputstyle`](../crates/origin-outputstyle.md) | Output styles (Explanatory/Learning/Concise) plus a transform-or-hide MessageDisplay hook. |
| [`origin-i18n`](../crates/origin-i18n.md) | Lightweight std-only UI string catalog with locale fallback and placeholder substitution. |
| [`origin-export`](../crates/origin-export.md) | Conversation transcript export to clean Markdown or pretty JSON. |
| [`origin-notify`](../crates/origin-notify.md) | Out-of-band human notifications with quiet-hours, batching policy, and injectable channel dispatch. |

### Security, sandbox & permission

| Crate | Role |
| --- | --- |
| [`origin-permission`](../crates/origin-permission.md) | Tier-based permission engine with a pluggable prompter. |
| [`origin-sandbox`](../crates/origin-sandbox.md) | Per-tool sandbox profiles for Linux, macOS, and Windows. |
| [`origin-cmdparse`](../crates/origin-cmdparse.md) | Bash command-line safety analysis that hardens the permission gate against known bypass classes. |
| [`origin-conseca`](../crates/origin-conseca.md) | Dynamic, model-generated per-prompt security policy parsed and enforced per tool call. |
| [`origin-policy`](../crates/origin-policy.md) | Layered governance / managed-settings engine: RBAC, model allow-lists, spend caps, trusted folders. |
| [`origin-keyvault`](../crates/origin-keyvault.md) | Cross-platform secret storage with a uniform async facade over per-OS keystores. |
| [`origin-oidc`](../crates/origin-oidc.md) | Workload Identity Federation / OIDC token-exchange request building and response parsing for keyless CI auth. |

### Ops, telemetry & trace

| Crate | Role |
| --- | --- |
| [`origin-trace`](../crates/origin-trace.md) | Tracing layer that writes spans to a per-day parquet ring with queryable predicates. |
| [`origin-metrics`](../crates/origin-metrics.md) | Bounded-cardinality counters with a Prometheus text encoder. |
| [`origin-telemetry`](../crates/origin-telemetry.md) | Opt-in, self-hostable product telemetry pipeline with secret redaction and sampling. |
| [`origin-doctor`](../crates/origin-doctor.md) | Environment/runtime diagnostics with injected probes plus a privacy phone-home disclosure. |
| [`origin-replay`](../crates/origin-replay.md) | Deterministic record-and-replay for origin sessions. |
| [`origin-cassette`](../crates/origin-cassette.md) | Deterministic, secret-safe HTTP cassette recording and sequential replay matching. |
| [`origin-bench`](../crates/origin-bench.md) | Benchmark harness comparing origin against other coding-agent CLIs. |

### Migration, review & misc

| Crate | Role |
| --- | --- |
| [`origin-migrate`](../crates/origin-migrate.md) | Migrate sessions, skills, and memories from other harnesses into origin. |
| [`origin-review`](../crates/origin-review.md) | Multi-agent confidence-scored review aggregation + issue auto-triage. |
| `xtask` | Release stamping, manpage generation, and repo automation (build-time workspace tool). |

> The per-crate pages under `../crates/<crate>.md` are authored separately; this overview only links to them.

---

## Cross-cutting principles

These invariants hold across the workspace and are enforced mechanically, not by convention.

### Zero `unsafe` (forbidden workspace-wide)

`Cargo.toml` sets `[workspace.lints.rust] unsafe_code = "forbid"`. The only crates that override the forbid are the three where zero-copy / SIMD / OS-FFI is unavoidable: **`origin-cas`** (mmap pack files, FastCDC), **`origin-tui`** (SIMD damage diffing, ANSI emit), and **`origin-ipc`** (low-level frame I/O). Even there, `origin-cas` additionally sets `#![deny(clippy::undocumented_unsafe_blocks)]`, so every `unsafe` block must carry a safety comment. The `unsafe: forbidden` badge in the README reflects this policy.

### Pedantic + nursery clippy, `unwrap_used = deny`

The workspace lint policy is strict by default:

```toml
[workspace.lints.clippy]
pedantic    = { level = "warn", priority = -1 }
nursery     = { level = "warn", priority = -1 }
unwrap_used = "deny"
panic       = "warn"
```

CI runs `cargo clippy -D warnings` with `pedantic`/`nursery` enabled and `unwrap_used` promoted to a hard error, so a stray `.unwrap()` fails the build. (Test modules opt out locally with `#[allow(clippy::unwrap_used, clippy::panic)]`.) This is also where the `spawn_in` discipline is enforced — a bare `tokio::spawn` is rejected so every task is classified.

### `Secret<T>` redaction

`origin-keyvault` provides `Secret<T: Zeroize>` — a guard wrapper that **zeroizes on drop** and **redacts in `Debug`** (its `fmt::Debug` writes the literal `Secret<redacted>`). Provider credentials live only in the daemon's `KeyVault`, which fronts OS-native credential stores with an age-encrypted fallback and an audit ring that records `(provider, account, action, timestamp)` tuples — **never the secret bytes** — to a rotating JSONL log. A CI lint additionally rejects raw secret bytes flowing through `tracing`.

### Deterministic replay

`origin-replay` records sessions into `.origin-replay` bundles by tapping the load-bearing seams behind a `recorder` feature: `origin-ipc::recorder_hook` registers an `IpcTap` over the frame stream, and `origin-cas::recorder_hook` registers a `CasTap` over the content store. Because the IR is the same archived bytes everywhere, a recorded bundle can be replayed deterministically — the foundation for the offline `--tutorial`, the bench harness, and HTTP cassette matching (`origin-cassette`).

---

## Where to go next

- [Runtime & concurrency](../architecture/runtime-and-concurrency.md) — the two-runtime launcher, `spawn_in` task classes, `BulkGate`, and cooperative shutdown in depth.
- [Data & storage](../architecture/data-and-storage.md) — the archived IR, CAS tiers + FastCDC + GC, and the SQLite session store / migrations.
- Subsystems:
  - [Agent loop & sessions](../subsystems/agent-and-sessions.md)
  - [Providers & routing](../subsystems/providers.md)
  - [Tools](../subsystems/tools.md)
  - [Skills, hooks & workflows](../subsystems/skills.md)
  - [Memory, code-graph & retrieval](../subsystems/memory-and-codegraph.md)
  - [Swarm & orchestration](../subsystems/swarm-and-orchestration.md)
  - [TUI & CLI](../subsystems/tui-and-cli.md)
  - [Trace, metrics & telemetry](../subsystems/observability.md)
- [Security model](../security/security-model.md) — permissions, sandbox, governance, secrets.
- [Getting started](../guides/getting-started.md) — install, first session, and configuration.
