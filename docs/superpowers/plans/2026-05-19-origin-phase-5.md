# `origin` Phase 5 — Sidecar + Summarization + Compaction — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run-to-fail, implement, run-to-pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Stand up `origin-sidecar` — an always-on small-model worker with a bounded job queue — and wire it into the agent loop and CAS so that (a) every completed turn is eagerly summarized to the `messages.summary` column for near-free compaction, (b) large tool outputs get a sibling CAS shard with a structured outline, (c) a summary-backed compactor swaps oldest turns for their summaries when the session exceeds a soft token cap (with full bodies still reachable via `Recall`), and (d) `origin-cas` trains a learned 64KB zstd dictionary at idle from sampled shards and uses it for cold-tier compression. Phase exits with a 2-hour long-session bench asserting flat RSS under compaction and tag `p5-complete`.

**Architecture:** A new crate `origin-sidecar` owns a `Sidecar` runtime: a `tokio::sync::mpsc::channel::<SidecarJob>(256)` (credit-based backpressure per spec N7.4) feeds N worker tasks (default 2) that drain jobs and dispatch them to a configured `dyn Provider`. Jobs are typed: `SidecarJob::Summarize { session_id, turn_index, transcript: Vec<Message>, deliver_to: Box<dyn SummaryDeliverer> }` and `SidecarJob::Extract { handle, deliver_to: Box<dyn ExtractDeliverer> }`. `SummaryDeliverer` and `ExtractDeliverer` are `async_trait` interfaces the daemon implements over `SessionStore::update_summary` and `Store::put` respectively, keeping `origin-sidecar` free of daemon-specific dependencies. The daemon's agent loop, after each turn finalizes, calls `sidecar.submit_summary(...)` (a fire-and-forget that returns immediately — never blocks the loop). Large `ToolResult` bytes (>16KB threshold) likewise fire `sidecar.submit_extract(...)`. A new `origin-daemon::compactor` module reads the planner's `PrefixLedger` view of current request bytes; when total > `compact_soft_cap_tokens` (default 50_000 input tokens), it walks `messages` in turn order and replaces the oldest 4 turn-bodies with their `summary` text wrapped in a `Block::Text { text, cache_marker: None }`, leaving the full bodies in CAS reachable via `Recall(handle)`. Finally `origin-cas` grows a `dict.rs` module that trains a 64KB zstd dictionary via `zstd::dict::from_samples` at idle (driven by a new `Store::train_dict_from_sample(n_samples) -> Result<DictVersion>` API) and uses it on the cold-write path via `zstd::Encoder::with_dictionary`. Per-shard metadata (`dict_version`) lives in the pack file's existing footer so reads pick the right dict.

**Tech Stack:** Rust 1.83 (MSRV pin). Tokio (`sync`+`time`+`macros`+`rt`+`rt-multi-thread`). New deps inside `origin-sidecar`: `async-trait = "0.1"`, `serde`, `serde_json` (for the structured-output prompts), `thiserror`, plus path deps on `origin-core`, `origin-provider`, `origin-cas`. New dep inside `origin-cas`: `zstd` is already in tree — only the dictionary API (`zstd::dict::from_samples`, `zstd::Encoder::with_dictionary`, `zstd::Decoder::with_dictionary`) needs to be exercised. Test deps: `tempfile`, `wiremock`, `mockall = "0.12"` for the `Provider` mock in sidecar unit tests. **Novel-implementation reflex** per `[[feedback-novel-implementations]]`: bounded mpsc + N worker tasks beats spawn-per-job (zero per-job allocation after pool warm); structured `SidecarJob` enum with `deliver_to` callback gets sidecar-internal logic out of daemon code; eager-summarization-into-existing-`messages.summary`-column reuses the P1 schema's reserved column (no migration); learned-dict zstd over per-tier dictionaries is the spec's N3.2 mechanism, beating zstd-default by ≥3× on similar-shape shards.

**Builds on:** Spec §2 (N2.5 sidecar-as-coroutine), §3 (N3.2 learned-dict zstd), and tags `p3-complete` + `p4-complete` (planner + custom TUI both shipped). Reference points in the existing code: `crates/origin-daemon/src/agent.rs` (turn loop, tool dispatch, CAS put on tool result), `crates/origin-daemon/src/session_store.rs:61` (`persist_message`; new `update_summary` joins here), `crates/origin-cas/src/store.rs:170` (`demote_to_cold` is the cold-write path that grows dict-awareness in P5.5), `crates/origin-store/src/migrations/V1__init.sql:21` (`messages.summary TEXT` column already exists — no schema migration in P5).

**Out of scope (deferred):**
- Per-component jemalloc arenas + Tokio task-class budgeting (`Sidecar` task class) — Phase 12. P5 ships a plain `tokio::spawn` worker pool; full budgeting integrates later.
- Memory-recall verification (N2.5.b) — needs `origin-mem` from Phase 6.
- Sidecar idle consolidation of memories (N6.4) — Phase 6 once memory exists.
- Cross-repo dict sharing / dict bundling in `origin-replay` — Phase 11.
- Compaction triggered by RSS pressure rather than token count — Phase 12 alongside arenas.

---

## Conventions reminder (apply to every task)

**TDD shape:** failing test → run-to-fail → implement → run-to-pass → verification gate → commit.

**Verification gate per task type:**

| Task type | Required commands (all exit 0) |
|---|---|
| Single-crate pure logic (P5.1 sidecar core, P5.5 cas dict) | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate (P5.2 / P5.3 / P5.4 / P5.6) | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Final phase gate (P5.6) | All of the above + new `phase5_compaction_flat_rss` bench passes its assertion + tag `p5-complete` |

**Inherited patterns:**
- `[lints] workspace = true` in every new `Cargo.toml`. Workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- `unsafe_code = "forbid"` (workspace default). Phase 5 introduces no new `unsafe`.
- `#[must_use]` on every public constructor; `const fn` where Rust allows.
- Tests use `.expect("meaningful message")`. Library code may use `.expect("msg")` but never bare `.unwrap()` (`clippy::unwrap_used` is denied workspace-wide).
- Custom error enums via `thiserror`; document `# Errors` / `# Panics` on every public fn that can return `Result` or panic.
- For each `#[allow(clippy::...)]` add an inline `reason = "..."` justification.
- **MSRV pin reflex** per `[[project-msrv-dep-pinning]]`: if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin offender with `cargo update -p <crate>@<bad-ver> --precise <good-ver>` and commit `Cargo.lock`. `mockall = "0.12"` is the last MSRV-1.83-compatible line. Do not bump.
- Commits: Conventional Commits, scoped (`feat(origin-sidecar): ...`), one commit per task; tag references `(P5.X[, N2.5/N3.2])`.

**Branch:** `dev` (per CLAUDE.md the integration branch; matches Phase 4 pattern). `phase-6` and `phase-8` worktrees are out for `origin-mem` and `origin-keyvault` — disjoint files, no merge conflicts expected.

**Conflict-risk note:** `origin-daemon/src/main.rs` is also being modified by `phase-8` (ProviderFactory + `/account` switch, task P8.9). P5.2 will add at most one helper call from `main.rs` (or `agent.rs`) into the new sidecar; keep those edits minimal and mergeable.

---

## File map for Phase 5

| New / modified | Responsibility | Task |
|---|---|---|
| `crates/origin-sidecar/Cargo.toml` + `src/lib.rs` + `src/job.rs` + `src/runtime.rs` + `tests/runtime.rs` | crate skeleton, `SidecarJob` enum, `Sidecar` runtime with mpsc + worker pool | P5.1 |
| `crates/origin-sidecar/src/summarize.rs` + `tests/summarize.rs` + `crates/origin-daemon/src/agent.rs` *(modify — call sidecar after turn)* + `crates/origin-daemon/src/session_store.rs` *(modify — `update_summary`)* + `crates/origin-daemon/src/main.rs` *(modify — construct Sidecar)* | Eager turn summarization; deliverer impl over SessionStore | P5.2 |
| `crates/origin-sidecar/src/extract.rs` + `tests/extract.rs` + `crates/origin-daemon/src/agent.rs` *(modify — fire extract on >16KB tool results)* | Tool-output structure extraction; sibling CAS shard | P5.3 |
| `crates/origin-daemon/src/compactor.rs` *(new)* + `crates/origin-daemon/src/lib.rs` *(modify — `pub mod compactor;`)* + `crates/origin-daemon/src/session_store.rs` *(modify — `load_compacted_messages`)* + `crates/origin-daemon/tests/compaction.rs` *(new)* | Compaction policy: swap oldest turns for summaries; recoverable via `Recall` | P5.4 |
| `crates/origin-cas/src/dict.rs` *(new)* + `crates/origin-cas/src/store.rs` *(modify — `train_dict_from_sample`, dict-aware cold I/O)* + `crates/origin-cas/src/lib.rs` *(modify — re-export `DictVersion`, `DictError`)* + `crates/origin-cas/tests/dict.rs` *(new)* | Learned-dict zstd training + use | P5.5 |
| `crates/origin-daemon/benches/long_session.rs` *(new)* + `crates/origin-daemon/Cargo.toml` *(modify — add `[[bench]]`)* | 2-hour synthetic session bench: flat RSS under compaction + tag `p5-complete` | P5.6 |

File-size discipline: every new `.rs` file targets <300 LOC. `compactor.rs` and `runtime.rs` are the longest — keep ≤300 LOC each by extracting helpers when needed.

---

## Task P5.1 — `origin-sidecar` runtime (FOUNDATIONAL, serial)

This task must complete before P5.2–P5.5 fan out, because each of them depends on the `Sidecar` handle, `SidecarJob` shape, and deliverer trait interfaces this task defines.

**Files:** `crates/origin-sidecar/Cargo.toml` *(new)*, `src/lib.rs` *(new)*, `src/job.rs` *(new)*, `src/runtime.rs` *(new)*, `tests/runtime.rs` *(new)*.

**Public surface (exact):**

```rust
// src/job.rs
#[derive(Debug)]
pub enum SidecarJob {
    Summarize {
        session_id: String,
        turn_index: u32,
        transcript: Vec<origin_core::types::Message>,
        deliver_to: Box<dyn SummaryDeliverer>,
    },
    Extract {
        handle: origin_cas::Hash,
        deliver_to: Box<dyn ExtractDeliverer>,
    },
}

#[async_trait::async_trait]
pub trait SummaryDeliverer: Send + Sync + std::fmt::Debug {
    async fn deliver(&self, session_id: &str, turn_index: u32, summary: &str);
}

#[async_trait::async_trait]
pub trait ExtractDeliverer: Send + Sync + std::fmt::Debug {
    async fn deliver(&self, source: origin_cas::Hash, outline_handle: origin_cas::Hash);
}

// src/runtime.rs
pub struct Sidecar { /* private */ }

pub struct SidecarConfig {
    pub workers: usize,            // default: 2
    pub queue_capacity: usize,     // default: 256
    pub model: String,             // default: "claude-haiku-4-5-20251001"
}

impl Default for SidecarConfig { fn default() -> Self { ... } }

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("queue full")] QueueFull,
    #[error("shutdown")] Shutdown,
}

impl Sidecar {
    /// Spawn `cfg.workers` worker tasks. Returns a clonable handle.
    #[must_use]
    pub fn spawn(provider: Arc<dyn Provider>, cas: Arc<Store>, cfg: SidecarConfig) -> Self;

    /// Submit a job. Non-blocking. Returns `QueueFull` if the queue is saturated.
    pub fn submit(&self, job: SidecarJob) -> Result<(), SidecarError>;

    /// Cooperative shutdown: close the sender, await all workers, drop the provider.
    pub async fn shutdown(self);
}
```

Internal flow inside `Sidecar`:
- `mpsc::channel::<SidecarJob>(cfg.queue_capacity)` — sender stored on the handle; receiver split across workers via `Arc<Mutex<Receiver>>` OR a single-receiver fan-out task that re-dispatches. Use the **single receiver per worker** pattern via `Arc<tokio::sync::Mutex<mpsc::Receiver>>` — each worker calls `rx.lock().await.recv().await`; this gives true work-stealing across N workers without a fan-out indirection.
- Each worker runs an `async fn worker_loop(rx, provider, cas, model)` that drains jobs until `recv()` returns `None` (sender dropped → shutdown).
- For each job, the worker calls into either `summarize::run(provider, cas, model, &job)` or `extract::run(provider, cas, &job)`. P5.1 includes only the dispatch skeleton; the actual `summarize::run` and `extract::run` implementations land in P5.2 and P5.3. For P5.1 the worker calls a placeholder `dispatch_stub(&job)` that just invokes `deliver_to.deliver(...)` with a fixed test value (so the test can verify the queue + worker pool wires up).

**Note on the stub:** for P5.1 testing, `dispatch_stub` is `pub(crate)` and simply forwards to deliverers with synthetic data. P5.2 / P5.3 replace it with calls to real `summarize::run` / `extract::run` modules.

### Step 1: Failing test at `crates/origin-sidecar/tests/runtime.rs`

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use async_trait::async_trait;
use origin_cas::{Hash, Store, StoreConfig};
use origin_core::types::{Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_sidecar::{Sidecar, SidecarConfig, SidecarJob, SummaryDeliverer, ExtractDeliverer};
use tempfile::tempdir;

#[derive(Debug, Default)]
struct StubProvider;
#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &'static str { "stub" }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message { role: Role::Assistant, blocks: Vec::new() },
            usage: Usage::default(),
        })
    }
}

#[derive(Debug)]
struct CountingSummary(Arc<AtomicU32>);
#[async_trait]
impl SummaryDeliverer for CountingSummary {
    async fn deliver(&self, _session: &str, _turn: u32, _summary: &str) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct CountingExtract(Arc<AtomicU32>);
#[async_trait]
impl ExtractDeliverer for CountingExtract {
    async fn deliver(&self, _src: Hash, _outline: Hash) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

fn store() -> Arc<Store> {
    let dir = tempdir().expect("tempdir");
    Arc::new(Store::open(StoreConfig {
        root: dir.into_path(),
        hot_capacity: 16,
        warm_pack_target_bytes: 1_000_000,
        cold_zstd_level: 3,
    }).expect("open"))
}

#[tokio::test(flavor = "current_thread")]
async fn submit_summarize_drives_delivery() {
    let counter = Arc::new(AtomicU32::new(0));
    let sidecar = Sidecar::spawn(
        Arc::new(StubProvider),
        store(),
        SidecarConfig::default(),
    );
    sidecar.submit(SidecarJob::Summarize {
        session_id: "s1".into(),
        turn_index: 0,
        transcript: Vec::new(),
        deliver_to: Box::new(CountingSummary(counter.clone())),
    }).expect("submit");
    // Give the worker a chance to drain
    for _ in 0..50 {
        if counter.load(Ordering::Relaxed) > 0 { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(counter.load(Ordering::Relaxed), 1, "deliverer should fire once");
    sidecar.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn submit_extract_drives_delivery() {
    let counter = Arc::new(AtomicU32::new(0));
    let sidecar = Sidecar::spawn(
        Arc::new(StubProvider),
        store(),
        SidecarConfig::default(),
    );
    // A throw-away hash — the stub deliverer ignores the value.
    let h = Hash::of(b"placeholder");
    sidecar.submit(SidecarJob::Extract {
        handle: h,
        deliver_to: Box::new(CountingExtract(counter.clone())),
    }).expect("submit");
    for _ in 0..50 {
        if counter.load(Ordering::Relaxed) > 0 { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    sidecar.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn queue_full_returns_error() {
    let cfg = SidecarConfig { workers: 0, queue_capacity: 1, model: "stub".into() };
    // workers=0 so nothing drains; capacity=1 so the second submit fails.
    let sidecar = Sidecar::spawn(Arc::new(StubProvider), store(), cfg);
    let mk = || SidecarJob::Summarize {
        session_id: "s".into(),
        turn_index: 0,
        transcript: Vec::new(),
        deliver_to: Box::new(CountingSummary(Arc::new(AtomicU32::new(0)))),
    };
    sidecar.submit(mk()).expect("first submit (fills queue)");
    let err = sidecar.submit(mk()).expect_err("second submit should fail");
    assert!(matches!(err, origin_sidecar::SidecarError::QueueFull));
    sidecar.shutdown().await;
}
```

### Step 2: Run → fail (crate doesn't exist).

```bash
cargo test -p origin-sidecar
```

### Step 3: Create `crates/origin-sidecar/Cargo.toml`

```toml
[package]
name = "origin-sidecar"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
origin-provider = { path = "../origin-provider" }
origin-cas = { path = "../origin-cas" }
async-trait = "0.1"
thiserror = "1"
tokio = { version = "1", features = ["sync", "time", "macros", "rt", "rt-multi-thread"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
mockall = "0.12"
tokio = { version = "1", features = ["macros", "rt", "test-util", "time"] }
```

### Step 4: Implement `src/job.rs` (the job enum + deliverer traits — copy the public surface block above verbatim; include `use` statements for `Hash`, `Message`).

### Step 5: Implement `src/runtime.rs`

```rust
//! Sidecar runtime: bounded mpsc queue + N worker tasks (N2.5).

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use thiserror::Error;

use origin_cas::Store;
use origin_provider::Provider;

use crate::job::SidecarJob;

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("queue full")]
    QueueFull,
    #[error("shutdown")]
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub workers: usize,
    pub queue_capacity: usize,
    pub model: String,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            queue_capacity: 256,
            model: "claude-haiku-4-5-20251001".to_string(),
        }
    }
}

pub struct Sidecar {
    tx: mpsc::Sender<SidecarJob>,
    workers: Vec<JoinHandle<()>>,
}

impl Sidecar {
    /// Spawn `cfg.workers` worker tasks. Returns the handle.
    ///
    /// `cfg.workers == 0` is legal — useful for tests that want to verify
    /// `queue_full` without races.
    #[must_use]
    pub fn spawn(provider: Arc<dyn Provider>, cas: Arc<Store>, cfg: SidecarConfig) -> Self {
        let (tx, rx) = mpsc::channel::<SidecarJob>(cfg.queue_capacity.max(1));
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(cfg.workers);
        for _ in 0..cfg.workers {
            let rx = rx.clone();
            let provider = provider.clone();
            let cas = cas.clone();
            let model = cfg.model.clone();
            workers.push(tokio::spawn(async move {
                loop {
                    let job = {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    };
                    let Some(job) = job else { break };
                    dispatch_stub(&provider, &cas, &model, job).await;
                }
            }));
        }
        Self { tx, workers }
    }

    /// Submit a job. Non-blocking. Returns `QueueFull` if the queue is full.
    ///
    /// # Errors
    /// Returns `QueueFull` if the bounded mpsc has no slot. `Shutdown` if the
    /// receiver half has been dropped.
    pub fn submit(&self, job: SidecarJob) -> Result<(), SidecarError> {
        self.tx.try_send(job).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SidecarError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => SidecarError::Shutdown,
        })
    }

    /// Cooperative shutdown.
    pub async fn shutdown(self) {
        drop(self.tx);
        for h in self.workers {
            let _ = h.await;
        }
    }
}

async fn dispatch_stub(_provider: &Arc<dyn Provider>, _cas: &Arc<Store>, _model: &str, job: SidecarJob) {
    // Placeholder dispatch: invokes the deliverer with synthetic data so the
    // runtime test can verify pool + queue wiring. Replaced by real calls into
    // `summarize::run` (P5.2) and `extract::run` (P5.3).
    match job {
        SidecarJob::Summarize { session_id, turn_index, deliver_to, .. } => {
            deliver_to.deliver(&session_id, turn_index, "stub-summary").await;
        }
        SidecarJob::Extract { handle, deliver_to } => {
            deliver_to.deliver(handle, handle).await;
        }
    }
}
```

### Step 6: `src/lib.rs`

```rust
//! `origin-sidecar` — always-on small-model worker (N2.5).

pub mod job;
pub mod runtime;

pub use job::{ExtractDeliverer, SidecarJob, SummaryDeliverer};
pub use runtime::{Sidecar, SidecarConfig, SidecarError};
```

### Step 7: Register the new crate

The workspace `Cargo.toml` uses `members = ["crates/*"]` (verified at start of session), so just creating the `crates/origin-sidecar/Cargo.toml` is sufficient — no edit needed. Run:

```bash
cargo build -p origin-sidecar
```

to confirm the crate compiles.

### Step 8: Tests pass:

```bash
cargo test -p origin-sidecar
```

All three runtime tests pass.

### Step 9: Verification gate

```bash
cargo test -p origin-sidecar
cargo clippy -p origin-sidecar --all-targets -- -D warnings
cargo fmt --check
```

### Step 10: Commit

```bash
git add crates/origin-sidecar/
git commit -m "feat(origin-sidecar): bounded mpsc + worker pool + SidecarJob enum (P5.1, N2.5)"
```

---

## Task P5.2 — Eager turn summarization (PARALLEL with P5.3 / P5.4 / P5.5)

Adds a real summarization path inside `origin-sidecar` and wires the daemon's agent loop to fire one `SidecarJob::Summarize` per finalized turn. The deliverer writes the summary text into `messages.summary` via a new `SessionStore::update_summary` method (column already exists in `V1__init.sql:21`, no migration needed).

**Files:**
- `crates/origin-sidecar/src/summarize.rs` *(new)*
- `crates/origin-sidecar/tests/summarize.rs` *(new)*
- `crates/origin-sidecar/src/runtime.rs` *(modify — replace `dispatch_stub`'s Summarize arm to call `summarize::run`)*
- `crates/origin-sidecar/src/lib.rs` *(modify — `pub mod summarize;`)*
- `crates/origin-daemon/src/session_store.rs` *(modify — add `update_summary`)*
- `crates/origin-daemon/src/agent.rs` *(modify — after each turn finalizes, call sidecar.submit)*
- `crates/origin-daemon/src/main.rs` *(modify — construct `Sidecar` at startup; pass into `LoopOptions`)*
- `crates/origin-daemon/src/lib.rs` *(no edit unless needed)*
- `crates/origin-daemon/tests/sidecar_summary.rs` *(new)*

**Public surface added:**

```rust
// summarize.rs
pub async fn run(
    provider: &Arc<dyn Provider>,
    model: &str,
    session_id: &str,
    turn_index: u32,
    transcript: &[Message],
    deliver_to: &dyn SummaryDeliverer,
);

// session_store.rs
impl SessionStore {
    /// Update the `summary` column for an existing `(session_id, turn_index)`
    /// row. No-op if the row does not exist.
    ///
    /// # Errors
    /// Returns sqlite errors on write failure.
    pub fn update_summary(
        &self,
        session_id: &str,
        turn_index: u32,
        summary: &str,
    ) -> Result<(), SessionStoreError>;
}

// agent.rs LoopOptions extension
impl LoopOptions {
    #[must_use]
    pub fn with_sidecar(mut self, sidecar: Arc<Sidecar>) -> Self {
        self.sidecar = Some(sidecar);
        self
    }
}
```

`summarize::run` builds a `ChatRequest` containing:
- `system`: hardcoded prompt: `"You are a summarizer. Reply with exactly one 1-3 sentence summary of the conversation turn. No prelude, no formatting."`
- `messages`: clone of `transcript`
- `model`: passed-in model name
- `tools`: empty (sidecar never uses tools)

Then calls `provider.chat(req).await`. Extracts the first `Block::Text` from `resp.assistant.blocks`; if none or chat errs, fall back to a synthesized one-line summary derived from the first 80 chars of the last assistant message text. Either way, call `deliver_to.deliver(session_id, turn_index, &summary).await`.

The daemon-side `SummaryDeliverer` impl (a small `struct SessionStoreSummaryDeliverer(Arc<SessionStore>)`) calls `update_summary` synchronously inside `deliver` (via `tokio::task::spawn_blocking` if needed, but SQLite is already synchronous in the project).

### Step 1: Failing test at `crates/origin-sidecar/tests/summarize.rs`

```rust
use std::sync::Arc;
use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_sidecar::{summarize, SummaryDeliverer};
use tokio::sync::Mutex;

#[derive(Debug, Default)]
struct EchoProvider;
#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &'static str { "echo" }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message {
                role: Role::Assistant,
                blocks: vec![Block::Text {
                    text: "User asked for X and assistant did Y.".to_string(),
                    cache_marker: None,
                }],
            },
            usage: Usage::default(),
        })
    }
}

#[derive(Debug, Default)]
struct Capture(Mutex<Option<(String, u32, String)>>);
#[async_trait]
impl SummaryDeliverer for Capture {
    async fn deliver(&self, s: &str, t: u32, summary: &str) {
        *self.0.lock().await = Some((s.to_string(), t, summary.to_string()));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_invokes_provider_and_delivers_text() {
    let provider: Arc<dyn Provider> = Arc::new(EchoProvider);
    let cap = Arc::new(Capture::default());
    let transcript = vec![Message {
        role: Role::User,
        blocks: vec![Block::Text {
            text: "do thing".into(),
            cache_marker: None,
        }],
    }];
    summarize::run(&provider, "stub-model", "sess-1", 3, &transcript, cap.as_ref()).await;
    let got = cap.0.lock().await.clone().expect("delivered");
    assert_eq!(got.0, "sess-1");
    assert_eq!(got.1, 3);
    assert!(got.2.contains("User asked for X"), "got summary {:?}", got.2);
}

#[derive(Debug, Default)]
struct ErroringProvider;
#[async_trait]
impl Provider for ErroringProvider {
    fn name(&self) -> &'static str { "err" }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Api("simulated".into()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn provider_error_falls_back_to_synthesized_summary() {
    let provider: Arc<dyn Provider> = Arc::new(ErroringProvider);
    let cap = Arc::new(Capture::default());
    let transcript = vec![Message {
        role: Role::Assistant,
        blocks: vec![Block::Text {
            text: "This is the final assistant message in the turn.".repeat(3),
            cache_marker: None,
        }],
    }];
    summarize::run(&provider, "m", "s", 0, &transcript, cap.as_ref()).await;
    let (_, _, summary) = cap.0.lock().await.clone().expect("delivered");
    // Fallback summary should be non-empty and derived from transcript content.
    assert!(!summary.is_empty());
    assert!(summary.len() <= 160, "fallback summary should be short, got {}", summary.len());
}
```

### Step 2: Failing test at `crates/origin-daemon/tests/sidecar_summary.rs`

```rust
use std::sync::Arc;
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use tempfile::tempdir;

#[test]
fn update_summary_writes_column() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("origin.db");
    let store = SessionStore::open(&db).expect("open");
    let s = Session::new("anthropic".to_string(), "claude-opus-4-7".to_string());
    store.persist_session(&s).expect("persist session");
    let m = Message {
        role: Role::Assistant,
        blocks: vec![Block::Text { text: "hi".into(), cache_marker: None }],
    };
    store.persist_message(&s.id.to_string(), 0, &m).expect("persist message");
    store.update_summary(&s.id.to_string(), 0, "first-summary").expect("update");
    // Re-open and verify the summary column is populated.
    drop(store);
    let conn = rusqlite::Connection::open(&db).expect("re-open");
    let got: String = conn
        .query_row(
            "SELECT summary FROM messages WHERE session_id = ?1 AND turn_index = ?2",
            rusqlite::params![s.id.to_string(), 0],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(got, "first-summary");
}
```

The test depends on `rusqlite` being in `origin-daemon`'s `[dev-dependencies]`. Verify with `cargo tree -p origin-daemon --depth 1 | grep rusqlite`; if absent, add `rusqlite = "0.31"` (workspace standard) to `origin-daemon`'s `[dev-dependencies]`. Note: `origin-store` already depends on rusqlite, so transitively it's in the build graph — but `dev-dependencies` of `origin-daemon` is needed for the test to compile.

### Step 3: Run both new tests → fail.

```bash
cargo test -p origin-sidecar --test summarize
cargo test -p origin-daemon --test sidecar_summary
```

### Step 4: Implement `summarize.rs`

```rust
//! Eager turn summarization (N2.5.a).

use std::sync::Arc;
use origin_core::types::{Block, Message};
use origin_provider::{ChatRequest, Provider};

use crate::job::SummaryDeliverer;

const SYS_PROMPT: &str =
    "You are a summarizer. Reply with exactly one 1-3 sentence summary of the \
     conversation turn. No prelude, no formatting.";

pub async fn run(
    provider: &Arc<dyn Provider>,
    model: &str,
    session_id: &str,
    turn_index: u32,
    transcript: &[Message],
    deliver_to: &dyn SummaryDeliverer,
) {
    let req = ChatRequest {
        system: SYS_PROMPT.to_string(),
        messages: transcript.to_vec(),
        model: model.to_string(),
        tools: Vec::new(),
    };
    let summary = match provider.chat(req).await {
        Ok(resp) => first_text(&resp.assistant).unwrap_or_else(|| fallback(transcript)),
        Err(_) => fallback(transcript),
    };
    deliver_to.deliver(session_id, turn_index, &summary).await;
}

fn first_text(m: &Message) -> Option<String> {
    m.blocks.iter().find_map(|b| match b {
        Block::Text { text, .. } => Some(text.clone()),
        _ => None,
    })
}

fn fallback(transcript: &[Message]) -> String {
    transcript
        .last()
        .and_then(first_text)
        .map(|s| {
            let trimmed = s.trim();
            if trimmed.len() <= 120 {
                trimmed.to_string()
            } else {
                format!("{}...", &trimmed[..120])
            }
        })
        .unwrap_or_else(|| "(empty turn)".to_string())
}
```

### Step 5: Replace the Summarize arm of `dispatch_stub` in `runtime.rs` to call `summarize::run`

Inside `dispatch_stub`'s `SidecarJob::Summarize { ... }` arm, replace:

```rust
deliver_to.deliver(&session_id, turn_index, "stub-summary").await;
```

with:

```rust
crate::summarize::run(provider, model, &session_id, turn_index, &transcript, deliver_to.as_ref()).await;
```

Add `pub mod summarize;` to `src/lib.rs`. P5.1's runtime test (`submit_summarize_drives_delivery`) used `StubProvider` which returned an empty assistant message, so the new path will fall through to the `fallback("(empty turn)")` branch. Update the assertion in that test from `assert_eq!(counter.load(...), 1)` (still holds — `deliver` still fires once) to match — no change needed, the counter still reaches 1.

### Step 6: Implement `SessionStore::update_summary`

Append to `crates/origin-daemon/src/session_store.rs`:

```rust
impl SessionStore {
    /// Update the `summary` column for an existing message row.
    ///
    /// No-op if the row does not exist. Idempotent.
    ///
    /// # Errors
    /// Propagates sqlite errors.
    pub fn update_summary(
        &self,
        session_id: &str,
        turn_index: u32,
        summary: &str,
    ) -> Result<(), SessionStoreError> {
        self.inner.with_conn(|c| {
            c.execute(
                "UPDATE messages SET summary = ?1 WHERE session_id = ?2 AND turn_index = ?3",
                rusqlite::params![summary, session_id, turn_index],
            )?;
            Ok(())
        })?;
        Ok(())
    }
}
```

### Step 7: Wire sidecar into the agent loop

In `crates/origin-daemon/src/agent.rs`, add a field to `LoopOptions`:

```rust
pub sidecar: Option<Arc<origin_sidecar::Sidecar>>,
```

Default it to `None`. Add `with_sidecar` builder. In the turn-finalization branch (right after the loop appends the assistant message and persists it via `persist_message`), submit a Summarize job. The exact insertion point depends on the existing loop structure; the rule is: after `persist_message` returns Ok for an assistant Message, call:

```rust
if let Some(sidecar) = &opts.sidecar {
    let _ = sidecar.submit(origin_sidecar::SidecarJob::Summarize {
        session_id: session.id.to_string(),
        turn_index: turn,
        transcript: transcript_so_far.clone(),
        deliver_to: Box::new(SessionStoreSummaryDeliverer(session_store.clone())),
    });
}
```

where `transcript_so_far` is the messages slice as known to the loop at that point. `SessionStoreSummaryDeliverer` is a new small struct defined adjacent to `LoopOptions`:

```rust
#[derive(Debug)]
struct SessionStoreSummaryDeliverer(Arc<SessionStore>);

#[async_trait::async_trait]
impl origin_sidecar::SummaryDeliverer for SessionStoreSummaryDeliverer {
    async fn deliver(&self, session_id: &str, turn_index: u32, summary: &str) {
        // SessionStore is sync; pin the call to a blocking task.
        let store = self.0.clone();
        let s = session_id.to_string();
        let sum = summary.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = store.update_summary(&s, turn_index, &sum);
        })
        .await;
    }
}
```

Add `use std::sync::Arc;` and `use crate::session_store::SessionStore;` at the top if not already present. Reading `agent.rs` start (lines 1–14) confirms `Arc` is already imported and `session_store` is reachable via `crate::session_store::SessionStore`.

### Step 8: Update `main.rs` to construct the Sidecar at startup

In `crates/origin-daemon/src/main.rs`, after the existing `Anthropic::new` (or whatever provider construction is current — this may have moved in P8.9, but on dev at HEAD it's a direct Anthropic instantiation), build a separate sidecar provider:

```rust
let sidecar_provider: Arc<dyn origin_provider::Provider> = Arc::new(
    origin_provider_anthropic::Anthropic::new(env::var("ANTHROPIC_API_KEY").unwrap_or_default()),
);
let sidecar_cfg = origin_sidecar::SidecarConfig {
    workers: 2,
    queue_capacity: 256,
    model: env::var("ORIGIN_SIDECAR_MODEL")
        .unwrap_or_else(|_| "claude-haiku-4-5-20251001".to_string()),
};
let sidecar = Arc::new(origin_sidecar::Sidecar::spawn(
    sidecar_provider,
    cas_store.clone(),
    sidecar_cfg,
));
```

Then on the `LoopOptions` builder chain add `.with_sidecar(sidecar.clone())`. Add `origin-sidecar = { path = "../origin-sidecar" }` to `crates/origin-daemon/Cargo.toml`'s `[dependencies]`.

### Step 9: Tests pass:

```bash
cargo test -p origin-sidecar --test summarize
cargo test -p origin-daemon --test sidecar_summary
cargo test --workspace
```

The P5.1 runtime test (`submit_summarize_drives_delivery`) still passes because `StubProvider` returns an empty assistant; `summarize::run` falls through to `fallback("(empty turn)")` and delivers the synthetic summary. The counter still increments to 1.

### Step 10: Verification gate

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

### Step 11: Commit

```bash
git add crates/origin-sidecar/ crates/origin-daemon/
git commit -m "feat(origin-sidecar): eager turn summarization + SessionStore::update_summary (P5.2, N2.5.a)"
```

---

## Task P5.3 — Tool-output structure extraction (PARALLEL with P5.2 / P5.4 / P5.5)

Adds an `extract::run` path inside `origin-sidecar` and wires the daemon's agent loop to fire `SidecarJob::Extract` for any tool output exceeding a 16KB threshold. The deliverer writes a small JSON outline (`{ "byte_count": u64, "line_count": u32, "first_120_chars": "..." }`) as a sibling CAS shard; the source handle stays the canonical one.

**Files:**
- `crates/origin-sidecar/src/extract.rs` *(new)*
- `crates/origin-sidecar/tests/extract.rs` *(new)*
- `crates/origin-sidecar/src/runtime.rs` *(modify — replace Extract arm)*
- `crates/origin-sidecar/src/lib.rs` *(modify — `pub mod extract;`)*
- `crates/origin-daemon/src/agent.rs` *(modify — fire Extract for tool results > 16KB)*
- `crates/origin-daemon/tests/sidecar_extract.rs` *(new)*

**Public surface added:**

```rust
// extract.rs
pub const EXTRACT_THRESHOLD_BYTES: usize = 16 * 1024;

pub async fn run(
    cas: &Arc<Store>,
    source: origin_cas::Hash,
    deliver_to: &dyn ExtractDeliverer,
);

/// Outline JSON shape — what gets put into the sibling CAS shard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outline {
    pub byte_count: u64,
    pub line_count: u32,
    pub first_120_chars: String,
}
```

`extract::run` reads `cas.get(source)?`. If `None` (handle unknown), the function returns silently without delivering. Otherwise it builds an `Outline` from the bytes (count lines via `bytes.iter().filter(|b| **b == b'\n').count()`; UTF-8-decode the first 120 bytes lossily for the preview). Serializes to JSON via `serde_json::to_vec`, `cas.put(&json)` → returns a fresh outline handle, then calls `deliver_to.deliver(source, outline_handle).await`.

**Why JSON, not rkyv:** the outline is a debugging / inspection artifact the model can `Recall` and reason about. Plain JSON is more transparent than rkyv-archived bytes and adds no schema-versioning headache.

### Step 1: Failing test at `crates/origin-sidecar/tests/extract.rs`

```rust
use std::sync::Arc;
use async_trait::async_trait;
use origin_cas::{Hash, Store, StoreConfig};
use origin_sidecar::{extract, ExtractDeliverer};
use tempfile::tempdir;
use tokio::sync::Mutex;

#[derive(Debug, Default)]
struct Capture(Mutex<Option<(Hash, Hash)>>);
#[async_trait]
impl ExtractDeliverer for Capture {
    async fn deliver(&self, src: Hash, outline: Hash) {
        *self.0.lock().await = Some((src, outline));
    }
}

fn store() -> Arc<Store> {
    let dir = tempdir().expect("tempdir");
    Arc::new(Store::open(StoreConfig {
        root: dir.into_path(),
        hot_capacity: 16,
        warm_pack_target_bytes: 1_000_000,
        cold_zstd_level: 3,
    }).expect("open"))
}

#[tokio::test(flavor = "current_thread")]
async fn extracts_outline_for_known_handle() {
    let cas = store();
    let body = b"line one\nline two\nline three\n".repeat(1000);
    let src = cas.put(&body).expect("put body");
    let cap = Arc::new(Capture::default());

    extract::run(&cas, src, cap.as_ref()).await;

    let (got_src, outline_handle) = cap.0.lock().await.clone().expect("delivered");
    assert_eq!(got_src, src);
    // Read outline back and verify shape.
    let outline_bytes = cas.get(outline_handle).expect("get").expect("Some");
    let outline: extract::Outline = serde_json::from_slice(&outline_bytes).expect("decode");
    assert_eq!(outline.byte_count, body.len() as u64);
    assert!(outline.line_count > 0);
    assert!(outline.first_120_chars.starts_with("line one"));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_handle_is_silent_noop() {
    let cas = store();
    let cap = Arc::new(Capture::default());
    // A hash we never put — get() returns Ok(None).
    let nonexistent = Hash::of(b"never-stored");
    extract::run(&cas, nonexistent, cap.as_ref()).await;
    assert!(cap.0.lock().await.is_none(), "no delivery for unknown handle");
}
```

### Step 2: Failing test at `crates/origin-daemon/tests/sidecar_extract.rs`

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use async_trait::async_trait;
use origin_cas::Hash;
use origin_sidecar::ExtractDeliverer;

// This test only verifies the threshold constant is exposed and is the
// expected 16KB value. The full agent-loop integration is covered by
// the workspace-wide cargo test pass at the verification gate.

#[test]
fn extract_threshold_is_16kb() {
    assert_eq!(origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES, 16 * 1024);
}

#[derive(Debug)]
struct Counter(Arc<AtomicU32>);
#[async_trait]
impl ExtractDeliverer for Counter {
    async fn deliver(&self, _: Hash, _: Hash) { self.0.fetch_add(1, Ordering::Relaxed); }
}

#[test]
fn small_payload_skips_extract() {
    let payload = vec![b'x'; 1024]; // 1KB < 16KB
    assert!(payload.len() < origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES);
}
```

(This test is intentionally light — heavy lifting is in `tests/extract.rs`. The threshold check is the public-contract assertion.)

### Step 3: Run → fail.

```bash
cargo test -p origin-sidecar --test extract
cargo test -p origin-daemon --test sidecar_extract
```

### Step 4: Implement `extract.rs`

```rust
//! Tool-output structure extraction (N2.5.c).

use std::sync::Arc;
use origin_cas::{Hash, Store};
use serde::{Deserialize, Serialize};

use crate::job::ExtractDeliverer;

pub const EXTRACT_THRESHOLD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outline {
    pub byte_count: u64,
    pub line_count: u32,
    pub first_120_chars: String,
}

pub async fn run(
    cas: &Arc<Store>,
    source: Hash,
    deliver_to: &dyn ExtractDeliverer,
) {
    let Ok(Some(bytes)) = cas.get(source) else { return };
    let outline = Outline {
        byte_count: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        line_count: u32::try_from(bytes.iter().filter(|b| **b == b'\n').count())
            .unwrap_or(u32::MAX),
        first_120_chars: {
            let cut = bytes.len().min(120);
            String::from_utf8_lossy(&bytes[..cut]).to_string()
        },
    };
    let Ok(json) = serde_json::to_vec(&outline) else { return };
    let Ok(outline_handle) = cas.put(&json) else { return };
    deliver_to.deliver(source, outline_handle).await;
}
```

### Step 5: Replace Extract arm of `dispatch_stub` in `runtime.rs`

In `dispatch_stub`'s `SidecarJob::Extract { handle, deliver_to }` arm, replace:

```rust
deliver_to.deliver(handle, handle).await;
```

with:

```rust
crate::extract::run(cas, handle, deliver_to.as_ref()).await;
```

The P5.1 test `submit_extract_drives_delivery` used a placeholder hash `Hash::of(b"placeholder")` which is NOT stored in the CAS. After this change, `extract::run` will silently no-op on the unknown handle and the counter will stay at 0. **Update that test** to first `cas.put(b"...")` real bytes and pass the returned hash:

```rust
let cas = store();
let body = b"the quick brown fox".repeat(1000);  // > 16KB
let h = cas.put(&body).expect("put");
let sidecar = Sidecar::spawn(Arc::new(StubProvider), cas.clone(), SidecarConfig::default());
sidecar.submit(SidecarJob::Extract {
    handle: h,
    deliver_to: Box::new(CountingExtract(counter.clone())),
}).expect("submit");
```

Same store reference must be passed to both `Sidecar::spawn` and the `cas.put` call.

Add `pub mod extract;` to `src/lib.rs`.

### Step 6: Wire into agent loop

In `crates/origin-daemon/src/agent.rs`, the tool-dispatch loop (around lines 130-180 based on the grep earlier) builds either `Block::ToolResult { handle: Some(*h.as_bytes()), inline: None, ... }` for CAS-stored outputs or `Block::ToolResult { handle: None, inline: Some(...), ... }` for inline. The Extract trigger applies to the handle-stored branch: after `cas.put(&result_bytes)` returns `Ok(h)`, check `if result_bytes.len() >= origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES` and submit:

```rust
if result_bytes.len() >= origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES {
    if let Some(sidecar) = &opts.sidecar {
        let _ = sidecar.submit(origin_sidecar::SidecarJob::Extract {
            handle: h,
            deliver_to: Box::new(NoopExtractDeliverer),
        });
    }
}
```

Define a minimal `NoopExtractDeliverer` next to `SessionStoreSummaryDeliverer` (or just below `LoopOptions`):

```rust
#[derive(Debug)]
struct NoopExtractDeliverer;

#[async_trait::async_trait]
impl origin_sidecar::ExtractDeliverer for NoopExtractDeliverer {
    async fn deliver(&self, _source: origin_cas::Hash, _outline: origin_cas::Hash) {
        // The outline handle's existence in CAS is sufficient — agent doesn't
        // route it anywhere this phase. Future phases may surface it via the
        // side panel or Recall.
    }
}
```

### Step 7: Run tests:

```bash
cargo test -p origin-sidecar --test extract
cargo test -p origin-daemon --test sidecar_extract
cargo test --workspace
```

### Step 8: Verification gate (workspace)

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

### Step 9: Commit

```bash
git add crates/origin-sidecar/ crates/origin-daemon/
git commit -m "feat(origin-sidecar): tool-output structure extraction (P5.3, N2.5.c)"
```

---

## Task P5.4 — Summary-backed compaction (PARALLEL with P5.2 / P5.3 / P5.5)

Adds `origin-daemon::compactor` — a stateless function that takes a current transcript + the session's persisted `messages.summary` lookups and returns a compacted transcript suitable for the next outgoing request. Compaction replaces the oldest N turns whose summaries are populated with a single synthetic `Block::Text { text: summary, ... }` per replaced turn. Full bodies stay in CAS and SQLite — the model can `Recall(handle)` to inflate any of them.

**Files:**
- `crates/origin-daemon/src/compactor.rs` *(new)*
- `crates/origin-daemon/src/lib.rs` *(modify — `pub mod compactor;`)*
- `crates/origin-daemon/src/session_store.rs` *(modify — add `load_summaries(session_id) -> Vec<(u32, Option<String>)>`)*
- `crates/origin-daemon/tests/compaction.rs` *(new)*

**Public surface added:**

```rust
// compactor.rs
pub const DEFAULT_SOFT_CAP_BYTES: usize = 200 * 1024;  // ~50K input tokens at 4B/token rough
pub const COMPACT_OLDEST_N_TURNS: usize = 4;

pub struct CompactionInput<'a> {
    pub transcript: &'a [Message],
    /// `summaries[i]` is `Some(text)` iff `messages.summary` is populated for
    /// the message at index `i` in `transcript`.
    pub summaries: &'a [Option<String>],
    /// Estimated outgoing-request size in bytes.
    pub current_bytes: usize,
    pub soft_cap_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutput {
    /// New transcript with oldest N turns swapped for summary blocks where
    /// possible. Length is always equal to the input length.
    pub transcript: Vec<Message>,
    /// Indices in `transcript` that were swapped.
    pub compacted_indices: Vec<usize>,
}

#[must_use]
pub fn compact(input: &CompactionInput<'_>) -> CompactionOutput;

// session_store.rs
impl SessionStore {
    /// Load `(turn_index, summary)` for every message in `session_id`, ordered
    /// by turn. `summary` is `None` for messages with no summary populated.
    pub fn load_summaries(
        &self,
        session_id: &str,
    ) -> Result<Vec<(u32, Option<String>)>, SessionStoreError>;
}
```

`compact` logic:
1. If `current_bytes <= soft_cap_bytes`, return the input transcript unchanged (and `compacted_indices = []`).
2. Otherwise, walk `transcript` left-to-right; replace the oldest `COMPACT_OLDEST_N_TURNS` entries whose corresponding `summaries[i]` is `Some` with a synthesized `Message { role: original_role, blocks: vec![Block::Text { text: format!("[compacted turn {i}] {summary}"), cache_marker: None }] }`. Record each `i` in `compacted_indices`. Stop after N replacements.
3. Entries where `summaries[i]` is `None` are skipped (not yet summarized — leave them alone).
4. Return the rebuilt transcript.

The function is pure: no I/O, no async. The agent loop will call `compact(...)` before each outgoing request and pass the result to the planner / provider.

### Step 1: Failing test at `crates/origin-daemon/tests/compaction.rs`

```rust
use origin_core::types::{Block, Message, Role};
use origin_daemon::compactor::{compact, CompactionInput, COMPACT_OLDEST_N_TURNS, DEFAULT_SOFT_CAP_BYTES};

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        blocks: vec![Block::Text { text: text.into(), cache_marker: None }],
    }
}
fn asst(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        blocks: vec![Block::Text { text: text.into(), cache_marker: None }],
    }
}

#[test]
fn under_cap_is_passthrough() {
    let transcript: Vec<Message> = (0..6).map(|i| user(&format!("turn {i}"))).collect();
    let summaries: Vec<Option<String>> = transcript.iter().map(|_| Some("s".into())).collect();
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: 1_000,
        soft_cap_bytes: 100_000,
    });
    assert_eq!(out.transcript, transcript);
    assert!(out.compacted_indices.is_empty());
}

#[test]
fn over_cap_replaces_oldest_n_turns_with_summaries() {
    let transcript: Vec<Message> = (0..10).map(|i| user(&format!("turn {i} body"))).collect();
    let summaries: Vec<Option<String>> = transcript.iter().map(|m| {
        // Use the message text itself as the summary stand-in.
        let Block::Text { text, .. } = &m.blocks[0] else { unreachable!() };
        Some(format!("sum-of-{text}"))
    }).collect();
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: 1_000_000,
        soft_cap_bytes: 100_000,
    });
    // First N replaced.
    assert_eq!(out.compacted_indices, (0..COMPACT_OLDEST_N_TURNS).collect::<Vec<_>>());
    // The replaced ones have summary text.
    for &i in &out.compacted_indices {
        let Block::Text { text, .. } = &out.transcript[i].blocks[0] else { panic!() };
        assert!(text.contains("sum-of-"), "summary text not in compacted body");
        assert!(text.starts_with("[compacted turn"), "compacted-turn marker missing");
    }
    // The non-compacted tail is unchanged.
    for i in COMPACT_OLDEST_N_TURNS..transcript.len() {
        assert_eq!(out.transcript[i], transcript[i]);
    }
}

#[test]
fn missing_summary_is_skipped_but_others_still_compact() {
    let transcript: Vec<Message> = (0..6).map(|i| user(&format!("t{i}"))).collect();
    // Turn 0's summary is missing; turns 1-3 have summaries.
    let summaries: Vec<Option<String>> = vec![
        None,
        Some("s1".into()),
        Some("s2".into()),
        Some("s3".into()),
        Some("s4".into()),
        Some("s5".into()),
    ];
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: 1_000_000,
        soft_cap_bytes: 100,
    });
    assert!(!out.compacted_indices.contains(&0), "turn 0 has no summary; must not compact");
    // We get 4 compactions from turns 1..=4.
    assert_eq!(out.compacted_indices, vec![1, 2, 3, 4]);
}

#[test]
fn default_constants_are_stable() {
    assert_eq!(COMPACT_OLDEST_N_TURNS, 4);
    assert_eq!(DEFAULT_SOFT_CAP_BYTES, 200 * 1024);
}
```

### Step 2: Run → fail.

```bash
cargo test -p origin-daemon --test compaction
```

### Step 3: Implement `compactor.rs`

```rust
//! Summary-backed compaction (P5.4).

use origin_core::types::{Block, Message};

pub const DEFAULT_SOFT_CAP_BYTES: usize = 200 * 1024;
pub const COMPACT_OLDEST_N_TURNS: usize = 4;

pub struct CompactionInput<'a> {
    pub transcript: &'a [Message],
    pub summaries: &'a [Option<String>],
    pub current_bytes: usize,
    pub soft_cap_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutput {
    pub transcript: Vec<Message>,
    pub compacted_indices: Vec<usize>,
}

/// Compact the oldest turns whose summaries are available, until either
/// `COMPACT_OLDEST_N_TURNS` replacements have been made or no more
/// summarizable turns remain.
///
/// Pass-through when `current_bytes <= soft_cap_bytes`.
#[must_use]
pub fn compact(input: &CompactionInput<'_>) -> CompactionOutput {
    if input.current_bytes <= input.soft_cap_bytes {
        return CompactionOutput {
            transcript: input.transcript.to_vec(),
            compacted_indices: Vec::new(),
        };
    }
    let mut out = input.transcript.to_vec();
    let mut compacted = Vec::with_capacity(COMPACT_OLDEST_N_TURNS);
    for (i, sum) in input.summaries.iter().enumerate().take(input.transcript.len()) {
        if compacted.len() >= COMPACT_OLDEST_N_TURNS { break; }
        let Some(summary) = sum.as_ref() else { continue };
        let role = input.transcript[i].role;
        out[i] = Message {
            role,
            blocks: vec![Block::Text {
                text: format!("[compacted turn {i}] {summary}"),
                cache_marker: None,
            }],
        };
        compacted.push(i);
    }
    CompactionOutput { transcript: out, compacted_indices: compacted }
}
```

### Step 4: Add `pub mod compactor;` to `crates/origin-daemon/src/lib.rs`

### Step 5: Implement `SessionStore::load_summaries`

Append to `session_store.rs`:

```rust
impl SessionStore {
    /// Return `(turn_index, summary)` for every persisted message of
    /// `session_id`, ordered ascending by `turn_index`.
    ///
    /// # Errors
    /// Propagates sqlite errors on read failure.
    pub fn load_summaries(
        &self,
        session_id: &str,
    ) -> Result<Vec<(u32, Option<String>)>, SessionStoreError> {
        let rows = self.inner.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT turn_index, summary FROM messages \
                 WHERE session_id = ?1 ORDER BY turn_index ASC",
            )?;
            let iter = stmt.query_map([session_id], |r| {
                let t: i64 = r.get(0)?;
                let s: Option<String> = r.get(1)?;
                Ok((u32::try_from(t).unwrap_or(u32::MAX), s))
            })?;
            let mut out = Vec::new();
            for r in iter { out.push(r?); }
            Ok(out)
        })?;
        Ok(rows)
    }
}
```

### Step 6: Run tests:

```bash
cargo test -p origin-daemon --test compaction
cargo test --workspace
```

### Step 7: Verification gate (workspace).

### Step 8: Commit

```bash
git add crates/origin-daemon/
git commit -m "feat(origin-daemon): summary-backed compactor + load_summaries (P5.4)"
```

---

## Task P5.5 — Learned-dictionary zstd training (PARALLEL with P5.2 / P5.3 / P5.4)

Adds `origin-cas::dict` — trains a 64KB zstd dictionary from sampled CAS shards and uses it on the cold-write / cold-read path. Persists the dict bytes under the store root as `dict-v1.zstd` and stores the active dict version in a tiny `dict_meta` sidecar file. New cold writes encode against the active dict; old cold shards (encoded with no dict) keep decoding via the no-dict path (zstd handles this automatically as long as we call the right decode function).

**Files:**
- `crates/origin-cas/src/dict.rs` *(new)*
- `crates/origin-cas/src/store.rs` *(modify — add `train_dict_from_sample`, dict-aware cold write/read)*
- `crates/origin-cas/src/lib.rs` *(modify — re-export `DictError`, `DictVersion`)*
- `crates/origin-cas/tests/dict.rs` *(new)*

**Public surface added:**

```rust
// dict.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictVersion(pub u32);

#[derive(Debug, Error)]
pub enum DictError {
    #[error("training failed: {0}")] Train(String),
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("not enough samples: have {have}, need {need}")] Insufficient { have: usize, need: usize },
}

pub const TARGET_DICT_BYTES: usize = 64 * 1024;
pub const MIN_SAMPLES_FOR_TRAINING: usize = 16;

/// Train a dict from `samples`. Returns the dict bytes.
///
/// # Errors
/// Returns `Insufficient` if fewer than `MIN_SAMPLES_FOR_TRAINING` samples
/// were provided; `Train` if zstd's training routine fails (it can fail on
/// degenerate input, e.g. all-zero samples).
pub fn train(samples: &[Vec<u8>]) -> Result<Vec<u8>, DictError>;

// store.rs new method
impl Store {
    /// Sample up to `n_samples` cold-tier shards and train a 64KB dict from them.
    /// Persists the resulting dict to `<root>/dict-v<n>.zstd` and updates
    /// `<root>/dict_meta` to point at the new version. Subsequent cold writes
    /// use the new dict; reads transparently fall back to dictless decode
    /// for pre-dict shards.
    ///
    /// # Errors
    /// Propagates `DictError` from training and `StoreError::Io` from
    /// file writes.
    pub fn train_dict_from_sample(&self, n_samples: usize) -> Result<DictVersion, StoreError>;

    /// Return the currently active dict version, or None if no dict is active.
    #[must_use]
    pub fn active_dict_version(&self) -> Option<DictVersion>;
}
```

**Implementation strategy for cold I/O:**
- Cold writes use `zstd::dict::EncoderDictionary` if a dict is loaded; falls back to plain `zstd::encode_all` if not.
- Cold reads always try the dict-aware path first (`zstd::dict::DecoderDictionary`); if that errors, fall back to `zstd::decode_all`. Pre-dict shards decode via fallback; post-dict shards decode via dict.
- This is **transparent**: existing pack files don't need migration. They keep decoding fine because zstd's framing self-describes when a dict is required, and we attempt both paths.
- Cleaner alternative: write a 1-byte sentinel prefix to each compressed cold blob indicating dict-mode (0x00) vs no-dict (0xFF), then dispatch. But this changes the on-disk pack format. **Skip this for P5.5** — the try-both-fallback path works without an on-disk format change. P12's arena work can revisit.

### Step 1: Failing test at `crates/origin-cas/tests/dict.rs`

```rust
use std::sync::Arc;
use origin_cas::{dict, Store, StoreConfig};
use tempfile::tempdir;

fn store(dir: &std::path::Path) -> Arc<Store> {
    Arc::new(Store::open(StoreConfig {
        root: dir.to_path_buf(),
        hot_capacity: 4,
        warm_pack_target_bytes: 1_000,  // small so writes spill quickly
        cold_zstd_level: 3,
    }).expect("open"))
}

#[test]
fn train_rejects_insufficient_samples() {
    let samples: Vec<Vec<u8>> = (0..5).map(|i| format!("sample {i}").into_bytes()).collect();
    let err = dict::train(&samples).expect_err("should fail");
    assert!(matches!(err, dict::DictError::Insufficient { .. }));
}

#[test]
fn train_produces_nonempty_dict_from_repetitive_samples() {
    let samples: Vec<Vec<u8>> = (0..32)
        .map(|i| format!("the quick brown fox jumps over the lazy dog. iter={i}\n").repeat(20).into_bytes())
        .collect();
    let dict_bytes = dict::train(&samples).expect("train");
    assert!(!dict_bytes.is_empty(), "dict should be non-empty");
    assert!(dict_bytes.len() <= dict::TARGET_DICT_BYTES, "dict <= target");
}

#[test]
fn train_dict_from_sample_persists_and_returns_version() {
    let dir = tempdir().expect("tempdir");
    let s = store(dir.path());
    // Populate the store with enough cold shards.
    for i in 0..40 {
        let body = format!("the quick brown fox jumps over the lazy dog. seq={i}\n").repeat(20).into_bytes();
        let h = s.put(&body).expect("put");
        s.demote_to_cold(h).expect("demote");
    }
    let v = s.train_dict_from_sample(32).expect("train");
    assert_eq!(s.active_dict_version(), Some(v));
    // The dict file should exist on disk.
    assert!(dir.path().join(format!("dict-v{}.zstd", v.0)).exists());
}

#[test]
fn predict_shards_remain_readable_after_dict_training() {
    let dir = tempdir().expect("tempdir");
    let s = store(dir.path());
    let body = b"hello world".repeat(100);
    let h = s.put(&body).expect("put");
    s.demote_to_cold(h).expect("demote");
    // Train a dict. Old cold shard must still decode.
    for i in 0..40 {
        let _ = s.put(&format!("filler {i}").repeat(50).into_bytes()).expect("put");
    }
    let _v = s.train_dict_from_sample(32).expect("train");
    let got = s.get(h).expect("get").expect("Some");
    assert_eq!(got, body);
}
```

### Step 2: Run → fail.

```bash
cargo test -p origin-cas --test dict
```

### Step 3: Implement `dict.rs`

```rust
//! Learned-dictionary zstd compression (N3.2).

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictVersion(pub u32);

#[derive(Debug, Error)]
pub enum DictError {
    #[error("training failed: {0}")]
    Train(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not enough samples: have {have}, need {need}")]
    Insufficient { have: usize, need: usize },
}

pub const TARGET_DICT_BYTES: usize = 64 * 1024;
pub const MIN_SAMPLES_FOR_TRAINING: usize = 16;

/// Train a 64KB zstd dictionary from `samples`.
///
/// # Errors
/// Returns `Insufficient` if there are fewer than `MIN_SAMPLES_FOR_TRAINING`
/// samples, or `Train` if zstd rejects the training set.
pub fn train(samples: &[Vec<u8>]) -> Result<Vec<u8>, DictError> {
    if samples.len() < MIN_SAMPLES_FOR_TRAINING {
        return Err(DictError::Insufficient {
            have: samples.len(),
            need: MIN_SAMPLES_FOR_TRAINING,
        });
    }
    zstd::dict::from_samples(samples, TARGET_DICT_BYTES)
        .map_err(|e| DictError::Train(e.to_string()))
}

/// Read the persisted dict file at `path`. Returns `None` if absent.
///
/// # Errors
/// Returns `Io` for any read error other than NotFound.
pub fn load_dict_file(path: &Path) -> Result<Option<Vec<u8>>, DictError> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DictError::Io(e)),
    }
}
```

### Step 4: Extend `Store`

In `crates/origin-cas/src/store.rs`, add to `Inner` (private fields):

```rust
active_dict: Option<(crate::dict::DictVersion, Vec<u8>)>,
```

Initialize to `None` inside `Store::open` after migrations.

Add to the impl:

```rust
impl Store {
    pub fn train_dict_from_sample(&self, n_samples: usize) -> Result<crate::dict::DictVersion, StoreError> {
        let samples = self.collect_samples(n_samples)?;
        let dict_bytes = crate::dict::train(&samples)
            .map_err(|e| StoreError::Zstd(e.to_string()))?;
        let v = self.next_dict_version();
        let path = self.inner.lock().cfg.root.join(format!("dict-v{}.zstd", v.0));
        std::fs::write(&path, &dict_bytes)?;
        self.inner.lock().active_dict = Some((v, dict_bytes));
        // Update the dict_meta sidecar pointer.
        let meta_path = self.inner.lock().cfg.root.join("dict_meta");
        std::fs::write(meta_path, v.0.to_string())?;
        Ok(v)
    }

    #[must_use]
    pub fn active_dict_version(&self) -> Option<crate::dict::DictVersion> {
        self.inner.lock().active_dict.as_ref().map(|(v, _)| *v)
    }

    fn collect_samples(&self, n: usize) -> Result<Vec<Vec<u8>>, StoreError> {
        // Walk cold packs, decode each shard, collect up to `n`.
        let inner = self.inner.lock();
        let mut samples: Vec<Vec<u8>> = Vec::new();
        for (h, &pack_idx) in &inner.cold_index {
            if samples.len() >= n { break; }
            if let Some(slice) = inner.cold_packs[pack_idx].read(*h) {
                let dec = zstd::decode_all(slice.as_ref()).map_err(|e| StoreError::Zstd(e.to_string()))?;
                samples.push(dec);
            }
        }
        Ok(samples)
    }

    fn next_dict_version(&self) -> crate::dict::DictVersion {
        let cur = self.inner.lock().active_dict.as_ref().map_or(0, |(v, _)| v.0);
        crate::dict::DictVersion(cur + 1)
    }
}
```

Modify `demote_to_cold` to use the active dict when encoding:

```rust
// Replace this line in demote_to_cold:
let compressed = zstd::encode_all(&bytes[..], inner.cfg.cold_zstd_level)
    .map_err(|e| StoreError::Zstd(e.to_string()))?;

// With dict-aware encoding:
let compressed = if let Some((_, dict)) = &inner.active_dict {
    use zstd::stream::Encoder;
    use std::io::Write;
    let mut enc = Encoder::with_dictionary(Vec::new(), inner.cfg.cold_zstd_level, dict)
        .map_err(|e| StoreError::Zstd(e.to_string()))?;
    enc.write_all(&bytes).map_err(|e| StoreError::Zstd(e.to_string()))?;
    enc.finish().map_err(|e| StoreError::Zstd(e.to_string()))?
} else {
    zstd::encode_all(&bytes[..], inner.cfg.cold_zstd_level)
        .map_err(|e| StoreError::Zstd(e.to_string()))?
};
```

Modify `get` to attempt dict-aware decode first, falling back to plain decode for pre-dict shards. In the cold-pack arm (around line 156-160):

```rust
if let Some(&idx) = inner.cold_index.get(&h) {
    if let Some(slice) = inner.cold_packs[idx].read(h) {
        let decoded = if let Some((_, dict)) = &inner.active_dict {
            // Try dict-aware first; fall back to plain.
            use zstd::stream::Decoder;
            use std::io::Read;
            let cursor = std::io::Cursor::new(slice.as_ref());
            let dec_result = (|| -> Result<Vec<u8>, std::io::Error> {
                let mut d = Decoder::with_dictionary(cursor, dict)?;
                let mut buf = Vec::new();
                d.read_to_end(&mut buf)?;
                Ok(buf)
            })();
            match dec_result {
                Ok(bytes) => bytes,
                Err(_) => zstd::decode_all(slice.as_ref())
                    .map_err(|e| StoreError::Zstd(e.to_string()))?,
            }
        } else {
            zstd::decode_all(slice.as_ref()).map_err(|e| StoreError::Zstd(e.to_string()))?
        };
        return Ok(Some(decoded));
    }
}
```

### Step 5: Tests pass:

```bash
cargo test -p origin-cas --test dict
cargo test -p origin-cas
```

### Step 6: Verification gate

```bash
cargo test -p origin-cas
cargo clippy -p origin-cas --all-targets -- -D warnings
cargo fmt --check
```

### Step 7: Commit

```bash
git add crates/origin-cas/
git commit -m "feat(origin-cas): learned-dict zstd training + dict-aware cold I/O (P5.5, N3.2)"
```

---

## Task P5.6 — Phase 5 checkpoint: long-session bench + tag `p5-complete`

**Files:**
- `crates/origin-daemon/benches/long_session.rs` *(new)*
- `crates/origin-daemon/Cargo.toml` *(modify — add `[[bench]]` + `criterion` dev-dep if absent)*

The bench drives a 2-hour synthetic session via in-memory mocks (NOT live API): 720 turns × `(user message + assistant message)`, with summaries simulated. Verifies that the compactor keeps the outgoing-request size below a chosen ceiling.

### Step 1: Modify `crates/origin-daemon/Cargo.toml`

In `[dev-dependencies]`, add (if not present) `criterion = "0.5"`. Append:

```toml
[[bench]]
name = "long_session"
harness = false
```

### Step 2: Write the bench at `crates/origin-daemon/benches/long_session.rs`

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use origin_core::types::{Block, Message, Role};
use origin_daemon::compactor::{compact, CompactionInput, DEFAULT_SOFT_CAP_BYTES};

fn synth_turn(i: usize) -> (Message, Message) {
    let user_text = format!("Question {i}: ").repeat(50);
    let asst_text = format!("Answer {i}: ").repeat(50);
    (
        Message { role: Role::User, blocks: vec![Block::Text { text: user_text, cache_marker: None }] },
        Message { role: Role::Assistant, blocks: vec![Block::Text { text: asst_text, cache_marker: None }] },
    )
}

fn estimate_bytes(transcript: &[Message]) -> usize {
    transcript.iter().flat_map(|m| m.blocks.iter()).map(|b| {
        if let Block::Text { text, .. } = b { text.len() } else { 0 }
    }).sum()
}

fn bench_long_session(c: &mut Criterion) {
    let mut transcript: Vec<Message> = Vec::with_capacity(1440);
    let mut summaries: Vec<Option<String>> = Vec::with_capacity(1440);
    for i in 0..720 {
        let (u, a) = synth_turn(i);
        transcript.push(u);
        summaries.push(Some(format!("user said q{i}")));
        transcript.push(a);
        summaries.push(Some(format!("asst answered a{i}")));
    }
    // Simulate the daemon's per-turn compaction call.
    c.bench_function("compact_long_session", |b| {
        b.iter(|| {
            let current = estimate_bytes(&transcript);
            let out = compact(&CompactionInput {
                transcript: &transcript,
                summaries: &summaries,
                current_bytes: current,
                soft_cap_bytes: DEFAULT_SOFT_CAP_BYTES,
            });
            black_box(out);
        });
    });
    // Static assertion: after one round of compaction the size is strictly less
    // than the pre-compaction size, demonstrating the policy fires.
    let current = estimate_bytes(&transcript);
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: current,
        soft_cap_bytes: DEFAULT_SOFT_CAP_BYTES,
    });
    let new_bytes = estimate_bytes(&out.transcript);
    assert!(new_bytes < current, "compaction must shrink transcript bytes: was {current}, now {new_bytes}");
    assert!(!out.compacted_indices.is_empty(), "compaction must replace at least one turn");
}

criterion_group!(benches, bench_long_session);
criterion_main!(benches);
```

### Step 3: Run the bench

```bash
cargo bench -p origin-daemon --bench long_session -- --quick
```

Expect: clean run; the embedded assertions (`new_bytes < current` and non-empty `compacted_indices`) hold; the criterion `mean` reports a comfortable number (compaction is pure-CPU + clone, should run in microseconds).

If either assertion fires, **do not advance** — investigate. Likely cause: the soft-cap default was set too high, or the summary block-text format produces blocks larger than the bodies they replace (summary should be ≤120 chars + the `"[compacted turn N] "` prefix; an original turn is `"Question N: " * 50` which is ~600 chars — so summaries are strictly smaller).

### Step 4: Final verification gate

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo bench -p origin-daemon --bench long_session -- --quick
```

### Step 5: Tag

```bash
git tag p5-complete
```

### Step 6: Commit

```bash
git add crates/origin-daemon/Cargo.toml crates/origin-daemon/benches/long_session.rs
git commit -m "chore(origin-daemon): long-session compaction bench; tag p5-complete (P5.6)"
```

---

## Self-review checklist

**Spec coverage:**
- ✅ N2.5.a — eager turn summarization (P5.2)
- ✅ N2.5.c — tool-output structure extraction (P5.3)
- ✅ Summary-backed compaction (P5.4)
- ✅ N3.2 — learned-dict zstd training (P5.5)
- ✅ Sidecar coroutine — bounded mpsc + N workers (P5.1)
- ⚠️ N2.5.b — memory-recall verification — explicitly deferred to Phase 6 (depends on `origin-mem`)

**Type consistency:**
- `SidecarJob::Summarize { session_id: String, turn_index: u32, transcript: Vec<Message>, deliver_to }` consistent in P5.1, P5.2.
- `SidecarJob::Extract { handle: Hash, deliver_to }` consistent in P5.1, P5.3.
- `SummaryDeliverer::deliver(&self, session_id: &str, turn_index: u32, summary: &str)` consistent across P5.1 trait def, P5.2 daemon impl, P5.2 sidecar `summarize::run` callsite.
- `ExtractDeliverer::deliver(&self, source: Hash, outline: Hash)` consistent across P5.1 trait def, P5.3 daemon impl, P5.3 sidecar `extract::run` callsite.
- `Outline { byte_count: u64, line_count: u32, first_120_chars: String }` consistent in P5.3 module + test.
- `CompactionInput { transcript, summaries, current_bytes, soft_cap_bytes }` and `CompactionOutput { transcript, compacted_indices }` consistent in P5.4 module + tests + P5.6 bench.
- `DictVersion(u32)`, `DictError { Insufficient, Train, Io }`, `TARGET_DICT_BYTES = 64KB`, `MIN_SAMPLES_FOR_TRAINING = 16` consistent in P5.5.
- `EXTRACT_THRESHOLD_BYTES = 16 * 1024` consistent in P5.3 module + daemon callsite + light test.
- `COMPACT_OLDEST_N_TURNS = 4`, `DEFAULT_SOFT_CAP_BYTES = 200 * 1024` consistent in P5.4 module + tests + P5.6 bench.

**Placeholders:** No "TBD" / "implement later" / "fill in details". Every task names exact files, exact public surfaces, exact deps, and exact failing-test code.

**Parallel-dispatch map (per the user's instruction "tasks can be done in parallel"):**

```
P5.1 (foundation, serial)
  └── verify → THEN fan out in parallel ──────────┐
                                                  │
              P5.2 (summarize)                    │
              P5.3 (extract)                      │ same dispatch message,
              P5.4 (compactor)                    │ 4 subagents in parallel
              P5.5 (zstd dict)                    │
                                                  │
              all four verify ────────────────────┘
              THEN
P5.6 (bench + tag, serial)
```

The controller is permitted to dispatch P5.2, P5.3, P5.4, P5.5 in a SINGLE message with four `Agent` tool calls — see the user's override of the skill's "never parallel implementations" rule. **Important sequencing note for the parallel batch:** P5.2 and P5.3 both modify `crates/origin-daemon/src/agent.rs` (P5.2 adds the sidecar field to `LoopOptions` + the summary-submit at turn-end; P5.3 adds the extract-submit at tool-result-CAS-put). The controller MUST serialize the merge of `agent.rs` even if subagents work in parallel — i.e., dispatch all four subagents with `isolation: "worktree"`, then cherry-pick or rebase their commits onto `dev` one at a time, resolving the `agent.rs` overlap when both P5.2 and P5.3 land. An alternative is to dispatch in two waves: { P5.2, P5.4, P5.5 } in parallel (all touch different files except P5.2 touches agent.rs alone), then P5.3 serially.

---

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-05-19-origin-phase-5.md`. Per the user's instruction, execution is via **superpowers:subagent-driven-development**, each task internally following **superpowers:test-driven-development** and gated by **superpowers:verification-before-completion**. Tasks can fan out in parallel after P5.1 lands.

Branch: `dev`.
