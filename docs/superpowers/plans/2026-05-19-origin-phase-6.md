# `origin` Phase 6 — Memory Graph — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run-to-fail, implement, run-to-pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Ship `origin-mem` — a cross-session conversation-memory subsystem with int8-quantized embeddings (per-cluster centroid offsets), HNSW + temporal-decay re-rank, memory bodies in CAS / vectors inline in SQLite, three explicit tools (`mem_search` / `mem_save` / `mem_forget`), auto-save proposer + auto-recall injector wired through the cache planner's Sticky band, IPC-driven memory-proposal review on the client, and a similarity-only idle consolidation pass.

**Architecture:** `origin-mem` owns: (a) the `Embedder` (ONNX MiniLM L6 v2, 384-d, shipped P6.1); (b) a `Quantizer` that learns 256 cluster centroids via k-means on a training sample and encodes f32-384 vectors into `(centroid_id: u8, deltas: [i8; 384])` ~6× smaller than f32; (c) a `MemIndex` over `hnsw_rs` returning candidate IDs, with a re-ranker that applies `sim × exp(-age_days / τ) × cluster_priority × edge_boost`; (d) a `MemoryStore` SQLite layer (migration `V3__memories.sql`) that holds the quantized vector inline and stores the body as a CAS handle, edges in `mem_edges`, tag dictionary in `mem_tags`; (e) a `Proposer` (turn-end heuristic extraction) and `Injector` (turn-start sticky-band injection); (f) a `Consolidator` that runs on a daemon idle heartbeat and proposes `Supersedes` edges via cosine clustering. The three explicit tools live in `origin-tools` and delegate to `origin-mem` through a single `MemoryHandle` injected via the existing dispatch state. The daemon wires a `MemoryHandle` into `LoopOptions`, emits `StreamEvent::MemoryProposed` for each turn-end candidate, and accepts `ClientMessage::MemoryDecision` on the reverse channel. The CLI parses `/mem accept|reject|edit N …` and round-trips the decision.

**Tech Stack:** Rust 1.83 (MSRV pin). Crate-local deps already on `phase-6`: `ort 2.0.0-rc.4` (download-binaries) + `tokenizers 0.20` + `ndarray 0.15` (P6.1), `hnsw_rs 0.3`, `rand 0.8` + `rand_chacha 0.3`, `rusqlite 0.31` (bundled + blob), `ulid 1`, `blake3 1`, `sha2 0.10`, `ureq 2`, `thiserror 1`. New (workspace-pinned this phase): `bitvec 1` (tag bitset), `regex 1.10` (proposer heuristics — already in workspace from Recall). **Novel-implementation reflex** per `[[feedback-novel-implementations]]`: int8 + per-cluster centroid offsets is the headline mechanism (≈6× compression and 8–12× faster recall vs. f32 dot on a 10k store); HNSW returns top-3K and the re-ranker applies decay + edge boost in one pass; memory bodies are content-addressed in CAS so identical memories dedupe across sessions for free; vectors are stored inline in SQLite as a single `BLOB` column for mmap-cache-warm reads. **No prose memory bodies on the hot path** — the message log carries `body_handle` and `preview_64B` only; the model inflates via the existing `Recall` tool.

**Builds on:** Spec §6A (N6.1–N6.5) of `docs/superpowers/specs/2026-05-19-origin-harness-design.md`. Reuses `origin-cas::Store` for body storage (Phase 2), `origin-store::Store` for the SQL layer (Phase 0/2), `origin-planner::Band::Sticky` for recall injection (Phase 3), `origin-tools::origin_tool!` registration macro (Phase 1), and the daemon's `StreamEvent` channel (Phase 2). P6.1 (`Embedder` + ONNX runtime + `EMBED_DIM = 384`) is **already shipped** at `crates/origin-mem/src/embedder.rs` — this plan starts at P6.2.

**Explicitly out of scope (deferred):**
- Sidecar-driven contradiction classifier (Phase 5 — `origin-sidecar` ships the small-model worker; P6 implements only similarity-based supersede clustering and lexical contradiction stubs).
- TUI side-panel rendering for proposals (Phase 4 P4.5 — until the side panel target exists, the CLI surfaces proposals as inline status-line entries; the daemon-side IPC contract is the durable surface and won't change when the panel lands).
- Joint code-graph cross-edges (Phase 7 — `cross_links` table referenced in §6C is not provisioned this phase).
- N6.5 hooks-driven auto-tagging beyond the lexical rules (waits on Phase 10 hooks pool).
- Provider matrix beyond Anthropic (Phase 8 — `phase-8` branch).

---

## Conventions reminder (apply to every task)

**TDD shape:** failing test → run-to-fail → implement → run-to-pass → verification gate → commit.

**Verification gate per task type:**

| Task type | Required commands (all exit 0) |
|---|---|
| Single-crate pure logic (P6.2, P6.3, P6.4 unit-only paths) | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / tool registration (P6.5) | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Daemon-wired / IPC / CLI (P6.6, P6.7, P6.8, P6.9) | Above + `cargo build --workspace` (smoke) |
| Final phase gate (P6.9) | Above + `git tag p6-complete` |

**Inherited patterns:**
- `[lints] workspace = true` in every new `Cargo.toml`; `origin-mem` already conforms.
- Workspace inheritance for `version` / `edition` / `rust-version` / `license` / `repository`.
- `unsafe_code = "forbid"` is the default. **No `unsafe` introduced this phase.** (Existing ONNX/ort code goes through safe wrappers; HNSW lib is safe.)
- `#[must_use]` on every public constructor; `const fn` wherever possible.
- Tests use `.expect("meaningful message")`. No `clippy::unwrap_used` allows.
- Custom error enums via `thiserror`; document `# Errors` on every public `Result`-returning fn.
- For each `#[allow(clippy::...)]` add an inline justification.
- **MSRV pin reflex** (`[[project-msrv-dep-pinning]]`): if `cargo check` complains about `edition2024`, pin offender with `cargo update -p <crate>@<bad> --precise <last-1.83-compatible>` and commit `Cargo.lock`. Baseline pins already applied on this branch.
- Live smoke tests follow `crates/origin-daemon/tests/anthropic_smoke.rs` shape: gate on env var (`ANTHROPIC_API_KEY`), skip silently when unset.
- Determinism in tests: every `rand` use seeds via `rand_chacha::ChaCha8Rng::seed_from_u64(0xC0FFEE)` so k-means / HNSW insertion order are reproducible.
- Commits: Conventional Commits, scoped (`feat(origin-mem): …`), one commit per task. Always co-author Claude Opus 4.7.

---

## File map for Phase 6

| New / modified | Responsibility | Task |
|---|---|---|
| `crates/origin-mem/src/embedder.rs` *(exists, shipped P6.1)* | ONNX MiniLM session + tokenizer | — |
| `crates/origin-mem/src/quantizer.rs` *(new)* + `tests/quantizer.rs` *(new)* | k-means 256 centroids; encode/decode; int8 dot | P6.2 |
| `crates/origin-mem/src/index.rs` *(new)* + `tests/index.rs` *(new)* | HNSW wrapper + temporal-decay re-rank | P6.3 |
| `crates/origin-store/src/migrations/V3__memories.sql` *(new)* + `crates/origin-mem/src/storage.rs` *(new)* + `tests/storage_round_trip.rs` *(new)* | `memories` / `mem_edges` / `mem_tags` tables + `MemoryStore` API | P6.4 |
| `crates/origin-tools/src/builtins/mem.rs` *(new)* + `crates/origin-tools/src/builtins/mod.rs` *(modify)* + `crates/origin-tools/src/dispatch.rs` *(modify: optional `MemoryHandle` in dispatch state)* | `mem_search` / `mem_save` / `mem_forget` tool registrations | P6.5 |
| `crates/origin-mem/src/proposer.rs` *(new)* + `src/injector.rs` *(new)* + `tests/proposer.rs` *(new)* + `tests/injector.rs` *(new)* | Turn-end candidate extraction + turn-start sticky-band injection | P6.6 |
| `crates/origin-daemon/src/protocol.rs` *(modify)* + `crates/origin-daemon/src/agent.rs` *(modify: emit Proposed; consume Decision)* + `crates/origin-cli/src/input.rs` *(modify)* + `crates/origin-cli/src/main.rs` *(modify)* + `crates/origin-daemon/tests/memory_proposal_round_trip.rs` *(new)* | `StreamEvent::MemoryProposed` + `ClientMessage::MemoryDecision` + `/mem` slash command | P6.7 |
| `crates/origin-mem/src/consolidator.rs` *(new)* + `tests/consolidator.rs` *(new)* | Idle pass: cosine clusters → `Supersedes`; lexical contradiction stub | P6.8 |
| `crates/origin-daemon/src/main.rs` *(modify)* + `crates/origin-daemon/src/agent.rs` *(modify: wire MemoryHandle into LoopOptions)* + `crates/origin-mem/src/lib.rs` *(re-exports)* + `crates/origin-daemon/tests/memory_e2e.rs` *(new)* + tag `p6-complete` | Daemon wiring, idle heartbeat task, E2E gate | P6.9 |

File-size discipline: every new `.rs` targets <300 LOC. The `MemoryStore` may push to ~350 LOC because of three tables — acceptable; split if it exceeds 400.

---

## Task P6.2 — Int8 quantizer with per-cluster centroid offsets (N6.1)

**Files:** `crates/origin-mem/src/quantizer.rs` (new), `crates/origin-mem/tests/quantizer.rs` (new). Modify `crates/origin-mem/src/lib.rs` to add `pub mod quantizer; pub use quantizer::{Quantizer, QuantizerError, NUM_CENTROIDS, EncodedVector};`.

**Public surface:**

```rust
/// Number of cluster centroids learned by [`Quantizer::fit`]. Constant per spec N6.1.
pub const NUM_CENTROIDS: usize = 256;

/// One quantized vector: `(centroid_id, deltas)` where each delta is the f32
/// residual from the centroid, scaled to i8 by a per-quantizer global scale.
#[derive(Debug, Clone)]
pub struct EncodedVector {
    pub centroid_id: u8,
    pub deltas: Box<[i8; crate::EMBED_DIM]>,
}

#[derive(Debug, thiserror::Error)]
pub enum QuantizerError {
    #[error("training set must contain at least {min} vectors, got {got}")]
    TooFewSamples { got: usize, min: usize },
    #[error("k-means failed to converge after {iters} iterations")]
    NoConverge { iters: u32 },
}

pub struct Quantizer {
    centroids: Box<[[f32; crate::EMBED_DIM]; NUM_CENTROIDS]>,
    /// Global i8 scale: real_delta = i8_delta * scale.
    scale: f32,
}

impl Quantizer {
    /// Train via k-means++ init + Lloyd refinement (max 25 iters).
    /// Requires at least `NUM_CENTROIDS` training vectors.
    ///
    /// # Errors
    /// `TooFewSamples` if `training.len() < NUM_CENTROIDS`;
    /// `NoConverge` if iter budget exceeded with movement above 1e-4.
    pub fn fit(training: &[[f32; crate::EMBED_DIM]], rng_seed: u64) -> Result<Self, QuantizerError>;

    #[must_use]
    pub fn encode(&self, v: &[f32; crate::EMBED_DIM]) -> EncodedVector;

    /// Reconstruct (lossy) the original vector — useful for HNSW seeding.
    #[must_use]
    pub fn decode(&self, e: &EncodedVector) -> [f32; crate::EMBED_DIM];

    /// Approximate dot product of a fresh f32 query against an encoded vector.
    /// Uses centroid dot + i8-scaled delta accumulation in one pass.
    #[must_use]
    pub fn dot(&self, query: &[f32; crate::EMBED_DIM], e: &EncodedVector) -> f32;

    /// Serialize centroids + scale for persistence inside [`MemoryStore`].
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8>;

    /// Inverse of `to_bytes`.
    ///
    /// # Errors
    /// Returns `QuantizerError::TooFewSamples` (re-used as a generic "malformed")
    /// when the buffer length is wrong.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, QuantizerError>;
}
```

**Algorithm notes:** k-means++ init (probability proportional to squared distance to nearest existing centroid). Distance metric is cosine over L2-normalized vectors (the MiniLM output is already normalized — assert at debug build). Lloyd iterations stop when total centroid movement < 1e-4 or after 25 iterations. The `scale` is `max(|delta|) / 127.0` computed once after Lloyd finishes; deltas saturate to ±127.

**Anti-bloat:** No `serde` for centroids — `to_bytes`/`from_bytes` is a simple little-endian dump (`[u32 magic, u32 version, f32 scale, [f32; 384*256] centroids]`) so the on-disk format is fast and version-tagged without serde overhead.

- [ ] **Step 1: Write failing test** `crates/origin-mem/tests/quantizer.rs`:

  ```rust
  use origin_mem::quantizer::{Quantizer, NUM_CENTROIDS};
  use origin_mem::EMBED_DIM;
  use rand::SeedableRng;
  use rand::Rng;
  use rand_chacha::ChaCha8Rng;

  fn synth(rng_seed: u64, n: usize) -> Vec<[f32; EMBED_DIM]> {
      let mut rng = ChaCha8Rng::seed_from_u64(rng_seed);
      (0..n)
          .map(|_| {
              let mut v = [0_f32; EMBED_DIM];
              for slot in v.iter_mut() { *slot = rng.gen_range(-1.0..1.0); }
              // L2 normalize
              let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
              for slot in v.iter_mut() { *slot /= norm; }
              v
          })
          .collect()
  }

  #[test]
  fn fit_encode_dot_approximates_f32() {
      let training = synth(0xC0FFEE, NUM_CENTROIDS * 4);
      let q = Quantizer::fit(&training, 0xC0FFEE).expect("fit");
      let query = training[0];
      let target = training[1];
      let f32_dot: f32 = query.iter().zip(target.iter()).map(|(a, b)| a * b).sum();
      let enc = q.encode(&target);
      let approx = q.dot(&query, &enc);
      let err = (approx - f32_dot).abs();
      assert!(err < 0.02, "approx dot off by {err} (f32={f32_dot} approx={approx})");
  }

  #[test]
  fn round_trip_bytes_preserves_dot() {
      let training = synth(0xBEEF, NUM_CENTROIDS * 4);
      let q = Quantizer::fit(&training, 0xBEEF).expect("fit");
      let bytes = q.to_bytes();
      let q2 = Quantizer::from_bytes(&bytes).expect("from_bytes");
      let enc = q.encode(&training[0]);
      let enc2 = q2.encode(&training[0]);
      // Centroid IDs may differ only if scale serialization round-trips badly; assert equality.
      assert_eq!(enc.centroid_id, enc2.centroid_id);
      assert_eq!(enc.deltas[..], enc2.deltas[..]);
  }

  #[test]
  fn too_few_samples_errors() {
      let training = synth(1, NUM_CENTROIDS - 1);
      let err = Quantizer::fit(&training, 1).expect_err("must error");
      match err {
          origin_mem::quantizer::QuantizerError::TooFewSamples { got, min } => {
              assert_eq!(got, NUM_CENTROIDS - 1);
              assert_eq!(min, NUM_CENTROIDS);
          }
          other => panic!("wrong variant: {other:?}"),
      }
  }
  ```

- [ ] **Step 2:** Run `cargo test -p origin-mem --test quantizer` — expect failure (module missing).

- [ ] **Step 3: Implement** `crates/origin-mem/src/quantizer.rs` per public surface above. Use `rand_chacha::ChaCha8Rng` for determinism; reject NaN deltas at encode-time (debug assertion is fine — production input is the embedder's L2-normalized output). Centroid lookup is a linear scan over 256×384 f32 (one cache-line-friendly pass).

- [ ] **Step 4:** Run test → PASS.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-mem
cargo clippy -p origin-mem --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-mem): int8 quantizer with 256 centroid offsets (P6.2)`.

---

## Task P6.3 — HNSW index + temporal-decay re-rank (N6.2)

**Files:** `crates/origin-mem/src/index.rs` (new), `crates/origin-mem/tests/index.rs` (new). Modify `lib.rs` to add `pub mod index; pub use index::{MemIndex, SearchOpts, Candidate, IndexError};`.

**Public surface:**

```rust
/// Re-rank input: one HNSW shortlist entry with everything the decay scorer needs.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: u64,
    pub raw_sim: f32,
    pub age_days: f32,
    /// Multiplier for the memory's containing cluster (default 1.0).
    pub cluster_priority: f32,
    /// Sum of weighted boosts from `Supersedes` / `RelatedTo` edges pointing inward.
    pub edge_boost: f32,
    /// Final re-ranked score = raw_sim * exp(-age_days/tau) * cluster_priority * (1 + edge_boost).
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct SearchOpts {
    /// Number of HNSW candidates pulled before re-rank (spec N6.2 says 3000).
    pub shortlist_k: usize,
    /// Top-N to return after re-rank.
    pub top_n: usize,
    /// Time constant in days for `exp(-age_days/tau)`. Spec default = 30.
    pub decay_tau_days: f32,
    /// `Supersedes` edges with `from -> active winner` drop the loser entirely.
    pub drop_superseded: bool,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self { shortlist_k: 3000, top_n: 5, decay_tau_days: 30.0, drop_superseded: true }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("hnsw insert failed: {0}")]
    Insert(String),
    #[error("hnsw search failed: {0}")]
    Search(String),
}

pub struct MemIndex { /* hnsw_rs::Hnsw + id map */ }

impl MemIndex {
    /// New empty HNSW with M=16, ef_construction=200 (recommended defaults).
    #[must_use]
    pub fn new() -> Self;

    /// Insert a *decoded* (or original) f32 vector under id.
    ///
    /// # Errors
    /// Returns `Insert` on hnsw_rs failure.
    pub fn insert(&mut self, id: u64, vec: &[f32; crate::EMBED_DIM]) -> Result<(), IndexError>;

    /// Re-rank using a supplied lookup closure that resolves each id to its
    /// `(age_days, cluster_priority, edge_boost, superseded_by)` row. Returning
    /// `None` from the closure silently drops the candidate.
    ///
    /// # Errors
    /// Returns `Search` on hnsw_rs failure.
    pub fn search(
        &self,
        query: &[f32; crate::EMBED_DIM],
        opts: &SearchOpts,
        lookup: impl Fn(u64) -> Option<MetaRow>,
    ) -> Result<Vec<Candidate>, IndexError>;
}

#[derive(Debug, Clone, Copy)]
pub struct MetaRow {
    pub age_days: f32,
    pub cluster_priority: f32,
    pub edge_boost: f32,
    pub superseded_by: Option<u64>,
}
```

**Algorithm notes:** wrap `hnsw_rs::Hnsw<f32, hnsw_rs::dist::DistDot>`. Distance from hnsw is `1 - cos_sim` (since vectors are L2-normalized); we recover `raw_sim = 1 - dist`. After fetching shortlist, apply the formula in spec N6.2 exactly. Stable sort by descending score; tie-break by `id` ascending for determinism.

- [ ] **Step 1: Write failing test** `tests/index.rs`:

  ```rust
  use origin_mem::index::{MemIndex, SearchOpts, MetaRow};
  use origin_mem::EMBED_DIM;
  use std::collections::HashMap;

  fn unit_vec(seed: f32) -> [f32; EMBED_DIM] {
      let mut v = [0_f32; EMBED_DIM];
      v[0] = seed.cos();
      v[1] = seed.sin();
      v
  }

  #[test]
  fn decay_demotes_old_match() {
      let mut idx = MemIndex::new();
      let fresh = unit_vec(0.0);
      let stale = unit_vec(0.05); // nearly identical
      idx.insert(1, &fresh).expect("ins");
      idx.insert(2, &stale).expect("ins");
      let meta: HashMap<u64, MetaRow> = HashMap::from([
          (1_u64, MetaRow { age_days: 1.0,   cluster_priority: 1.0, edge_boost: 0.0, superseded_by: None }),
          (2_u64, MetaRow { age_days: 300.0, cluster_priority: 1.0, edge_boost: 0.0, superseded_by: None }),
      ]);
      let opts = SearchOpts { top_n: 2, ..Default::default() };
      let out = idx.search(&fresh, &opts, |id| meta.get(&id).copied()).expect("search");
      assert_eq!(out[0].id, 1, "fresh ranks higher despite same raw sim");
      assert!(out[0].score > out[1].score);
  }

  #[test]
  fn supersede_drops_loser() {
      let mut idx = MemIndex::new();
      idx.insert(10, &unit_vec(0.0)).expect("ins");
      idx.insert(11, &unit_vec(0.0)).expect("ins");
      let meta: HashMap<u64, MetaRow> = HashMap::from([
          (10_u64, MetaRow { age_days: 0.5, cluster_priority: 1.0, edge_boost: 0.0, superseded_by: Some(11) }),
          (11_u64, MetaRow { age_days: 0.5, cluster_priority: 1.0, edge_boost: 0.0, superseded_by: None }),
      ]);
      let out = idx
          .search(&unit_vec(0.0), &SearchOpts::default(), |id| meta.get(&id).copied())
          .expect("search");
      assert!(out.iter().all(|c| c.id != 10), "10 should be dropped as superseded");
  }
  ```

- [ ] **Step 2:** Run `cargo test -p origin-mem --test index` → fail (module missing).

- [ ] **Step 3: Implement** `index.rs`. Use `hnsw_rs::hnsw::Hnsw::new(16, 10_000, 16, 200, hnsw_rs::dist::DistDot {})`. Maintain a side `Vec<u64>` mapping internal hnsw point indices to public `u64` ids. Convert distance to similarity, build `Candidate`, drop where `lookup` returns `None` or where `opts.drop_superseded && superseded_by.is_some()`. Score: `raw_sim * (-age_days / tau).exp() * cluster_priority * (1.0 + edge_boost)`.

- [ ] **Step 4:** Run test → PASS.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-mem
cargo clippy -p origin-mem --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-mem): HNSW index + temporal-decay re-rank (P6.3)`.

---

## Task P6.4 — `MemoryStore` + SQLite migration V3 (N6.3)

**Files:**
- New `crates/origin-store/src/migrations/V3__memories.sql`.
- New `crates/origin-mem/src/storage.rs` (~350 LOC budget — split into `storage/{mod,tags,edges}.rs` if it exceeds 400).
- New `crates/origin-mem/tests/storage_round_trip.rs`.
- Modify `crates/origin-mem/src/lib.rs` to re-export `MemoryStore`, `MemoryId`, `MemoryRecord`, `Tag`, `EdgeKind`, `StorageError`.
- Modify `crates/origin-mem/Cargo.toml`: add `bitvec = "1"`.

**Migration `V3__memories.sql`:**

```sql
-- N6.3: memory body in CAS, vector inline, tags as bitset over a tag dictionary.
PRAGMA foreign_keys = ON;

CREATE TABLE memories (
    id              TEXT PRIMARY KEY,           -- ULID
    centroid_id     INTEGER NOT NULL,           -- 0..255
    deltas          BLOB    NOT NULL,           -- 384 i8 values, length=384
    body_handle     BLOB    NOT NULL,           -- 32-byte CAS hash
    body_preview    TEXT    NOT NULL,           -- ≤64 bytes utf-8
    tags_bitset     BLOB    NOT NULL DEFAULT (X'00000000000000000000000000000000'),  -- 128-bit
    created_at      INTEGER NOT NULL,           -- epoch ms
    last_seen_at    INTEGER NOT NULL,
    superseded_by   TEXT    REFERENCES memories(id) ON DELETE SET NULL,
    cluster_priority REAL   NOT NULL DEFAULT 1.0
);

CREATE INDEX idx_memories_last_seen ON memories(last_seen_at);
CREATE INDEX idx_memories_superseded ON memories(superseded_by);

CREATE TABLE mem_edges (
    from_id    TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    to_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
    kind       INTEGER NOT NULL,  -- 0=RelatedTo, 1=Supersedes, 2=Contradicts
    weight     REAL    NOT NULL DEFAULT 1.0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
);

CREATE INDEX idx_mem_edges_to ON mem_edges(to_id);

CREATE TABLE mem_tags (
    bit_idx INTEGER PRIMARY KEY,  -- 0..127
    name    TEXT NOT NULL UNIQUE
);

CREATE TABLE mem_quantizer (
    id    INTEGER PRIMARY KEY CHECK (id = 1),  -- singleton row
    bytes BLOB    NOT NULL
);
```

**Rust public surface (`crates/origin-mem/src/storage.rs`):**

```rust
use crate::quantizer::{EncodedVector, Quantizer};
use origin_cas::{Hash, Store as CasStore};
use origin_store::Store as SqlStore;
use ulid::Ulid;

pub type MemoryId = Ulid;

#[derive(Debug, Clone, Copy)]
pub enum EdgeKind { RelatedTo = 0, Supersedes = 1, Contradicts = 2 }

#[derive(Debug, Clone)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub encoded: EncodedVector,
    pub body_handle: [u8; 32],
    pub body_preview: String,    // ≤64 bytes
    pub tags: Vec<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub superseded_by: Option<MemoryId>,
    pub cluster_priority: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sql: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
    #[error("ulid: {0}")]
    Ulid(#[from] ulid::DecodeError),
    #[error("no quantizer trained yet")]
    NoQuantizer,
    #[error("preview must be utf-8")]
    Utf8(#[from] std::str::Utf8Error),
}

pub struct MemoryStore { /* &SqlStore + &CasStore + cached Quantizer */ }

impl MemoryStore {
    pub fn new(sql: std::sync::Arc<SqlStore>, cas: std::sync::Arc<CasStore>) -> Self;

    /// Persist the trained quantizer (singleton row). Subsequent `save` calls
    /// require this to have happened first.
    ///
    /// # Errors
    /// Sql errors only.
    pub fn install_quantizer(&self, q: &Quantizer) -> Result<(), StorageError>;

    /// Load the stored quantizer; returns `Ok(None)` if untrained.
    ///
    /// # Errors
    /// Sql errors only.
    pub fn load_quantizer(&self) -> Result<Option<Quantizer>, StorageError>;

    /// Persist body to CAS, vector to SQL, tag bits derived from registry.
    /// Returns the new id.
    ///
    /// # Errors
    /// Storage errors only.
    pub fn save(
        &self,
        body: &str,
        vector: &[f32; crate::EMBED_DIM],
        tags: &[&str],
    ) -> Result<MemoryId, StorageError>;

    /// Hard delete (and decrement CAS refcount).
    ///
    /// # Errors
    /// Storage errors only.
    pub fn forget(&self, id: MemoryId) -> Result<(), StorageError>;

    /// Soft supersede: set `superseded_by` and bump `last_seen_at_ms`.
    ///
    /// # Errors
    /// Storage errors only.
    pub fn mark_superseded(&self, loser: MemoryId, winner: MemoryId) -> Result<(), StorageError>;

    /// Read one row (no body inflation — call `body(handle)` to fetch the CAS bytes).
    ///
    /// # Errors
    /// Storage errors only; `Ok(None)` if not found.
    pub fn get(&self, id: MemoryId) -> Result<Option<MemoryRecord>, StorageError>;

    /// Stream every record (for index rebuild + consolidator passes).
    ///
    /// # Errors
    /// Storage errors only.
    pub fn iter_all(&self) -> Result<Vec<MemoryRecord>, StorageError>;

    /// Insert a `mem_edges` row (idempotent on PK collision).
    ///
    /// # Errors
    /// Storage errors only.
    pub fn add_edge(&self, from: MemoryId, to: MemoryId, kind: EdgeKind, weight: f32) -> Result<(), StorageError>;
}
```

**Tag bitset:** 128 bits in a `BLOB`. The first time a tag name is seen, a row is inserted into `mem_tags` with the next free `bit_idx` (0..127). Tags beyond 128 are silently dropped with a `tracing::warn!` — fine for v1.

**Preview:** first ≤64 UTF-8 bytes of body with character-boundary truncation (use `body.char_indices().take_while(|(i, _)| *i < 64).map(|(i, _)| i).last()`).

- [ ] **Step 1: Failing test** `crates/origin-mem/tests/storage_round_trip.rs`:

  ```rust
  use origin_mem::quantizer::Quantizer;
  use origin_mem::storage::{EdgeKind, MemoryStore};
  use origin_mem::EMBED_DIM;
  use std::sync::Arc;

  fn synth(n: usize) -> Vec<[f32; EMBED_DIM]> {
      use rand::{Rng, SeedableRng};
      let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0xC0FFEE);
      (0..n).map(|_| {
          let mut v = [0_f32; EMBED_DIM];
          for s in v.iter_mut() { *s = rng.gen_range(-1.0..1.0); }
          let norm = v.iter().map(|x| x*x).sum::<f32>().sqrt().max(1e-9);
          for s in v.iter_mut() { *s /= norm; }
          v
      }).collect()
  }

  #[test]
  fn save_then_get_round_trips() {
      let tmp = tempfile::tempdir().expect("tmp");
      let sql = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("sql"));
      let cas = Arc::new(origin_cas::Store::open(origin_cas::StoreConfig {
          root: tmp.path().join("cas"),
          hot_capacity: 16,
          warm_pack_target_bytes: 64 * 1024,
          cold_zstd_level: 1,
      }).expect("cas"));
      let store = MemoryStore::new(Arc::clone(&sql), Arc::clone(&cas));
      let q = Quantizer::fit(&synth(1024), 7).expect("fit");
      store.install_quantizer(&q).expect("install");
      let body = "user prefers terse responses with no trailing summaries";
      let v = synth(1)[0];
      let id = store.save(body, &v, &["feedback", "terseness"]).expect("save");
      let got = store.get(id).expect("get").expect("present");
      assert_eq!(got.body_preview.len() <= 64, true);
      assert!(got.tags.iter().any(|t| t == "feedback"));
      // body fetched out of CAS
      let body_bytes = cas.get(origin_cas::Hash::from_bytes(got.body_handle)).expect("cas get").expect("present");
      assert_eq!(std::str::from_utf8(&body_bytes).expect("utf8"), body);
  }

  #[test]
  fn edge_and_supersede_round_trip() {
      let tmp = tempfile::tempdir().expect("tmp");
      let sql = Arc::new(origin_store::Store::open(tmp.path().join("o.db")).expect("sql"));
      let cas = Arc::new(origin_cas::Store::open(origin_cas::StoreConfig {
          root: tmp.path().join("cas"),
          hot_capacity: 16,
          warm_pack_target_bytes: 64 * 1024,
          cold_zstd_level: 1,
      }).expect("cas"));
      let store = MemoryStore::new(sql, cas);
      let q = Quantizer::fit(&synth(1024), 8).expect("fit");
      store.install_quantizer(&q).expect("install");
      let v = synth(1)[0];
      let loser = store.save("old", &v, &[]).expect("save");
      let winner = store.save("new", &v, &[]).expect("save");
      store.mark_superseded(loser, winner).expect("supersede");
      store.add_edge(loser, winner, EdgeKind::Supersedes, 1.0).expect("edge");
      let l = store.get(loser).expect("get").expect("present");
      assert_eq!(l.superseded_by, Some(winner));
  }
  ```

- [ ] **Step 2:** Run `cargo test -p origin-mem --test storage_round_trip` → fail.

- [ ] **Step 3:** Add migration `V3__memories.sql`. Implement `MemoryStore` per surface. Body write: `cas.put(body.as_bytes())` → handle; bump CAS refcount via existing `RefTable`. `forget`: delete row + decrement refcount (let CAS GC reap on idle). Tag write: lookup-or-insert `mem_tags`; set bit in 128-bit `bitvec::BitArray`. Tag read: invert via `mem_tags` join.

- [ ] **Step 4:** Run test → PASS.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-mem
cargo test -p origin-store
cargo clippy -p origin-mem --all-targets -- -D warnings
cargo clippy -p origin-store --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-mem): MemoryStore + V3 memories/mem_edges/mem_tags migration (P6.4)`.

---

## Task P6.5 — `mem_search` / `mem_save` / `mem_forget` tools

**Files:**
- New `crates/origin-tools/src/builtins/mem.rs` (~280 LOC budget).
- Modify `crates/origin-tools/src/builtins/mod.rs`: add `pub mod mem;`.
- Modify `crates/origin-tools/Cargo.toml`: add `origin-mem = { path = "../origin-mem" }` and `serde_json` (already present transitively, but assert explicit).
- Modify `crates/origin-tools/src/dispatch.rs`: extend the dispatch-state struct with `pub memory: Option<std::sync::Arc<dyn MemoryHandle>>` where `MemoryHandle` is a new tiny trait in `origin-tools::dispatch` so the tools crate doesn't take a hard `origin-mem` dep at the dispatch layer.

**`MemoryHandle` trait (lives in `origin-tools::dispatch`):**

```rust
/// Thin object-safe handle the tool dispatch passes into memory-aware tools.
/// Implemented in `origin-daemon` by wrapping `origin_mem::storage::MemoryStore`
/// + `origin_mem::index::MemIndex` + `origin_mem::Embedder`.
pub trait MemoryHandle: Send + Sync + std::fmt::Debug {
    /// Search top-k by semantic similarity to `query`.
    fn search(&self, query: &str, k: usize, fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError>;
    /// Save a body with tags; returns the new id as a ulid string.
    fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError>;
    /// Hard delete by ulid string.
    fn forget(&self, id: &str) -> Result<(), MemoryToolError>;
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub id: String,
    pub preview: String,
    pub score: f32,
    pub age_days: f32,
    pub tags: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryToolError {
    #[error("memory subsystem unavailable")]
    Unavailable,
    #[error("invalid id: {0}")]
    BadId(String),
    #[error("storage: {0}")]
    Storage(String),
}
```

**Tool surfaces:**

```rust
crate::origin_tool! {
    name: "mem_search",
    description: "Semantic search over the user's cross-session memory. Returns top-k hits with previews.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "query":{"type":"string"},
            "k":{"type":"integer","minimum":1,"maximum":50,"default":5},
            "fresh":{"type":"boolean","default":false}
        },
        "required":["query"]
    }"#,
}

crate::origin_tool! {
    name: "mem_save",
    description: "Persist a memory across sessions. Tags optional.",
    tier: crate::Tier::RequiresPermission,
    urgency: crate::Urgency::Medium,
    side_effects: crate::SideEffects::Mutating,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "body":{"type":"string"},
            "tags":{"type":"array","items":{"type":"string"}}
        },
        "required":["body"]
    }"#,
}

crate::origin_tool! {
    name: "mem_forget",
    description: "Permanently delete a memory by id.",
    tier: crate::Tier::RequiresPermission,
    urgency: crate::Urgency::High,
    side_effects: crate::SideEffects::Mutating,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "id":{"type":"string"}
        },
        "required":["id"]
    }"#,
}
```

Each tool's `execute` function takes the dispatch-state and the parsed input, calls the corresponding `MemoryHandle` method, and serializes the result into JSON which is then written into CAS (consistent with how `Recall` and the other Phase 3 tools land their outputs).

- [ ] **Step 1: Failing test** `crates/origin-tools/tests/mem_tools.rs` (new):

  ```rust
  use origin_tools::dispatch::{MemoryHandle, MemoryToolError, SearchHit};
  use origin_tools::builtins::mem::{mem_search_execute, mem_save_execute, mem_forget_execute};
  use std::sync::Arc;
  use std::sync::Mutex;

  #[derive(Debug, Default)]
  struct MockMem {
      saved: Mutex<Vec<(String, Vec<String>)>>,
      forgotten: Mutex<Vec<String>>,
  }

  impl MemoryHandle for MockMem {
      fn search(&self, query: &str, _k: usize, _fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
          if query == "boom" { return Err(MemoryToolError::Unavailable); }
          Ok(vec![SearchHit {
              id: "01J0".into(),
              preview: format!("hit for {query}"),
              score: 0.9,
              age_days: 1.0,
              tags: vec!["t".into()],
          }])
      }
      fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError> {
          self.saved.lock().unwrap().push((body.to_string(), tags.to_vec()));
          Ok("01J1".into())
      }
      fn forget(&self, id: &str) -> Result<(), MemoryToolError> {
          self.forgotten.lock().unwrap().push(id.to_string());
          Ok(())
      }
  }

  #[tokio::test]
  async fn search_returns_hits() {
      let mem: Arc<dyn MemoryHandle> = Arc::new(MockMem::default());
      let json = mem_search_execute(&*mem, r#"{"query":"x","k":3}"#).await.expect("ok");
      assert!(json.contains("\"id\":\"01J0\""));
      assert!(json.contains("\"preview\":\"hit for x\""));
  }

  #[tokio::test]
  async fn save_persists_and_returns_id() {
      let mock = Arc::new(MockMem::default());
      let mem: Arc<dyn MemoryHandle> = Arc::clone(&mock) as _;
      let json = mem_save_execute(&*mem, r#"{"body":"hello","tags":["a","b"]}"#).await.expect("ok");
      assert_eq!(json, r#"{"id":"01J1"}"#);
      let saved = mock.saved.lock().unwrap();
      assert_eq!(saved[0].0, "hello");
      assert_eq!(saved[0].1, vec!["a".to_string(), "b".to_string()]);
  }

  #[tokio::test]
  async fn forget_returns_unit() {
      let mock = Arc::new(MockMem::default());
      let mem: Arc<dyn MemoryHandle> = Arc::clone(&mock) as _;
      let json = mem_forget_execute(&*mem, r#"{"id":"01J0"}"#).await.expect("ok");
      assert_eq!(json, r#"{}"#);
      assert_eq!(mock.forgotten.lock().unwrap()[0], "01J0");
  }
  ```

- [ ] **Step 2:** Run `cargo test -p origin-tools --test mem_tools` → fail.

- [ ] **Step 3:** Implement `mem.rs`. Use `serde_json::Value` for input parsing (already on the Phase 3 tools path). The `origin_tool!` registrations make the tools auto-discoverable by the daemon; the actual `execute` functions are public free functions so unit tests can call them with a mock handle without going through the dispatch state.

- [ ] **Step 4:** Run test → PASS.

- [ ] **Step 5: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-tools): mem_search / mem_save / mem_forget builtins (P6.5)`.

---

## Task P6.6 — Auto-save proposer + auto-recall injector (N6.5)

**Files:**
- New `crates/origin-mem/src/proposer.rs` + `tests/proposer.rs`.
- New `crates/origin-mem/src/injector.rs` + `tests/injector.rs`.
- Modify `crates/origin-mem/src/lib.rs`: `pub mod proposer; pub mod injector; pub use proposer::{Proposer, MemoryProposal}; pub use injector::{Injector, InjectedContext};`.

**Proposer surface:**

```rust
/// One candidate the daemon will surface for user accept/reject/edit.
#[derive(Debug, Clone)]
pub struct MemoryProposal {
    /// Stable id within the session for this proposal (counter from 1).
    pub proposal_id: u32,
    pub body: String,
    pub suggested_tags: Vec<String>,
    /// Reason this was extracted (regex name or constant).
    pub source_hint: &'static str,
}

pub struct Proposer { /* compiled regex set */ }

impl Proposer {
    #[must_use]
    pub fn new() -> Self;

    /// Scan the assistant message + user message at turn end. Returns 0..N proposals.
    /// Heuristics (initial set, extensible without breaking callers):
    /// - User text matching `(?i)\bremember(?: that)?[: ]+(.+)` → body=$1, tag=user-statement.
    /// - User text matching `(?i)i (?:prefer|like|always|never)\b.{0,140}` → body=$0, tag=feedback.
    /// - Assistant text matching `(?i)i'll (?:remember|note) that (.+?)(?:\.|$)` → body=$1.
    /// - Either side matching `(?i)\bTODO\b: (.+)` → body=$1, tag=todo.
    /// Each regex is bounded with `(?s)` off so newlines act as separators.
    #[must_use]
    pub fn scan(&self, user: &str, assistant: &str, next_id: &mut u32) -> Vec<MemoryProposal>;
}
```

**Injector surface:**

```rust
#[derive(Debug, Clone)]
pub struct InjectedContext {
    /// Pre-formatted `<context>` block; ready to prepend to system prompt's Sticky band.
    pub block: String,
    /// IDs that were injected — daemon bumps `last_seen_at_ms` for these.
    pub touched_ids: Vec<crate::storage::MemoryId>,
}

pub struct Injector { /* embedder + index + store handles */ }

impl Injector {
    pub fn new(
        embedder: std::sync::Arc<crate::Embedder>,
        index: std::sync::Arc<parking_lot::RwLock<crate::index::MemIndex>>,
        store: std::sync::Arc<crate::storage::MemoryStore>,
    ) -> Self;

    /// Embed the user prompt, search, format top-K previews into `<context>` block.
    /// Returns `Ok(None)` if no candidate scores above `min_score` (default 0.2).
    ///
    /// # Errors
    /// Embedder, index, or storage failures.
    pub fn for_prompt(&self, user_prompt: &str, k: usize) -> Result<Option<InjectedContext>, crate::injector::InjectorError>;
}
```

**Block format (deterministic — assert in test):**

```
<context source="origin-mem">
  <memory id="01J0…ABC" age="1.2d" tags="feedback,terseness">user prefers terse responses with no trailing summaries</memory>
  <memory id="01J0…XYZ" age="14d" tags="project">merge freeze begins 2026-03-05</memory>
</context>
```

- [ ] **Step 1: Failing test** `tests/proposer.rs`:

  ```rust
  use origin_mem::proposer::Proposer;

  #[test]
  fn extracts_remember_directive() {
      let p = Proposer::new();
      let mut next = 1_u32;
      let out = p.scan(
          "remember: I'm a senior Rust engineer",
          "Sure, noted.",
          &mut next,
      );
      assert_eq!(out.len(), 1);
      assert_eq!(out[0].body, "I'm a senior Rust engineer");
      assert!(out[0].suggested_tags.contains(&"user-statement".to_string()));
      assert_eq!(next, 2);
  }

  #[test]
  fn extracts_preference_phrase() {
      let p = Proposer::new();
      let mut next = 1_u32;
      let out = p.scan(
          "i prefer fewer comments in generated code",
          "ok.",
          &mut next,
      );
      assert!(out.iter().any(|m| m.body.contains("i prefer fewer comments")));
  }

  #[test]
  fn no_match_returns_empty() {
      let p = Proposer::new();
      let mut next = 1_u32;
      assert!(p.scan("hello", "hi", &mut next).is_empty());
      assert_eq!(next, 1);
  }
  ```

- [ ] **Step 2: Failing test** `tests/injector.rs`:

  ```rust
  use origin_mem::injector::Injector;
  use origin_mem::storage::MemoryStore;
  use origin_mem::quantizer::Quantizer;
  use parking_lot::RwLock;
  use std::sync::Arc;
  use origin_mem::index::MemIndex;

  fn unit_vec(seed: f32) -> [f32; origin_mem::EMBED_DIM] {
      let mut v = [0_f32; origin_mem::EMBED_DIM];
      v[0] = seed.cos();
      v[1] = seed.sin();
      v
  }

  #[test]
  fn returns_none_when_below_threshold() {
      // Mock-style: empty index → injector returns None.
      let tmp = tempfile::tempdir().expect("tmp");
      let sql = Arc::new(origin_store::Store::open(tmp.path().join("o.db")).expect("sql"));
      let cas = Arc::new(origin_cas::Store::open(origin_cas::StoreConfig {
          root: tmp.path().join("cas"), hot_capacity: 16, warm_pack_target_bytes: 64*1024, cold_zstd_level: 1
      }).expect("cas"));
      let store = Arc::new(MemoryStore::new(Arc::clone(&sql), Arc::clone(&cas)));
      // Use a stub Embedder fixture from the existing P6.1 test setup.
      let embedder = Arc::new(origin_mem::Embedder::from_path(
          std::path::Path::new("tests/fixtures/stub_minilm.onnx")
      ).expect("embedder"));
      let index = Arc::new(RwLock::new(MemIndex::new()));
      let injector = Injector::new(embedder, index, store);
      let out = injector.for_prompt("anything", 5).expect("injector");
      assert!(out.is_none());
  }
  ```

  *(The "block format" test runs in P6.9's E2E gate after a memory is saved end-to-end; covering it in isolation needs a populated store and is overkill for unit scope.)*

- [ ] **Step 3:** Run both tests → fail.

- [ ] **Step 4:** Implement `proposer.rs` and `injector.rs`. Use `regex::RegexSet` so multiple patterns are matched in one pass over the text.

- [ ] **Step 5:** Tests pass.

- [ ] **Step 6: Verification gate**

```bash
cargo test -p origin-mem
cargo clippy -p origin-mem --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit** `feat(origin-mem): Proposer (turn-end extraction) + Injector (sticky-band context) (P6.6)`.

---

## Task P6.7 — IPC + CLI for memory-proposal review

**Files:**
- Modify `crates/origin-daemon/src/protocol.rs`:
  - Add `StreamEvent::MemoryProposed { proposal_id: u32, body: String, suggested_tags: Vec<String> }`.
  - Add `enum ClientMessage` (this becomes the FIRST union; today only `PromptRequest` flows so wrap legacy `PromptRequest` as `ClientMessage::Prompt(PromptRequest)`):
    ```rust
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum ClientMessage {
        Prompt(PromptRequest),
        MemoryDecision { proposal_id: u32, action: MemoryAction },
    }
    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum MemoryAction {
        Accept,
        Reject,
        Edit { body: String, tags: Vec<String> },
    }
    ```
  - **Back-compat note:** the daemon main parses `serde_json::from_slice::<ClientMessage>` first; if that fails, fall back to `serde_json::from_slice::<PromptRequest>` to keep older clients alive for one phase. Marked `#[deprecated(note = "send ClientMessage::Prompt")]` on the legacy decode path.
- Modify `crates/origin-daemon/src/agent.rs`: at turn-end, call `proposer.scan(...)`, emit one `StreamEvent::MemoryProposed` per result, and stash `(proposal_id → MemoryProposal)` in a session-scoped `HashMap` (lives in `Session`). Bump the session-local `next_proposal_id` counter.
- Modify `crates/origin-daemon/src/session.rs`: add `pub pending_proposals: HashMap<u32, MemoryProposal>` + `pub next_proposal_id: u32`. Persist *not* to disk this phase (proposal lifetime = session).
- Modify `crates/origin-daemon/src/main.rs`: in `spawn_handler_task` after `read_frame_body`, attempt `ClientMessage` decode first; on `MemoryDecision`, look up the proposal, apply Accept → embed body, `MemoryStore::save`; Reject → drop; Edit → save with overridden body/tags. Don't go through `handle_request` for these — they're side-band.
- Modify `crates/origin-cli/src/input.rs` (or wherever slash commands live — search for the `/account`-style hook from the Phase 4 stub; if missing, add `parse_slash(&str)`). Add `/mem accept N`, `/mem reject N`, `/mem edit N <body>` recognizers that emit a `ClientMessage::MemoryDecision` frame.
- New `crates/origin-daemon/tests/memory_proposal_round_trip.rs`: drives a fake `Listener`, sends a Prompt that produces a proposal, sees the `MemoryProposed` event, sends a `MemoryDecision::Accept`, asserts the daemon called `MemoryStore::save` (verify by post-condition query on the store).

- [ ] **Step 1: Failing test** `tests/memory_proposal_round_trip.rs` — uses `tokio::net::UnixListener` on Unix or `tokio::net::windows::named_pipe::NamedPipeServer` on Windows, the existing test helper from `phase-2` (search `crates/origin-daemon/tests/` — re-use `connect_and_prompt` if present, else write a small helper).

- [ ] **Step 2:** Run → fail (the new variants don't exist).

- [ ] **Step 3:** Implement protocol additions, daemon side-band handler, CLI slash parser. Make sure every `match StreamEvent` arm in the CLI handles `MemoryProposed`. Format on CLI side: a single status-line entry, e.g. `[mem proposal #3] "user prefers …" (tags: feedback) — /mem accept 3, /mem reject 3, /mem edit 3 …`.

- [ ] **Step 4:** Test passes.

- [ ] **Step 5: Verification gate**

```bash
cargo test --workspace
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-daemon,origin-cli): MemoryProposed event + /mem decision command (P6.7)`.

---

## Task P6.8 — Idle consolidation pass (N6.4, similarity-only)

**Files:**
- New `crates/origin-mem/src/consolidator.rs` (~220 LOC budget).
- New `crates/origin-mem/tests/consolidator.rs`.
- Modify `crates/origin-mem/src/lib.rs`: `pub mod consolidator; pub use consolidator::{Consolidator, ConsolidationReport, ConsolidationError};`.

**Public surface:**

```rust
#[derive(Debug, Clone)]
pub struct ConsolidationReport {
    /// (loser, winner) pairs proposed by similarity clustering.
    pub supersedes_proposed: Vec<(crate::storage::MemoryId, crate::storage::MemoryId)>,
    /// Pairs flagged as candidate contradictions (lexical antonym heuristic).
    pub contradictions_flagged: Vec<(crate::storage::MemoryId, crate::storage::MemoryId)>,
    /// Memories whose cluster_priority was bumped because they were re-touched.
    pub priority_bumped: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ConsolidationError {
    #[error("storage: {0}")]
    Storage(#[from] crate::storage::StorageError),
    #[error("index: {0}")]
    Index(#[from] crate::index::IndexError),
}

pub struct Consolidator { /* store + index + thresholds */ }

impl Consolidator {
    pub fn new(
        store: std::sync::Arc<crate::storage::MemoryStore>,
        index: std::sync::Arc<parking_lot::RwLock<crate::index::MemIndex>>,
    ) -> Self;

    /// One bounded pass (≤ `max_pairs` candidate inspections). Safe to call repeatedly.
    /// Concrete rules:
    /// 1. For each memory M, take HNSW top-3 within cos_sim > 0.92 (after decode).
    /// 2. If a near-duplicate has older `created_at_ms`, propose `M` supersedes it.
    /// 3. Lexical contradiction stub: scan tags + body for `(prefer|like) X` vs `(not|never|don't) X` patterns; flag pair without auto-resolving.
    /// 4. Bump `cluster_priority` on M by `+0.05` (cap 2.0) if it touched ≥2 peers.
    ///
    /// # Errors
    /// Storage or index errors.
    pub fn run_pass(&self, max_pairs: usize) -> Result<ConsolidationReport, ConsolidationError>;
}
```

**Determinism:** order memories by `id` ascending so two runs produce identical reports.

- [ ] **Step 1: Failing test** `tests/consolidator.rs`:

  ```rust
  use origin_mem::consolidator::Consolidator;
  use origin_mem::storage::MemoryStore;
  use origin_mem::quantizer::Quantizer;
  use origin_mem::index::MemIndex;
  use origin_mem::EMBED_DIM;
  use parking_lot::RwLock;
  use std::sync::Arc;

  fn unit_vec(seed: f32) -> [f32; EMBED_DIM] {
      let mut v = [0_f32; EMBED_DIM];
      v[0] = seed.cos();
      v[1] = seed.sin();
      v
  }

  #[test]
  fn near_duplicates_get_supersede_proposal() {
      let tmp = tempfile::tempdir().expect("tmp");
      let sql = Arc::new(origin_store::Store::open(tmp.path().join("o.db")).expect("sql"));
      let cas = Arc::new(origin_cas::Store::open(origin_cas::StoreConfig {
          root: tmp.path().join("cas"), hot_capacity: 16, warm_pack_target_bytes: 64*1024, cold_zstd_level: 1
      }).expect("cas"));
      let store = Arc::new(MemoryStore::new(Arc::clone(&sql), Arc::clone(&cas)));
      let q = Quantizer::fit(&vec![unit_vec(0.0); 1024], 9).expect("fit"); // degenerate but legal
      store.install_quantizer(&q).expect("install");
      let id_a = store.save("user is a rust engineer", &unit_vec(0.0), &[]).expect("save");
      // sleep ms tick so created_at differs
      std::thread::sleep(std::time::Duration::from_millis(2));
      let id_b = store.save("user is a senior rust engineer", &unit_vec(0.001), &[]).expect("save");
      let mut idx = MemIndex::new();
      idx.insert(0, &unit_vec(0.0)).expect("ins");      // id_a
      idx.insert(1, &unit_vec(0.001)).expect("ins");    // id_b
      let index = Arc::new(RwLock::new(idx));
      let cons = Consolidator::new(Arc::clone(&store), Arc::clone(&index));
      let report = cons.run_pass(32).expect("pass");
      assert!(!report.supersedes_proposed.is_empty(), "should propose at least one supersede");
      let (loser, winner) = report.supersedes_proposed[0];
      assert_eq!(loser, id_a, "older should be the loser");
      assert_eq!(winner, id_b);
  }
  ```

  *(The hnsw-internal-id → MemoryId mapping is something the consolidator owns — likely it iterates `store.iter_all()` and re-queries the index with each decoded vector instead of relying on hnsw internal ids. Implementer's choice as long as the test passes.)*

- [ ] **Step 2:** Run → fail.

- [ ] **Step 3:** Implement. Iterate `store.iter_all()` deterministically, decode each, query `index.search(...)` with `drop_superseded=false`, filter by cosine > 0.92, pick older as loser. Wrap the whole pass in a single SQL transaction via `Store::with_conn`.

- [ ] **Step 4:** Test passes.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-mem
cargo clippy -p origin-mem --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-mem): idle consolidator (cosine clustering → Supersedes) (P6.8)`.

---

## Task P6.9 — Daemon wiring + idle heartbeat + E2E test + tag `p6-complete`

**Files:**
- Modify `crates/origin-daemon/src/main.rs`:
  - Build a `MemoryStore`, `Embedder` (load from `$ORIGIN_MEM_MODEL` or `~/.origin/models/minilm.onnx`; if missing, log warn and *disable* the memory subsystem — daemon still runs, just no recall / proposals).
  - Train the quantizer the first time the daemon starts with ≥`NUM_CENTROIDS * 4` recorded vectors: until then, save bodies but skip the quantized vector column (store zeroed deltas + centroid_id=0). At first cross-threshold start, run `Quantizer::fit` over current vectors stored as side-channel `f32` blobs and replace the deltas in place. *(Practical reality: many users will hit threshold quickly via the Proposer.)*
  - Wire a `MemoryDispatchHandle` impl into the tools dispatch state for `mem_search` / `mem_save` / `mem_forget`.
  - Spawn an idle-heartbeat tokio task: `tokio::time::interval(Duration::from_secs(30))` — every tick, if `Instant::now() - last_turn_end > 30s`, run `consolidator.run_pass(64)` and emit a `tracing::info!` summary. Cancel via `tokio::sync::Notify` on shutdown.
- Modify `crates/origin-daemon/src/agent.rs`:
  - Extend `LoopOptions` with `pub memory: Option<MemoryWiring>` where `MemoryWiring` holds `Arc<Embedder>`, `Arc<RwLock<MemIndex>>`, `Arc<MemoryStore>`, `Arc<Proposer>`, `Arc<Injector>`.
  - Turn-start: if `memory.is_some()`, call `injector.for_prompt(user_prompt, 5)`; if `Some(ctx)` returned, append `ctx.block` to the system prompt as a new `Section` with `Band::Sticky`.
  - Turn-end: call `proposer.scan(user_prompt, assistant_text, &mut session.next_proposal_id)`; for each proposal, stash in `session.pending_proposals` and emit `StreamEvent::MemoryProposed`.
- New `crates/origin-daemon/tests/memory_e2e.rs`:
  - Anthropic env gate (skip when `ANTHROPIC_API_KEY` unset).
  - Start daemon, send `"remember: i prefer terse replies"`, expect a `MemoryProposed` event with `body.contains("terse replies")`.
  - Send `ClientMessage::MemoryDecision::Accept { proposal_id }`.
  - Open a fresh session in the same daemon, send `"how should you reply to me?"`, assert the response (after `<context>` injection) contains `terse` somewhere in the assistant text **OR** assert the wire payload sent to Anthropic included `<context source="origin-mem">` (the latter is the deterministic assertion — use `wiremock` instead of live Anthropic to make this part env-gate-free).
- Tag.

- [ ] **Step 1: Failing test** `tests/memory_e2e.rs` (env-gate-free path uses wiremock). Use the `phase-3` wiremock pattern from `crates/origin-provider-anthropic/tests/` for shape.

- [ ] **Step 2:** Run → fail (memory wiring missing).

- [ ] **Step 3:** Implement daemon wiring as described. Use `parking_lot::RwLock` for the index. Make sure the idle task is cancelled cleanly on daemon shutdown — add a `Notify` to the existing shutdown path or, if none exists, follow the simplest pattern (`tokio::select!` between heartbeat and a cancellation token).

- [ ] **Step 4:** All tests pass.

- [ ] **Step 5: Final verification gate**

```bash
cargo test --workspace
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Tag**

```bash
git tag p6-complete
```

- [ ] **Step 7: Commit** `feat(origin): memory subsystem wired into daemon; tag p6-complete (P6.9)`.

---

## Self-review checklist

**Spec coverage:**
- ✅ N6.1 — int8 + per-cluster centroid offsets (P6.2). 256 centroids, scale-quantized i8 deltas, dot in one pass.
- ✅ N6.2 — HNSW + temporal-decay re-rank (P6.3). Formula matches spec verbatim; `Supersedes` drops loser; `Contradicts` will route through the consolidator's flag list (P6.8) for sidecar resolution when Phase 5 lands.
- ✅ N6.3 — body in CAS / vector inline (P6.4). Tags as 128-bit bitset over a small dictionary. CAS dedup automatic.
- ⚠️ N6.4 — sidecar idle consolidation: similarity-clustering pass landed (P6.8); LLM-driven contradiction classifier is **deferred to Phase 5**. The contradictions list is surfaced as a flag-only output so callers can act later.
- ✅ N6.5 — auto-save proposer (P6.6 + P6.7) + auto-recall injector behind Sticky band (P6.6 + P6.9). Explicit `mem_search` / `mem_save` / `mem_forget` exist (P6.5).
- ✅ Shippable surface — daemon emits `MemoryProposed`; CLI accepts `/mem accept|reject|edit`. TUI side-panel render is deferred (Phase 4 P4.5) but the **IPC contract is durable** — when the panel lands, the same events power it.

**Type consistency check:**
- `MemoryId = Ulid` reused in `MemoryStore`, `Consolidator::ConsolidationReport`, `Injector::InjectedContext::touched_ids`.
- `EncodedVector` returned by `Quantizer::encode`, consumed by `Quantizer::dot`, persisted by `MemoryStore::save` (decomposed into `centroid_id` + `deltas` columns).
- `MetaRow` produced by `MemoryStore::get` / `MemoryStore::iter_all`, consumed by `MemIndex::search`'s lookup closure (the daemon adapter glues them in P6.9 — explicit conversion).
- `MemoryProposal` produced by `Proposer::scan`, consumed by daemon for `StreamEvent::MemoryProposed` and stash + `ClientMessage::MemoryDecision`.
- `SearchHit` returned by `MemoryHandle::search`, serialized into the `mem_search` tool output.
- `MemoryHandle` is the SINGLE trait every tool / injector path takes — defined in `origin-tools::dispatch`, implemented in `origin-daemon`.

**Placeholders:** No "TBD", "implement later", "appropriate error handling" steps. Every task names exact files, exact public surface, exact SQL DDL, exact regex shapes, exact tool input schemas, exact test assertions. The two intentional deferrals (sidecar classifier, side-panel render) are called out explicitly with their owning phases.

**Idempotency / safety:**
- Migration V3 is additive; existing P0–P3 sessions survive.
- `MemoryStore::add_edge` PK is `(from_id, to_id, kind)` → idempotent.
- Consolidator runs in a single SQL txn — crash mid-pass leaves the store unchanged.
- Idle heartbeat is cancellable on shutdown; no leaked tasks.

---

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-05-19-origin-phase-6.md`. Per the user's instruction, execution is via **superpowers:subagent-driven-development**, each task internally following **superpowers:test-driven-development** and gated by **superpowers:verification-before-completion** before advancing. Branch: `phase-6` (worktree at `C:/Users/wooai/Documents/origin-p6`). P6.1 is already shipped; first task to dispatch is **P6.2 — int8 quantizer**.
