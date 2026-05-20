# Architecture

`origin` is a Cargo workspace of 30+ crates split into a UI-agnostic core
library, a daemon binary that hosts sessions, and a thin client. The daemon
never imports `origin-tui`; the CLI never imports `origin-daemon` internals.
They speak only through `origin-ipc`, which is what lets headless, multi-client,
and future remote/desktop surfaces fall out for free.

## Crate map

The signature subsystems each live in their own crate so they can be
feature-gated and benchmarked independently.

```
origin-core         pure types, rkyv-archived IR (Message, Block, ToolCall)
origin-cas          content-addressed store — the blob layer
origin-provider     trait + per-provider crates (anthropic, openai, gemini, ...)
origin-tools        compile-time tool registry + sandboxed exec
origin-mcp          MCP v1.29 client (stdio/HTTP/SSE)
origin-hooks        lifecycle hook dispatch with pre-spawned shell pool
origin-skills       skill loader, frontmatter, embedding-triggered injection
origin-mem          semantic memory graph + HNSW + sidecar verifier client
origin-codegraph    native code knowledge graph (FastCDC + Leiden + PageRank)
origin-sidecar      always-on small-model worker (compaction, extraction)
origin-swarm        coordinator/worker protocol, SMR rings
origin-plan         CRDT plan op log + snapshots
origin-permission   tier + rule + interactive engine
origin-keyvault     OS-native credential store wrapper
origin-store        SQLite WAL layer + migrations + mmap reads
origin-ipc          daemon IPC (rkyv over Unix socket / named pipe; QUIC remote)
origin-replay       deterministic replay harness
origin-daemon       binary: hosts sessions, plumbs ipc → core → providers
origin-supervisor   restart-on-panic shim for origin-daemon
origin-tui          custom cell-grid renderer with SIMD damage diff
origin-cli          binary: TUI + headless one-shot + admin subcommands
```

`origin-core` types are `rkyv`-archived from day 1, so the same byte buffer
flows through IPC, SQLite blob columns, and in-memory ring buffers with no
serialize/deserialize hops on the hot path. `origin-cas` is the foundational
novelty — tool outputs, file reads, embeddings, memory bodies, and code-graph
nodes/edges all live there, content-addressed and deduped across turns,
sessions, and swarm workers.

## Two-runtime daemon

Inside `origin-daemon`, work is split across two Tokio runtimes so that
control-plane latency never depends on worker-pool saturation. Tasks are
spawned via a `spawn_in(class, fut)` helper enforced by a clippy lint; classes
are `Critical`, `Realtime`, `Sidecar`, `Background`, `Bulk`.

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
```

The control core is pinned to one OS thread and runs `current_thread` — IPC
accept, renderer tick scheduling, event dispatch, and the shutdown supervisor
all live here. The worker pool is `multi_thread` sized to `physical_cores - 1`
and handles agent loops, provider HTTP/2, tool execution, sidecar, MCP, CAS
GC, and code-graph builds. Cross-runtime communication goes through shared-
memory rings + atomics — never `block_on`.

## Storage as one addressable namespace

SQLite holds only indexes and small inline fields; every blob is a
`Handle(hash, preview_64B)` resolved through `origin-cas`. Three tiers share
one namespace: Hot (`Arc<Bytes>` LRU, default 64MB), Warm (mmap'd pack
files, returned as `&[u8]`), Cold (zstd-dict-compressed pack files). Promotion
and demotion are automatic by access frequency, so the agent loop can pretend
storage is uniform while the daemon keeps RSS flat.

For the daemon's internal runtime details (arenas, task classes, shutdown
phases) see the runtime overview embedded in the diagram above; for the
storage schema see [Configuration](configuration.md) and [SDK](sdk.md).
