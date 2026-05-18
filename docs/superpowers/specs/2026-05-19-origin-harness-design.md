# `origin` — Performance-First Agentic Coding Harness

**Date:** 2026-05-19
**Status:** Design — approved through brainstorming; pending writing-plans
**Authors:** Ainsley Woo + Claude (Opus 4.7)

---

## Executive summary

`origin` is a Rust-native agentic coding harness that combines the surface area of a Claude-Code-class tool with multi-provider support and four perf KPIs treated as first-class goals: cold-start + TUI responsiveness, memory footprint, token efficiency, and turn latency. It draws *attributes* from three prior harnesses — openclaude, jcode, opencode — but every signature subsystem uses an **original mechanism** that beats those sources on at least one perf or token dimension.

The system runs as a daemon process hosting one or more sessions, with thin client surfaces (TUI primary, headless fall-out, future desktop/web via the same SDK). Storage uses SQLite (WAL) for indexes and a content-addressed store (CAS) for blobs. Memory is split into two graphs: conversation memory (`origin-mem`) for cross-session learned context, and code knowledge graph (`origin-codegraph`) ported natively from graphify's approach. Subagents are coordinated via a CRDT-backed plan with credit-based shared-memory channels; the TUI uses a custom cell-grid renderer with SIMD damage diff.

## Goals

1. **Perf KPIs as gates:** cold start to first prompt < 50ms; keystroke-to-pixel < 12ms p99; steady RSS < 200MB under 4-hour heavy sessions; cache hit rate ≥ 70% on stable sessions.
2. **Multi-provider with no lowest-common-denominator loss:** native trait-per-provider, superset IR, compile-time projections.
3. **Cross-session intelligence:** semantic memory graph + native code knowledge graph, jointly queryable.
4. **Parallel multi-agent work:** swarm coordinator/worker model with CRDT plan + CoW workspaces.
5. **Production-grade:** sandboxed tool execution, OS-keychain credential storage, structured observability, signed releases.
6. **Originality constraint:** for every signature subsystem, the implementation is novel relative to openclaude, jcode, opencode — we may keep attributes (shortlisted features) but not mechanisms.

## Non-goals (for v1)

- Desktop GUI / web frontend (out-of-process; the SDK is shaped to allow these but they ship separately).
- WASM plugin runtime (deferred; user code extends via skills, hooks, and MCP).
- Agent Client Protocol (ACP) for external agents (deferred).
- Mobile clients.
- Public hosted backend service.

---

## Section 1 — High-level architecture & crate map

### Shape

A core library that knows nothing about UI, plus a daemon binary that hosts sessions, plus a TUI client. Headless mode is "TUI client without the renderer."

### Crate graph (Cargo workspace)

```
origin-core         pure types, no_std-compatible where possible
  ├─ types          Message, Part, ToolCall, Permission, Tier, Urgency (rkyv-derived)
  └─ ir             canonical superset Intermediate Representation

origin-cas          content-addressed store for tool outputs, file reads, embeddings,
                    code-graph nodes/edges, memory bodies

origin-provider     trait + per-provider crates (anthropic, openai, gemini, openrouter,
                    bedrock, ollama, github-models)

origin-tools        tool registry, tool impls, sandboxed exec
origin-mcp          MCP v1.29 client (stdio/HTTP/SSE)
origin-hooks        lifecycle hook dispatch with pre-spawned shell pool
origin-skills       skill loader, frontmatter parser, embedding-triggered injection

origin-mem          semantic memory graph + HNSW index + sidecar verifier client
origin-codegraph    native code knowledge graph (graphify port, novel impl)

origin-sidecar      always-on small-model worker: compaction, memory verification,
                    summarization, non-code entity extraction

origin-swarm        coordinator/worker protocol, plan reference, shared-memory rings
origin-plan         CRDT plan op log + snapshots

origin-permission   tier + rule + interactive engine
origin-keyvault     OS-native credential store wrapper (Credential Manager / Keychain /
                    Secret Service) + OAuth flows

origin-store        SQLite WAL layer + migrations + mmap reads
origin-ipc          daemon IPC: rkyv-archived messages over Unix sockets / named pipes;
                    QUIC for remote
origin-replay       provider/IPC recording + deterministic replay harness

origin-daemon       binary: hosts sessions, plumbs ipc → core → providers
origin-supervisor   tiny binary: restart-on-panic shim for origin-daemon

origin-tui          custom renderer (damage-region diff, SIMD compositor, side panel)
origin-cli          binary: TUI client + headless one-shot mode + admin subcommands
```

### Why this shape

- `origin-core` types are `rkyv`-archived from day 1 so the same byte buffer flows through IPC, SQLite blob storage, and in-memory ring buffers — no serialize/deserialize hops on the hot path.
- `origin-cas` is the foundational novelty: tool outputs, file reads, embeddings, memory bodies, and graph nodes/edges all live there, content-addressed and deduped across turns/sessions/swarm workers.
- `origin-sidecar` is a service the agent loop, memory, code graph, and compaction all consume — single shared small-model worker, never spun up per-call.
- The daemon never imports `origin-tui`; the CLI never imports `origin-daemon` internals. They speak only `origin-ipc`. Headless, multi-client, and future remote/desktop surfaces fall out from this separation.

---

## Section 2 — Agent loop

The standard loop everyone uses: build messages → call provider → stream tokens → parse `tool_use` → execute tool → append result → repeat. Our shape is the same; the mechanics inside each step are where the wins are.

### Novel mechanisms

**N2.1 Single-buffer token stream with multi-tail consumers.** The provider stream is one append-only `Bytes` ring; the renderer, tool-use parser, sidecar, and transcript writer each hold a tail cursor and read concurrently. Zero allocation per token after the ring is sized.

**N2.2 Speculative tool dispatch.** The tool-use parser is incremental over the streaming JSON. As soon as a pure tool's input args are fully present, the tool runs on a background task even before the model closes the `tool_use` block. Pure tools only (`Read`, `Glob`, `Grep`, `WebFetch`, `graph_*`); side-effecting tools (`Bash`, `Write`, `Edit`, MCP writes) wait for full block + permission.

**N2.3 KV-cache lattice — predictive prefix planning.** Every outgoing request is built by a `CachePlanner` that sorts sections into four bands by historical stability — *Frozen* (system prompt, tool schemas), *Sticky* (long-lived skill injections, project context), *Sliding* (stable recent conversation prefix), *Volatile* (this turn's new injections). Cache markers are emitted at band boundaries. Bands track their measured hit rate via `cache_read_input_tokens`; sections that miss repeatedly are demoted to *Volatile* so they stop poisoning higher bands.

**N2.4 Messages as content-addressed handles.** Tool results live in CAS. The message log stores `Handle(hash, preview_64B)`, not the inline bytes. The message-to-wire serializer expands handles to inline content only when the cache plan deems it worth the tokens; otherwise it emits a short reference and the model uses a `Recall` tool to inflate just what it needs.

**N2.5 Sidecar-as-coroutine.** `origin-sidecar` runs a small model (Haiku/Flash-class, user-configurable) continuously in the background with three duties: (a) eager turn summarization so compaction is a near-free lookup, (b) memory-recall verification in parallel with the main turn starting, (c) tool-output structure extraction for large CAS blobs.

### Loop sequence

```
prompt → CachePlanner builds request (handles, ordered prefixes)
       → provider stream → ring buffer → multi-tail
       → tool parser (incremental, speculative dispatch for pure tools)
       → tool result → CAS store → handle in message log
       → sidecar queues summarization + structure extraction
       → repeat until model emits no tool_use
```

Each mechanism is a feature flag at crate level for staged rollout.

---

## Section 3 — Content-addressed store + SQLite storage layer

CAS underpins messages, memory bodies, code-graph nodes, query results, and tool outputs. SQLite holds only indexes and small inline fields.

### Novel mechanisms

**N3.1 FastCDC variable-length chunking.** Tool output, file content, and graph blobs are chunked at content-defined boundaries (rolling hash, ~16KB avg). Re-reading a file with one inserted line produces ~95% identical chunk hashes; second/third reads dedupe naturally.

**N3.2 Learned-dictionary zstd compression.** At daemon idle, train a 64KB zstd dictionary from sampled CAS shards. Subsequent shards compress 5–10× vs. zstd default and decompress in microseconds because the dict is mmap-resident. Dictionary versioned per user; re-trained on shard distribution drift.

**N3.3 Three-tier storage with one addressable namespace.** Same `Handle(hash, preview_64B)` resolves to whichever tier holds the blob:
- **Hot:** `Arc<Bytes>` in a daemon-process LRU (RAM budget default 64MB).
- **Warm:** mmap'd append-only pack files on disk; read returns `&[u8]` slice (no decode).
- **Cold:** zstd-dict-compressed pack files; decompressed lazily into Hot.

Promotion/demotion automatic by access frequency.

**N3.4 SQLite as index, never as blob store.** Schema holds `messages(session_id, turn, role, handle_root, sidecar_summary)`, `cas_refs(hash, refcount, tier, last_access)`, `memories`, `code_nodes`, `code_edges`, `code_communities`, `cross_links`, `permissions`, `sessions`, `swarm_state`, `plans`, `hooks_log`. SQLite stays under tens of MB even on huge histories; mmap-resident, page-cache-warm.

**N3.5 Zero-copy IPC blob handoff.** When a client asks for message N's tool output, the daemon returns a `SharedHandle { pack_file, offset, len, hash }`. The client mmaps the same pack file (read-only) and slices into it directly. The daemon never touches userspace memory for the payload. On Windows, named `FileMapping` provides the same semantics over named pipes.

### Garbage collection

Refcount decremented on: message dropped by compaction, session deleted, memory superseded. GC runs on daemon idle: shards with refcount=0 are marked dead; pack files re-compacted when dead-byte ratio exceeds 30%. Never blocks an active turn.

### Failure modes

- CAS corruption → hash check on read; auto-rebuild from session log replay.
- SQLite corruption → WAL rollback; daemon falls back to read-only mode and alerts user.
- mmap on Windows requires daemon to hold an exclusive write lock on pack files; clients open shared-read.

---

## Section 4 — Provider abstraction + cache planner

### Novel mechanisms

**N4.1 Superset IR + compile-time projections.** A `Message { role, blocks: Vec<Block> }` IR includes every feature any major provider supports (Text with `cache_marker`, ToolUse, ToolResult, Thinking, Image, Audio, …). Each provider crate declares `Capabilities` as a `const`. A `#[derive(ProviderProjection)]` macro generates the IR → wire adapter at compile time. Blocks the provider can't handle are projected losslessly when possible (e.g., Thinking → `<thinking>…</thinking>` text marked for strip-on-resend) or rejected at compile time when the caller marked them as required.

**N4.2 CachePlanner — predictive prefix layout.** The headline novelty (mechanism N2.3 detailed). PrefixLedger keeps `(section_id, byte_range, last_hit_turn)` across recent turns; sections sorted into Frozen/Sticky/Sliding/Volatile bands; markers emitted at boundaries. After each response, `cache_read_input_tokens` per band updates stability scores. **Swarm workers inherit the coordinator's Frozen+Sticky layout** so N workers hit cache N times.

**N4.3 Provider-aware request encoder.** No `serde_json::Value` on the hot path. Per-provider encoders walk the IR and write directly into a pre-sized `BytesMut`. Build-time codegen from per-provider schema files.

**N4.4 Unified streaming → ring buffer.** Per-provider stream parsers (Anthropic event-stream, OpenAI SSE, Gemini SSE, Bedrock binary, NDJSON for Ollama) write `TokenEvent`s — `#[repr(C)]`, `rkyv::Archive`-derived — directly into the daemon's shared byte ring. No intermediate `String`.

**N4.5 KeyVault.** Credentials never touch disk in plaintext. Windows: Credential Manager. macOS: Keychain. Linux: Secret Service with age-encrypted file fallback for headless. Multi-account per provider; OAuth flows with PKCE; refresh-token rotation. Sessions can pin an account; swarm workers inherit coordinator's account by default.

### Provider matrix (GA)

| Provider | Auth | Streaming | Prompt cache | Thinking | Tools |
|---|---|---|---|---|---|
| Anthropic | API key / OAuth | Event-stream | Explicit (cache_control) | Native | Parallel |
| OpenAI | API key | SSE | Implicit + Beta API | Reasoning models | Parallel |
| Gemini | API key / ADC | SSE | Implicit context cache | Thought process | Parallel |
| Bedrock | SigV4 | Event-stream (binary) | Pass-through Anthropic | Pass-through | Parallel |
| OpenRouter | API key | SSE | Provider-dependent | Provider-dependent | Provider-dependent |
| Ollama | Local socket | NDJSON | n/a | n/a | Single |
| GitHub Models | OAuth | SSE | Implicit | Reasoning models | Parallel |

Default build ships Anthropic + OpenAI + Gemini + Ollama. The rest are opt-in feature flags.

### Failure modes

- 429 → exponential backoff with `Retry-After` budget; optional fallback provider.
- Schema drift → `TokenEvent::Unknown` logged but non-fatal.
- Cache-marker rejection (provider semantics shift) → planner detects via `cache_read_input_tokens == 0` for marked sections; auto-disables markers for that provider; re-plans.

---

## Section 5 — Tool system

### Novel mechanisms

**N5.1 Compile-time tool registry.** A `#[origin_tool]` proc-macro emits a `const TOOL_META` per tool and registers it into a generated enum. Dispatch is a `match` expression the compiler turns into a jump table. Tool schemas are emitted as `&'static str` for inclusion in the system prompt — zero runtime allocation, no schema generation per turn.

**N5.2 Tool I/O through CAS handles.** Every tool's `execute` returns `ToolResult { handle: CasHandle, preview: SmallVec<u8, 256>, metadata }`. Tools write directly into a pre-allocated `BytesMut` sealed into a CAS shard on completion. The agent loop, message log, sidecar, and renderer consume the same mmap'd bytes.

**N5.3 Speculative dispatch for pure tools.** Detailed in N2.2. Side-effecting tools wait for full block + permission.

**N5.4 Result memoization via input hash within a session.** `ToolCall { name, normalized_input }` is hashed; if the exact same call ran this session and the result handle is still live, return the handle without re-executing. Per-tool normalization rules. `Bash` does **not** memoize (side effects possible). Message log carries `(cached from turn N)` so the model knows.

**N5.5 The Recall tool.** Counterpart to N2.4 handle substitution: the model calls `Recall(handle: "7af3", region: Optional<…>)` to inflate just the bytes it needs. Region selectors: `lines: 100..200`, `match: "regex"`, `outline_only: true` (sidecar produces a structure summary).

### Core tool set at GA

| Category | Tools |
|---|---|
| Filesystem | `Read`, `Write`, `Edit`, `Glob`, `Grep`, `MultiEdit` |
| Execution | `Bash` (PowerShell on Windows), `BashBackground` |
| Web | `WebFetch`, `WebSearch` |
| Agent control | `Task` (spawn worker), `Plan`, `Recall`, `Skill` (invoke skill), `Ask` (router) |
| Code graph | `graph_query`, `graph_path`, `graph_explain`, `graph_summarize`, `graph_rebuild` |
| Memory | `mem_search`, `mem_save`, `mem_forget` |
| MCP | dynamically registered, prefixed `mcp:<server>:<tool>` |
| Notebooks | `NotebookRead`, `NotebookEdit` |

### Permission tier defaults

- **AutoAllowed:** `Read`, `Glob`, `Grep`, `graph_*` (read-only), `mem_search`, `Recall`, `Plan`, `NotebookRead`.
- **RequiresPermission / Low:** `WebFetch`, `WebSearch`, MCP read tools.
- **RequiresPermission / Medium:** `Edit`, `Write`, `MultiEdit`, `Task`, MCP write tools.
- **RequiresPermission / High:** `Bash`, `BashBackground`, `graph_rebuild`, `mem_forget`.

---

## Section 6 — Memory graph + code graph

Two distinct subsystems, designed together because they cross-link.

### 6A `origin-mem` (conversation memory)

**N6.1 Int8-quantized embeddings with per-cluster centroid offsets.** MiniLM-class ONNX embedding (swappable). 384-dim f32 vectors compressed to `(centroid_id, int8 deltas)` with 256 cluster centroids. Recall: centroid-pruned shortlist, then int8 dot product. 6× smaller than f32; 8–12× faster recall on a 10k store; memory subsystem RAM under 5MB at 100k memories.

**N6.2 HNSW + temporal-decay re-rank.** HNSW returns top-3K, re-ranked by `sim × exp(-age_days / τ) × cluster_priority × edge_boost`. `τ` defaults to 30 days; agents can shift via `--fresh` / `--archival`. `Supersedes` edges drop candidates pointing back to active winners; `Contradicts` edges surface both for sidecar resolution.

**N6.3 Memory body in CAS, vector inline in SQLite.** `memories(id, vector_int8_blob, centroid_id, body_handle, tags_bitset, age, links)`. Bodies content-addressed → identical memories dedupe. Tags as 64-bit bitset over a small dictionary.

**N6.4 Sidecar idle consolidation.** When daemon idle >30s, sidecar runs bounded passes: cluster nearby memories (propose merged supersedes), detect contradictions (small classifier head + similarity), re-tag drifted memories. Crash-safe via SQLite transactions.

**N6.5 Save/recall as side-effects, not tools.** Sidecar auto-extracts candidate memories at end-of-turn; user reviews proposed memories in the TUI side panel (one-key accept/reject/edit). Auto-recall injects matching memories into the next turn behind the CachePlanner's Sticky band. `mem_search`, `mem_save`, `mem_forget` exist for explicit cases.

### 6B `origin-codegraph` (native graphify port)

**N6.6 FastCDC-incremental code extraction.** On `git commit` (hooks subsystem), only touched files reconsidered. Each file cut into FastCDC chunks with rolling-hash scoring biased toward AST node boundaries (tree-sitter range hints). Edit to one function changes ~1 chunk hash; only that chunk re-extracts.

**N6.7 Graph nodes/edges as CAS records, queries return handles.** `(entity_id, kind, name, location, signature, body_handle)` and `(from, to, kind, confidence, evidence_handle)` are CAS records. Cross-repo dedup automatic (`Option<T>` appears once across all your Rust projects). Query results are CAS-stored → CachePlanner pins them in Sticky band.

**N6.8 Sidecar for non-code entities.** Tree-sitter handles 30+ code languages locally. PDFs, docs, images, video frames route to sidecar via structured-output prompts with `confidence: EXTRACTED | INFERRED | AMBIGUOUS`. Async; never blocks `graph_rebuild`.

**N6.9 Leiden + flow-weighted PageRank for god nodes.** Edges weighted by call-graph flow (`Calls` > `Mentions`); `INFERRED` confidence discounted. Leiden clusters; top-N per cluster by weighted PageRank are god nodes. Communities re-cluster only when ≥5% of nodes changed.

**N6.10 Typed query DSL.** `graph_query` takes a typed DSL (`path`, `neighbors`, `communities`, `god_nodes`, `recent_changes`), not free-form NL. No second LLM hop inside the tool. NL is the model's job (it composes typed queries from intent). `graph_explain` is the only NL-output tool; it routes the structured result through sidecar with a tight template.

### 6C Cross-linking & query routing

**Shared identifiers:** `code:{repo_hash}:{kind}:{stable_name}` and `mem:{ulid}`. Both addressable as `NodeRef = enum { Code(CodeId), Memory(MemId), External(MCPRef) }`.

**Cross-edges:** memory body's `linked_code: Vec<CodeId>` populated by sidecar on save; reverse-indexed from code entities.

**`Ask(query)` router tool:** sub-millisecond typed classifier routes code-shaped queries to `origin-codegraph`, memory-shaped to `origin-mem`, hybrid to both. No LLM in the router.

**Joint recall injection:** memory auto-injector and code-graph auto-injector each propose up to N items per turn; merged, deduplicated against prior turn, emitted as one `<context>` block in the system prompt — one cache band, one set of tokens.

### Storage layout

```
SQLite: memories, mem_edges, mem_tags,
        code_nodes, code_edges, code_communities, cross_links

CAS:    memory bodies, code signatures/bodies/evidence,
        query result snapshots, embedding vectors (large blobs)

Sidecar queue: pending consolidation + non-code extraction jobs
```

---

## Section 7 — Swarm + plan CRDT + IPC

### 7A `origin-swarm` (coordinator/worker)

Lifecycle: `Spawned → Ready → Running → (Blocked | Paused) → (Completed | Failed)`.
Coordinator spawns workers via `Task` tool with `WorkerSpec { goal, allowed_tools, budget, isolation }`.

**N7.1 Workers inherit coordinator's CachePlanner prefix.** Worker's first request reuses the coordinator's Frozen + Sticky byte ranges. PrefixLedger promoted to swarm-scope; workers fork instead of building from scratch.

**N7.2 SPSC shared-memory ring buffer per pair.** Local swarm DMs/broadcasts go through `memmap2` + atomics, no syscalls per message. 64KB ring per pair, rkyv-archived `SwarmEvent`. ~80ns round-trip vs ~5µs over sockets.

**N7.3 CoW worker isolation via reflinks.** Linux: `ioctl_ficlone` on btrfs/xfs/zfs. macOS: `clonefile()` (APFS). Windows: `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on ReFS; hardlink-tree + write-overlay fallback elsewhere. Worker spawn drops from seconds (git worktree) to milliseconds.

**N7.4 Credit-based backpressure on every channel.** Each channel kind has a configured credit budget — plan updates (1–4), DMs (16), broadcasts (4), sidecar queue (256). Senders consume; receivers issue on consume. A stuck worker stops getting messages; never balloons RAM.

**N7.5 Structured `CompletionReport`.** Workers emit `{ goal, status, plan_updates, files_touched: Vec<CasHandle>, decisions: Vec<DecisionRecord>, follow_ups: Vec<TaskRef>, transcript_handle }`, not prose. Coordinator inlines only what it needs; transcript is a handle recallable on demand.

### 7B `origin-plan` (CRDT)

Append-only op log over a typed plan tree. Each op: `(actor_id, lamport_clock, op)`.

Ops: `AddStep(parent, content)`, `MarkStep(id, status)`, `EditContent(id, body)` (LWW), `AddNote(id, body)` (append), `Reorder(id, before)` (Logoot position keys).

**N7.6 Per-step lease tokens.** `LeaseStep(id, worker_id, expires_at)` op — workers race via lamport ordering; loser picks next step. No coordinator round-trip.

**N7.7 Snapshot compaction.** Every 128 ops, coordinator writes `Snapshot(seq, state_handle_in_cas)`. New workers fast-forward from latest snapshot; old ops GC'd after all workers ack.

### 7C `origin-ipc` (transport)

**N7.8 Single duplex stream per client, request-ID multiplexed.** All requests/responses/events over one socket/pipe per client; concurrent in-flight streams.

**N7.9 rkyv-archived wire format with validation-first parse.** ~200ns to validate a message vs ~20µs to JSON-decode.

**N7.10 Blob handoff via shared file mapping (N3.5).** Frames carry `SharedHandle`, never bytes.

**N7.11 Credit-based backpressure on streams** (same scheme as N7.4). Slow client → stop emitting events for that client; main loop continues.

**N7.12 QUIC + mutual TLS for remote.** Cross-machine clients use QUIC; stream multiplexing maps 1:1 to request-ID multiplexing. Auth via short-lived bearer from daemon pairing flow.

### Wire protocol surface

```
Session:    open, prompt, interrupt, close
Inspect:    list_sessions, list_workers, get_message, get_plan
Stream:     subscribe(session_id) → Event stream
Tool:       approve_permission, deny_permission, override_tier
Memory:     search, save, forget, list_proposals
Code graph: query, path, explain, summarize, rebuild
Config:     get, set, reload_skills, reload_hooks
Admin:      gc_cas, vacuum_db, switch_account, attach_swarm
```

All versioned (`v: u16`).

### Failure modes

- Worker hard crash → coordinator detects via lease expiry; plan steps released; overlay discarded; sidecar synthesizes `CompletionReport { status: Failed }` from partial transcript.
- IPC client crash → socket close detected; subscribed streams cleaned; sessions survive (multi-client reattach).
- SMR ring overflow → drop-oldest with metric; logged non-fatal.
- CoW unsupported → fall back to hardlink-tree + overlay; warning issued.

---

## Section 8 — Custom TUI renderer + daemon runtime

### 8A Renderer (`origin-tui`)

Built on `crossterm` for raw input/output; layout/draw/scrollback are ours.

**N8.1 Cell-grid double buffer with SIMD damage diff.** Two grids: `front` (terminal state model) and `back` (next frame). Cells are 16-byte packed `(glyph: u32, fg: u32, bg: u32, attr: u32)`. Diff via `wide::u8x32` on AVX2/NEON; emit cursor-move + style-set + glyph runs only for changed cells. ~120 bytes/frame on a 4-line update vs. 12KB for full redraw.

**N8.2 Event-loop-tied frame coalescing.** A `dirty: AtomicBool` flag set by state updates. After each event-loop tick: if dirty and ≥6ms since last render, draw. Otherwise schedule a one-shot wake at the 6ms mark; subsequent updates piggyback. Effective cap ~166Hz only when motion exists; idle frames cost zero.

**N8.3 Streaming render reads CAS / ring directly.** Streaming widget holds a tail cursor into the shared byte ring (live) or a `&[u8]` slice into mmap'd CAS (completed turns). On render, walk cursor with grapheme segmentation + width-cache, emit glyphs directly into back grid. No `String` per token.

**N8.4 Snapshot-stable grapheme-width cache.** `(grapheme_hash → width)` LRU at 8K entries. ZWJ-emoji clusters pre-canonicalized via `unicode-segmentation` once. Cache shared across widgets.

**N8.5 Side panel as separate render target.** Diffs, code-graph subgraph views, sidecar memory proposals, file inspection live in a right-hand panel with its own cell grid and damage tracker. Main pane reduces visible width but is clipped, not rewrapped. Per-turn layout cache stored in CAS-backed scrollback.

### 8B Daemon runtime (`origin-daemon`)

**N8.6 Per-component jemalloc arenas.** Each subsystem allocates from a named arena — `agent_*`, `cas`, `sidecar`, `swarm_*`, `ipc`. Worker completion returns its entire arena to the OS in one syscall. Fragmentation isolated; a chatty worker can't poison the agent's heap.

**N8.7 Tokio task-class budgeting.** Every spawn via `spawn_in(class, fut)` helper. Classes: `Critical` (agent loop turns), `Realtime` (renderer ticks, IPC events), `Sidecar` (compaction, extraction), `Background` (CAS GC, consolidation), `Bulk` (initial graph build, MCP discovery). Per-class concurrency cap; bulk paused when sessions active. Clippy lint enforces usage.

**N8.8 Two-runtime split.** Control core (`current_thread`) pinned to one OS thread handles IPC + renderer ticks + event dispatch + shutdown. Worker pool (`multi_thread`) sized to `physical_cores - 1` handles agent/provider/sidecar/CAS/MCP. Cross-runtime via SMR rings + atomics, never `block_on`.

**N8.9 Platform-native async I/O.** `tokio-uring` on Linux for CAS pack files. IOCP on Windows via `tokio::fs` overlapped I/O. kqueue on macOS. Provider HTTPS uses `hyper` + `rustls` with HTTP/2 multiplexing for Anthropic/OpenAI/Gemini; per-account warm connection pool.

**N8.10 Cooperative phased shutdown.** Phases: (1) stop accepting new IPC, (2) cancel Bulk + Background, (3) wait for Critical with budget (default 30s), (4) persist sidecar queue, (5) flush CAS write buffer + close pack files, (6) SQLite WAL checkpoint, (7) close IPC, (8) release shared memory + tear down arenas. Each phase has its own timeout; stuck phase force-advances after log.

### Topology diagram

```
                 ┌─────────────────────────────────────────────┐
                 │              origin-daemon (1 proc)         │
   IPC clients ──┼─→  control core (Tokio current_thread)      │
   (TUI/script)  │       ├ IPC accept loop                     │
                 │       ├ renderer tick scheduler             │
                 │       ├ event dispatch                      │
                 │       └ shutdown supervisor                 │
                 │                                             │
                 │    worker pool (Tokio multi_thread)         │
                 │       ├ session agent loops      ─ Critical │
                 │       ├ provider HTTP/2 clients  ─ Critical │
                 │       ├ tool execs               ─ Critical │
                 │       ├ swarm workers            ─ Critical │
                 │       ├ sidecar small-model      ─ Sidecar  │
                 │       ├ MCP server clients       ─ Sidecar  │
                 │       ├ CAS GC, vacuum           ─ Background│
                 │       └ initial code-graph build ─ Bulk     │
                 │                                             │
                 │    arenas: agent_*, cas, sidecar, swarm_*…  │
                 │    storage: SQLite (WAL) + CAS pack files   │
                 └─────────────────────────────────────────────┘
                              │
                              │  SMR rings (local) + QUIC (remote)
                              ↓
                  origin-cli (TUI client process)
                       ├ keystroke loop (crossterm)
                       ├ renderer (cell-grid + SIMD diff)
                       ├ scrollback cache (CAS-backed)
                       └ side panel
```

### Failure modes

- Renderer panics → caught at widget boundary; widget shown as `[render error]`.
- Daemon panics → `origin-supervisor` restarts; sessions resume from SQLite + WAL.
- Allocator arena exhaustion → resize to cap (default 256MB/arena); hard cap triggers OOM-soft (evict CAS Hot, drop background tasks) before OS OOM.

---

## Section 9 — Permissions, Skills, Hooks, MCP

### 9A Permissions

**N9.1 Tier + urgency declared at the tool source via macro.** `#[origin_tool(tier=…, urgency=…, side_effects=…)]` emits `const TOOL_META`. No runtime lookup table.

**N9.2 Bloom-filter rule pre-check.** 4KB filter over configured user rules; ~20ns reject for ~95% of queries; wildcard matching only on probable hits. Filter rebuilds incrementally on config change.

**N9.3 Permission prompts are TUI side-panel events, not modals.** `PermissionAsk` event into the side panel; agent loop continues other work. User: `Tab → focus panel → y/n/e → Enter`. Concurrent tool calls don't serialize on the gate.

**Headless default action:** configurable (`deny` default); requests recorded for audit.

### 9B Skills

**N9.4 Embedding-indexed lazy injection.** At skill install, embed `(name + description + first-line-of-body)` into the memory HNSW with kind `Skill`. Same per-turn recall pass that finds memories also proposes skills. Top-K skill bodies materialize into CachePlanner Sticky band. Zero session-start scan cost.

**N9.5 Skills narrow allowed-tools as a typed capability.** Frontmatter:
```yaml
---
name: refactor-rust-module
description: …
allowed-tools: [Read, Glob, Grep, Edit, Bash(cargo check:*), Bash(cargo test:*)]
required-capabilities: [code_graph]
---
```
Permission engine narrows the session's allowed set for the duration of the skill. `required-capabilities` lets a skill refuse to inject when its prerequisites aren't met.

**N9.6 First-run import via content-hashing.** Imports from `~/.claude/skills/`, Codex CLI, etc., dedupe by content hash; user sees one-screen TUI confirmation.

### 9C Hooks

**N9.7 Pre-spawned shell pool.** N=2 default (configurable). Hooks dispatched over stdin with a harness preamble that echoes a delimiter on completion. Dispatch latency ~200µs vs. 5–200ms for spawn-per-event.

**N9.8 Typed event payloads with structured override channel.** Each event kind has a declared output schema (`{ "override": "block" | "modify", … }`). Mismatches log and skip silently. Events: `pre_prompt`, `post_prompt`, `pre_tool`, `post_tool`, `permission_ask`, `session_start`, `session_end`, `worker_spawn`, `worker_complete`, `memory_proposed`, `skill_injected`, `compaction_triggered`.

**N9.9 Hooks dispatched on Sidecar task class.** Never blocks the agent loop. `pre_tool` blocking hook has 1s default timeout; tool proceeds on miss.

### 9D MCP

**N9.10 Connection-per-server with backpressure.** One persistent connection per MCP server; tool calls multiplex via request IDs. Health-monitored; quarantine state on missed heartbeat.

**N9.11 MCP tool schemas in the same registry as native tools.** `McpToolProxy` registered identically; same dispatch path, same permission tiering, same speculative-dispatch rules (only `read*`/`list*` names speculative). Per-server tier overrides in config.

**N9.12 MCP outputs land in CAS just like native tools.** No special-casing.

**N9.13 OAuth via KeyVault.** Device-flow or PKCE; refresh in background; multi-account. Credentials stay in OS keychain.

**Failure modes:**
- Server crash → backoff reconnect; tools marked unavailable with explicit user message.
- Schema drift mid-session → re-register; next turn sees updated schemas.

---

## Section 10 — Errors, observability, testing, security

### 10A Error handling

**N10.1 Two error tiers.** `OriginError` (typed, recoverable, `thiserror`); `Bug` (panics caught at widget/worker/IPC-request/tool-execution boundaries).

**N10.2 Audience-typed routing.** Every error variant declares its audience: `Internal | Operator | Model | User`. Routing is part of the type — wrong audience is a compile error. Provider errors the model can recover from (e.g., invalid tool input) flow back to the model automatically.

**N10.3 Per-error-kind retry policy.** `impl Retryable for ProviderError` declares per-variant policy (`AfterDelay`, `Exponential { base, cap, max }`, `Never`). Generic retry executor; no ad-hoc loops.

### 10B Observability

**N10.4 Structured spans → parquet ring.** Every IPC request, agent turn, tool call, provider call, sidecar job is a `tracing` span with a typed event schema. Spans written to per-day compressed parquet (64MB rotation). Query via `origin trace query` subcommand.

**N10.5 Bounded-cardinality metrics.** Counters/histograms by `(class, provider, tool, error_kind)`. Surfaces: TUI `?metrics` panel, `/metrics` Prometheus socket, optional OTel export.

**N10.6 Live token accounting.** TUI status bar shows `in / out / cache_read / cache_write / cost($) / time` for the current turn, updated as the stream progresses. `origin usage` for rollups.

### 10C Testing

**N10.7 Three tiers with crate-assignment rules.** Unit (pure functions; required for `origin-core`, `origin-permission`, `origin-plan`, `origin-mem`). Component (single crate + mocked deps; required for providers, tools, codegraph, mem, sidecar). End-to-end (daemon + TUI client + recorded provider; required for agent loop, swarm, hooks, MCP, permission flows).

**N10.8 `origin-replay` as a first-class crate.** Records every IPC frame + provider HTTP response + CAS write into a `.origin-replay` bundle. E2E tests load bundles and assert deterministic re-execution. Bundles double as bug repros.

**N10.9 Property tests for CRDT, ranker, FastCDC.** `proptest`-generated random op-log permutations of plans assert deterministic fold; random `(memory, age, edge)` populations assert temporal-decay monotonicity; random byte streams assert FastCDC boundary stability across small edits.

**N10.10 Fuzzing for parsers.** `cargo-fuzz` in CI for streaming JSON, rkyv validator, provider response parsers, FastCDC boundary finder.

### 10D Security & sandboxing

**N10.11 Per-tool sandbox profiles.** Linux: user/mount namespaces + seccomp + landlock. macOS: `sandbox-exec` profiles. Windows: `AppContainer` profiles with restricted Job Object (CPU/RAM caps).
- AutoAllowed tools → strictest profile.
- RequiresPermission tools → user-widenable profile per session.

**N10.12 Hook scripts inherit the triggering tool's profile.** No privilege escalation via hooks.

**N10.13 MCP responses validated against registered schema at the buffer layer.** 16MB cap per response; configurable per server. Schema mismatches rejected before agent exposure.

**N10.14 `Secret<T>` newtype with `Debug = <redacted>`.** Redaction-aware `tracing` fields. CI test enforces no field named `*key*`/`*token*`/`*password*`/`*auth*` emits raw bytes.

**N10.15 Worker process isolation.** Workers run as child processes with their own PID; CPU/RAM caps via cgroup (Linux), Job Objects (Windows), `taskpolicy` (macOS). Daemon can SIGKILL a runaway worker without touching the agent loop.

**N10.16 KeyVault is the single keyring-touching crate.** Typed `Credential<P>` requests; every access logs to a 30-day audit ring separate from trace parquet.

### Failure boundary table

| Failure | Caught at | Survives |
|---|---|---|
| Tool panic | Tool execution boundary | Agent loop, session |
| Worker panic | Swarm supervisor | Coordinator, plan, other workers |
| Widget render panic | Widget boundary | Other widgets, daemon |
| Daemon panic | Supervisor restart | SQLite + CAS (resumable session) |
| Provider 429/5xx | Retry executor | Session, retried turn |
| MCP server crash | MCP transport | All other tools |
| Sandbox violation | OS kernel | Daemon (tool sees `EPERM`) |
| OOM-soft trip | Allocator caps | All sessions (CAS Hot evicted) |
| OOM-hard | Supervisor restart | Last-checkpoint state |

---

## Section 11 — Phase plan (all phases of approach A)

Vertical-slice sequencing. Every phase ends with a shippable harness (feature-flagged where a phase isn't user-visible yet). Solo-engineer estimate: ~48 weeks. With 2 engineers parallelizing P5–P12: ~7–8 months.

### Phase 0 — Workspace + core types + IPC scaffold (weeks 1–2)
- Cargo workspace, CI, MSRV pin, clippy/rustc gates.
- `origin-core` IR types with `rkyv::Archive`.
- `origin-ipc` minimum: socket/pipe framing, request-ID multiplexing, rkyv validation.
- `origin-store` SQLite WAL + `refinery` migrations.
- `origin-daemon` + `origin-cli` handshake (echo).
- **Ships:** nothing user-visible; architecture validated.

### Phase 1 — First end-to-end turn (weeks 3–5)
- `origin-provider` trait + Anthropic non-streaming implementation.
- `origin-tools` minimum 5: `Read`, `Glob`, `Grep`, `Edit`, `Bash` (no speculative dispatch yet).
- `origin-permission` tier macros + interactive modal prompt.
- Agent loop: prompt → request → tool_use → tool_result → done.
- Sessions in SQLite (blobs inline; CAS arrives P2).
- Ratatui-baseline TUI (replaced P4).
- **Ships:** dogfoodable harness, Anthropic only, modal prompts.

### Phase 2 — Streaming + CAS + ring buffer (weeks 6–8)
- Anthropic event-stream parser → shared byte ring.
- `origin-cas`: FastCDC, mmap pack files, refcount, GC.
- Migrate messages to `Handle + preview`.
- Cell-grid scrollback widget reads CAS slices.
- Consume `cache_read_input_tokens`; status-bar token panel.
- **Ships:** flat-RAM long sessions; live streaming feel.

### Phase 3 — CachePlanner + speculative dispatch + Recall (weeks 9–11)
- PrefixLedger, predictive band ordering, cache-marker emission.
- Incremental JSON parser + speculative dispatch (pure tools).
- `Recall` tool + handle substitution in message-to-wire.
- Result memoization via input-hash.
- **Ships:** measured token bill drops; tool chains feel snappier.

### Phase 4 — Custom TUI renderer (weeks 12–15)
- Cell-grid double buffer + SIMD damage diff.
- Frame coalescing tied to event loop.
- Side panel as separate render target; permissions migrate from modal to panel.
- CAS-backed per-turn layout cache.
- Benchmark suite for keystroke→pixel + FPS-under-stream.
- **Ships:** the fast-feeling harness; published numbers.

### Phase 5 — Sidecar + summarization + compaction (weeks 16–18)
- `origin-sidecar` continuous worker (Haiku-class on Anthropic).
- Eager per-turn summarization; tool-output structure extraction.
- Summary-backed compaction.
- Learned-dictionary zstd training at idle.
- **Ships:** long sessions stay performant; first compaction near-free.

### Phase 6 — Memory graph (weeks 19–22)
- ONNX runtime + MiniLM bundled.
- Int8 + per-cluster centroid offsets.
- HNSW + temporal-decay re-rank.
- Memory bodies in CAS, vectors in SQLite, edges in `mem_edges`.
- Auto-save / auto-recall with TUI panel review.
- `mem_search` / `mem_save` / `mem_forget` tools.
- Idle consolidation.
- **Ships:** cross-session learned context.

### Phase 7 — Code graph (weeks 23–26)
- Tree-sitter for top 10 languages (Rust, TS/JS, Python, Go, Java, C/C++, Ruby, PHP, Swift, SQL).
- FastCDC with AST-boundary-biased rolling hash.
- Graph nodes/edges in CAS; communities + god nodes in SQLite.
- Sidecar non-code extraction (PDF/docs/images).
- Leiden + flow-weighted PageRank.
- Typed query DSL + `Ask` router.
- `git commit` hook incremental rebuild.
- **Ships:** native code knowledge graph; replaces grep on most navigation.

### Phase 8 — Provider matrix + KeyVault (weeks 27–29)
- `origin-keyvault` with OS keychains.
- OpenAI, Gemini, Ollama, OpenRouter providers.
- Bedrock, GitHub Models.
- OAuth device-flow / PKCE; refresh rotation.
- Default build adds Anthropic + OpenAI + Gemini + Ollama; rest opt-in.
- **Ships:** full provider matrix; account-switching in TUI.

### Phase 9 — Swarm + plan CRDT + CoW workers (weeks 30–33)
- `origin-plan` op log + Logoot keys + snapshots + lease tokens.
- `origin-swarm` coordinator/worker protocol + completion reports.
- SMR rings for local comms.
- Reflink CoW workspaces (with fallbacks).
- Workers inherit coordinator PrefixLedger.
- Plan side panel in TUI.
- **Ships:** parallel multi-agent refactors with observable plan + DM/broadcast.

### Phase 10 — Extensibility quartet (weeks 34–36)
- Skills: embedding-indexed lazy injection, allowed-tools narrowing, first-run import.
- Hooks: pre-spawned shell pool, full lifecycle events, typed payloads, sidecar-class dispatch.
- MCP: v1.29 client (stdio/HTTP/SSE), OAuth via KeyVault, per-server quarantine.
- Permissions: bloom-filter pre-check, side-panel-only prompts, headless defaults, override channel.
- **Ships:** third-party ecosystem extensions; skill libraries importable.

### Phase 11 — Security + observability + sandboxing (weeks 37–39)
- Sandbox profiles per platform + per-tool.
- Hook script profile inheritance.
- MCP message validation + 16MB cap.
- Tracing → parquet ring + `origin trace query`.
- TUI `?metrics` panel + `/metrics` Prometheus + optional OTel.
- KeyVault audit log.
- `Secret<T>` + CI lint.
- **Ships:** production-grade security/observability posture.

### Phase 12 — Multi-runtime + arenas + cooperative shutdown (weeks 40–42)
- jemalloc named arenas.
- `spawn_in(class, …)` enforced via clippy lint.
- Two-runtime split (control core + worker pool).
- io_uring on Linux for CAS pack file I/O.
- Phased cooperative shutdown supervisor.
- `origin-supervisor` restart-on-panic with session resume.
- **Ships:** sustained heavy use stays flat-RAM; clean shutdown/resume.

### Phase 13 — QUIC remote IPC + headless polish (weeks 43–44)
- QUIC + rustls transport + pairing flow.
- Bearer tokens via KeyVault.
- Headless polish: `origin run "..."`, `--json`.
- `origin trace query`, `origin usage`, `origin sessions ls/resume/rm`, `origin keyring`.
- **Ships:** feature-complete; remote attach + automation.

### Phase 14 — Hardening, docs, GA (weeks 45–48)
- Bug-bash on dogfooded sessions; fuzz CI gates.
- Migration tools: `origin import` for Claude Code / jcode / opencode sessions + skill dirs.
- Large-codebase benchmarks against Claude Code / jcode / opencode on a fixed task set.
- Documentation site + `origin --tutorial`.
- Release engineering: signed binaries (Linux x86_64+aarch64, macOS universal, Windows x86_64+aarch64); Homebrew, winget, cargo-binstall, AUR.
- **Ships:** v1.0 GA.

### Critical path & parallelization

**Hard sequential:** P0 → P1 → P2 → P3. P9 needs P3 + P6 + P7. P11 needs P10.

**Parallelizable:**
- P5 starts in parallel with P3 once P2 is done.
- P6 and P7 run in parallel (independent crates over the shared CAS).
- P8 runs in parallel with P5–P7 (independent provider crates).
- P10 and P11 overlap.
- P12 and P13 overlap.

### Build/release

- MSRV: latest stable at P0 start; bumped quarterly.
- Reproducible builds via `cargo --frozen` + locked `Cargo.lock`.
- Cross-compilation matrix via `cross` + GitHub Actions.
- Binary size budget: < 25MB compressed per platform (LTO + strip + opt-level=z); per-default-build tree-shake provider crates.
- Telemetry: opt-in only; aggregate metrics only (no prompts, no file content); separate `origin-telemetry-opt-in` crate linked only when enabled.

### GA acceptance criteria (P14 exit)

1. End-to-end fuzz + replay suite passes deterministically.
2. Perf gates met:
   - Cold daemon start to first prompt-ready frame: < 50ms.
   - Keystroke-to-pixel: < 12ms p99.
   - Steady RSS under 4-hour heavy session: < 200MB (excluding mmap'd CAS).
   - Cache hit rate on stable sessions: ≥ 70% read tokens.
   - Code-graph incremental rebuild on a 100KLOC monorepo: < 500ms p95.
3. Zero `unsafe` in surface-level crates; `unsafe` only in `origin-cas` (mmap), `origin-tui` (SIMD), `origin-ipc` (shared memory); each audited.
4. Security review pass on sandbox profiles + KeyVault.
5. Three migration paths validated against real user data (Claude Code, jcode, opencode).

---

## Appendix A — Mapping: attributes kept vs novel mechanisms

For each signature subsystem we list (a) what we keep from each source as an **attribute** (concept, capability, surface), and (b) the **novel mechanism** we use to implement it.

| Subsystem | Kept from openclaude | Kept from jcode | Kept from opencode | Novel mechanism (this design) |
|---|---|---|---|---|
| Agent loop | turn-based tool loop, prompt-cache awareness | streaming-first; KV-cache-aware tool output capping | per-message part model | Single-buffer multi-tail ring + speculative dispatch + CachePlanner + handle-substitution + sidecar coroutine (N2.1–N2.5) |
| Provider abstraction | multi-provider via shim | native per-provider trait | AI-SDK style unified surface | Superset IR + compile-time projections + planner-driven encoding (N4.1–N4.3) |
| Tool system | broad tool set + permission rules | tiered permissions + urgency | rule-based wildcard permissions | Compile-time registry + CAS-handle I/O + speculative + memoization + Recall (N5.1–N5.5) |
| Memory | session transcripts | semantic embedding + graph + sidecar verifier | session SQLite | Int8-quantized + centroid offsets + temporal-decay re-rank + body-in-CAS + auto-save side-effects (N6.1–N6.5) |
| Code knowledge | — | — | — *(graphify attribute: tree-sitter + Leiden + god nodes + confidence tags)* | FastCDC AST-boundary chunking + CAS records + sidecar non-code extraction + flow-weighted PageRank + typed query DSL (N6.6–N6.10) |
| Swarm | coordinator mode | coordinator/worker + plan + DMs + broadcasts + worktree isolation | ACP for external agents (deferred) | PrefixLedger inheritance + SPSC SMR rings + reflink CoW + credit backpressure + structured completion reports (N7.1–N7.5) |
| Plan | — | shared plan object | — | CRDT op log + Logoot keys + lease tokens + snapshot compaction (N7.6–N7.7) |
| IPC | — | server/client model | WebSocket SDK | Request-ID-multiplexed rkyv-validated single stream + shared file mapping for blobs + credit-backpressure + QUIC for remote (N7.8–N7.12) |
| TUI | Ink/React | handterm custom renderer concept | OpenTUI/Solid | Cell-grid + SIMD damage diff + event-loop-tied coalescing + CAS-direct streaming render + side panel as separate target (N8.1–N8.5) |
| Daemon runtime | — | Tokio + jemalloc | — | Per-component named arenas + task-class budgeting + two-runtime split + platform-native async I/O + phased shutdown (N8.6–N8.10) |
| Permissions | interactive prompts + settings rules | tiered + urgency | SQLite wildcard rules | Macro-declared tiers + bloom-filter pre-check + side-panel async prompts (N9.1–N9.3) |
| Skills | skills system | embedding-triggered injection | markdown SKILL.md | HNSW-indexed lazy injection + allowed-tools as typed capability narrowing + content-hashed first-run import (N9.4–N9.6) |
| Hooks | lifecycle hooks via settings.json | — | — | Pre-spawned shell pool + typed payloads w/ override channel + sidecar-class dispatch (N9.7–N9.9) |
| MCP | full MCP client | MCP proxies | full MCP 1.27+ client | Connection-per-server backpressure + same registry as native tools + CAS outputs + KeyVault OAuth (N9.10–N9.13) |

---

## Appendix B — Glossary

- **CAS** — content-addressed store; blob layer for the harness.
- **CachePlanner** — predictive prompt-cache prefix layout engine.
- **CRDT** — conflict-free replicated data type; here: the plan op log.
- **FastCDC** — content-defined chunking algorithm with rolling hash.
- **Frozen / Sticky / Sliding / Volatile** — the four prefix bands the CachePlanner manages.
- **HNSW** — Hierarchical Navigable Small World graph; the embedding ANN index.
- **IR** — Intermediate Representation; the superset message format providers project from/to.
- **KeyVault** — `origin-keyvault`; the single crate that touches OS keychains.
- **PrefixLedger** — historical record of which byte ranges hit cache; consumed by CachePlanner.
- **Recall tool** — agent-visible tool that inflates a CAS handle back into the request.
- **Sidecar** — the always-on small-model worker (`origin-sidecar`).
- **SMR ring** — single-producer-single-consumer shared-memory ring buffer for local inter-agent comms.
- **Swarm** — `origin-swarm`; the coordinator/worker subagent system.

---

*End of design.*
