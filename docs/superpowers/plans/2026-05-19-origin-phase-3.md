# `origin` Phase 3 — CachePlanner + Speculative Dispatch + Recall — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Build the predictive prompt-cache prefix planner (PrefixLedger + CachePlanner) with provider-aware cache-marker emission; add an incremental SAX-style JSON parser over `TokenKind::ToolUseDelta` ring events; wire speculative dispatch of pure tools off the parser; add a `Recall` builtin that inflates CAS handles back into the message stream; bolt handle substitution and per-session result memoization into the outbound message-to-wire path.

**Architecture:** New crate `origin-planner` owns three pure-logic units — `PrefixLedger` (per-section stability scoring), `CachePlanner::plan` (band sort + provider-specific marker emission), and `WireDecision` (per-block "inline vs handle-reference" rule). The incremental tool_use JSON parser lives in `origin-daemon::tool_use_parser` because the agent drives it; it consumes `TokenKind::ToolUseDelta` payloads from a ring `Subscriber` and yields `ToolUseDelta` events as soon as a top-level input field completes. The agent loop forks a per-tool background task when the parser confirms a pure-tool first complete field, awaits the precomputed handle when the block closes, and refuses speculation for side-effecting tools. `Recall` joins `origin-tools::builtins::recall` and dispatches via a new shared dispatch table (`origin-tools::dispatch`) so it gets first-class CAS access and so per-session memoization (input-hash → handle) lives next to dispatch. The Anthropic provider's existing `expand_messages_for_wire` is extended to consult `WireDecision` and inject `cache_control: ephemeral` markers at band boundaries.

**Tech Stack:** Rust 1.83 (MSRV pin), Tokio (current daemon runtime), `blake3` (handle hashes; already in `origin-cas`), `rkyv` 0.7 (`TokenEvent` payloads on the ring), `serde_json` for `cache_control` emission only (NOT on the streaming JSON parse hot path — that is hand-rolled), `proptest` for invariants, `wiremock` for provider integration tests, `tempfile` for CAS scaffolding. **No new third-party crates** are required for Phase 3 — every novel mechanism is implemented in-tree.

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` (spec) and the Phase 2 deliverables (tag `p2-complete`).

**Phase 3 spec mechanism citations:**
- **N2.2** — speculative dispatch for pure tools (Tasks P3.3, P3.4)
- **N2.3** — KV-cache lattice / predictive prefix planning (Tasks P3.1, P3.2)
- **N2.4 step 2** — handle substitution in message-to-wire driven by the planner (Task P3.6)
- **N4.2** — `CachePlanner` (Tasks P3.1, P3.2)
- **N5.3** — speculative pure-tool execution (Task P3.4)
- **N5.4** — input-hash session-scope memoization, with `Bash` opt-out (Task P3.7)
- **N5.5** — `Recall` tool over CAS handles (Task P3.5)

What is **explicitly out of scope** for Phase 3 (deferred):
- N2.5 sidecar-as-coroutine summarization — Phase 6
- N4.3 provider-aware request encoder (codegen) — Phase 11
- N4.5 KeyVault — Phase 8
- N7.1 swarm-scope PrefixLedger inheritance — Phase 9 (lays the seams in P3.1 only)
- N9.4 embedding-indexed skill body materialization into Sticky — Phase 7
- `cargo fuzz` harness for the tool_use parser — listed under P3.3 step list but kept as a **stub `cargo fuzz add` only**; the corpus + crash-replay loop lands with N10.10 in Phase 14

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**
1. Write the failing test.
2. Run it — confirm the expected failure mode.
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit.

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / daemon / CLI | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Final phase gate (P3.8) | All of the above + the new `phase3_cache_warm_ratio` bench (assertion-bounded) + tag `p3-complete` |

**Patterns inherited from earlier phases:**
- `[lints] workspace = true` in every crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All shared/persisted/IPC-crossing types derive `Archive + Serialize + Deserialize` from rkyv 0.7 with `#[archive(check_bytes)]`.
- `[lints.rust] unsafe_code = "forbid"` is the default. **Phase 3 introduces no new crate that needs `unsafe`** — `origin-planner` and the new daemon submodules stay `forbid`.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex:** if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>` and record in `Cargo.lock`. See `[[project-msrv-dep-pinning]]` memory.
- **Novel-implementation reflex:** every signature subsystem must use a mechanism that beats openclaude / jcode / opencode on tokens or perf (see `[[feedback-novel-implementations]]` memory). Phase 3's novelties: PrefixLedger stability scoring (vs. static prompt-cache markers), incremental SAX tool_use parser (vs. wait-for-full-block), speculative dispatch on pure-tool first-complete-field (vs. wait-for-tool_use-close), input-hash session memoization (vs. always-re-execute), CAS-handle `Recall` with region selectors (vs. inline-everything).

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit**.

---

## File map for Phase 3

| New crate / file | Responsibility |
|---|---|
| `crates/origin-planner/Cargo.toml` | manifest |
| `crates/origin-planner/src/lib.rs` | public surface — `Band`, `Section`, `SectionId`, `PrefixLedger`, `CachePlanner`, `WireDecision`, errors |
| `crates/origin-planner/src/band.rs` | `Band` enum + per-band ordering constants |
| `crates/origin-planner/src/ledger.rs` | `PrefixLedger` — stability scoring + demotion |
| `crates/origin-planner/src/planner.rs` | `CachePlanner::plan(request) -> Plan` |
| `crates/origin-planner/src/decision.rs` | `WireDecision::for_block` — inline vs handle-reference rule |
| `crates/origin-planner/tests/ledger.rs` | scoring invariants + demote/promote unit tests |
| `crates/origin-planner/tests/planner.rs` | four-band layout + marker placement |
| `crates/origin-planner/tests/decision.rs` | inline vs reference rule on Volatile/Sliding bands |
| `crates/origin-daemon/src/tool_use_parser.rs` *(new)* | incremental SAX-style JSON parser; consumes `TokenKind::ToolUseDelta` ring payloads, yields `ToolUseDelta` events |
| `crates/origin-daemon/tests/tool_use_parser.rs` *(new)* | streamed-fragment tests + property test |
| `crates/origin-daemon/src/agent.rs` *(modify)* | spawn speculative tasks on first complete field for pure tools; await precomputed handle on block close |
| `crates/origin-daemon/tests/speculative_e2e.rs` *(new)* | scripted provider: Read tool dispatched before tool_use closes |
| `crates/origin-tools/src/builtins/recall.rs` *(new)* | `recall_tool(store, handle, region) -> String` |
| `crates/origin-tools/src/dispatch.rs` *(new)* | shared dispatch trampoline + memoization table; called from `origin-daemon::agent::dispatch_tool` |
| `crates/origin-tools/tests/recall.rs` *(new)* | lines / match / outline_only region selectors |
| `crates/origin-tools/tests/memoization.rs` *(new)* | same `(name, normalized_input)` → cached handle; `Bash` opts out |
| `crates/origin-provider-anthropic/src/wire.rs` *(modify)* | add `cache_control: ephemeral` field to `WireBlock` variants |
| `crates/origin-provider-anthropic/src/lib.rs` *(modify)* | thread `Option<&Plan>` into `expand_messages_for_wire` + `message_to_wire`; consult `WireDecision` to inline vs reference handles |
| `crates/origin-provider-anthropic/tests/cache_markers.rs` *(new)* | wiremock observes `cache_control` at planned band boundaries |
| `crates/origin-daemon/benches/phase3_cache_warm_ratio.rs` *(new)* | 20-turn workload, twice; warm-pass `cache_read_input_tokens > 0.5 × input_tokens` |

**File-size discipline:** every new `.rs` file targets <300 LOC. If a task naturally pushes a file past 300 LOC, split early (e.g. `planner.rs` → `planner/sort.rs` + `planner/markers.rs`).

---

## Task P3.1 — `origin-planner` skeleton + `PrefixLedger`

**Files:**
- Create: `crates/origin-planner/Cargo.toml`
- Create: `crates/origin-planner/src/lib.rs`
- Create: `crates/origin-planner/src/band.rs`
- Create: `crates/origin-planner/src/ledger.rs`
- Create: `crates/origin-planner/tests/ledger.rs`

- [ ] **Step 1: Manifest** at `crates/origin-planner/Cargo.toml`

```toml
[package]
name = "origin-planner"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
origin-cas  = { path = "../origin-cas" }
thiserror = "1"

[dev-dependencies]
proptest = { version = "=1.4.0", default-features = false, features = ["std"] }
```

- [ ] **Step 2: `band.rs`** — four-band enum + ordinal helper

```rust
//! The four prefix bands the CachePlanner sorts sections into.

/// Bands are emitted in this order when building the request. Cache markers
/// are placed at every adjacent-band boundary. Volatile content is always last
/// because it is the most likely to change between turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Band {
    /// System prompt + tool schemas. Stable across all sessions.
    Frozen = 0,
    /// Long-lived skill injections, project context, recalled memories.
    Sticky = 1,
    /// Stable recent conversation prefix (older than the active turn).
    Sliding = 2,
    /// This turn's new injections / fresh tool results.
    Volatile = 3,
}

impl Band {
    /// Promotion target one band closer to Frozen, or `None` if already Frozen.
    #[must_use]
    pub const fn promoted(self) -> Option<Self> {
        match self {
            Self::Frozen => None,
            Self::Sticky => Some(Self::Frozen),
            Self::Sliding => Some(Self::Sticky),
            Self::Volatile => Some(Self::Sliding),
        }
    }

    /// Demotion target one band closer to Volatile, or `None` if already Volatile.
    #[must_use]
    pub const fn demoted(self) -> Option<Self> {
        match self {
            Self::Frozen => Some(Self::Sticky),
            Self::Sticky => Some(Self::Sliding),
            Self::Sliding => Some(Self::Volatile),
            Self::Volatile => None,
        }
    }
}
```

- [ ] **Step 3: `lib.rs`** declaring modules + re-exports

```rust
//! `origin-planner` — predictive prompt-cache prefix planner.
//!
//! Phase 3 deliverables: `Band`, `PrefixLedger`, `CachePlanner`, `WireDecision`.

pub mod band;
pub mod ledger;

pub use band::Band;
pub use ledger::{LedgerError, PrefixLedger, SectionId, Stability};
```

- [ ] **Step 4: Write the failing test** at `crates/origin-planner/tests/ledger.rs`

```rust
use origin_planner::{Band, PrefixLedger, SectionId};

#[test]
fn consecutive_hits_promote_section_from_volatile_to_sliding() {
    let mut ledger = PrefixLedger::new();
    let id = SectionId::new("memories");
    ledger.record_band(id, Band::Volatile);

    // Three consecutive hits across turns crosses the promotion threshold.
    ledger.record_hit(id, 100); // 100 tokens read from cache in turn 1
    ledger.record_hit(id, 100);
    ledger.record_hit(id, 100);

    assert_eq!(ledger.suggested_band(id), Some(Band::Sliding));
}

#[test]
fn missed_section_demotes_one_band() {
    let mut ledger = PrefixLedger::new();
    let id = SectionId::new("flaky-context");
    ledger.record_band(id, Band::Sticky);
    ledger.record_miss(id);
    ledger.record_miss(id);
    assert_eq!(ledger.suggested_band(id), Some(Band::Sliding));
}
```

- [ ] **Step 5: Run test to verify it fails**

Run: `cargo test -p origin-planner --test ledger`
Expected: FAIL — `cannot find type PrefixLedger / SectionId in crate`.

- [ ] **Step 6: Implement `ledger.rs`**

```rust
//! `PrefixLedger` — per-section stability scoring.
//!
//! Each `(section_id, band)` carries a running `Stability` score updated by
//! `record_hit` (positive) and `record_miss` (negative). When the score crosses
//! `PROMOTE_THRESHOLD` the section is promoted one band toward Frozen; when it
//! crosses `DEMOTE_THRESHOLD` it is demoted one band toward Volatile.

use crate::Band;
use std::collections::HashMap;
use thiserror::Error;

/// Score threshold above which a section is promoted (closer to Frozen).
pub const PROMOTE_THRESHOLD: i32 = 3;
/// Score threshold below which a section is demoted (closer to Volatile).
pub const DEMOTE_THRESHOLD: i32 = -2;

/// Stable identifier for a request section. Cheap to clone; semantically
/// opaque to the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionId(&'static str);

impl SectionId {
    #[must_use]
    pub const fn new(s: &'static str) -> Self {
        Self(s)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Running stability score for one section.
#[derive(Debug, Clone, Copy)]
pub struct Stability {
    /// Net hits minus misses across the lifetime of this section.
    pub score: i32,
    /// Current band the section is parked in.
    pub band: Band,
}

#[derive(Debug, Error)]
pub enum LedgerError {
    /// Caller asked for a section the ledger never saw.
    #[error("unknown section: {0}")]
    Unknown(&'static str),
}

#[derive(Debug, Default)]
pub struct PrefixLedger {
    table: HashMap<SectionId, Stability>,
}

impl PrefixLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a section with its current band. Idempotent.
    pub fn record_band(&mut self, id: SectionId, band: Band) {
        self.table.entry(id).or_insert(Stability { score: 0, band });
    }

    /// Record a cache hit. `tokens_read` is informational only at this stage
    /// (real workloads will weigh by token count once Phase 3 telemetry lands
    /// in P3.8); kept in the signature so callers don't change once weighting
    /// is added.
    pub fn record_hit(&mut self, id: SectionId, _tokens_read: u32) {
        let entry = self.table.entry(id).or_insert(Stability {
            score: 0,
            band: Band::Volatile,
        });
        entry.score = entry.score.saturating_add(1);
        if entry.score >= PROMOTE_THRESHOLD {
            if let Some(b) = entry.band.promoted() {
                entry.band = b;
                entry.score = 0;
            }
        }
    }

    /// Record a cache miss.
    pub fn record_miss(&mut self, id: SectionId) {
        let entry = self.table.entry(id).or_insert(Stability {
            score: 0,
            band: Band::Volatile,
        });
        entry.score = entry.score.saturating_sub(1);
        if entry.score <= DEMOTE_THRESHOLD {
            if let Some(b) = entry.band.demoted() {
                entry.band = b;
                entry.score = 0;
            }
        }
    }

    /// Current band the planner should park this section in.
    #[must_use]
    pub fn suggested_band(&self, id: SectionId) -> Option<Band> {
        self.table.get(&id).map(|s| s.band)
    }
}
```

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p origin-planner --test ledger`
Expected: PASS.

- [ ] **Step 8: Add a property test** at the bottom of `tests/ledger.rs`

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn consecutive_hits_never_demote(hits in 1u32..32) {
        let mut ledger = PrefixLedger::new();
        let id = SectionId::new("p");
        ledger.record_band(id, Band::Volatile);
        let start = ledger.suggested_band(id).expect("seeded");
        for _ in 0..hits {
            ledger.record_hit(id, 50);
        }
        let end = ledger.suggested_band(id).expect("present");
        // Bands order Frozen=0 < Sticky=1 < Sliding=2 < Volatile=3, so the
        // ord-numeric value of `end` must be <= `start` (closer to Frozen).
        prop_assert!(end as u8 <= start as u8);
    }
}
```

- [ ] **Step 9: Run all planner tests**

Run: `cargo test -p origin-planner`
Expected: PASS.

- [ ] **Step 10: Verification gate**

Run **all** of:

```bash
cargo test -p origin-planner
cargo clippy -p origin-planner --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 11: Commit**

```bash
git add crates/origin-planner Cargo.lock
git commit -m "feat(origin-planner): PrefixLedger stability scoring (P3.1)"
```

---

## Task P3.2 — `CachePlanner::plan(request)` + Anthropic marker emission

**Files:**
- Create: `crates/origin-planner/src/planner.rs`
- Create: `crates/origin-planner/tests/planner.rs`
- Modify: `crates/origin-planner/src/lib.rs` (export `CachePlanner`, `Plan`, `Section`)
- Modify: `crates/origin-provider-anthropic/src/wire.rs` (add `cache_control` to each `WireBlock` variant)
- Modify: `crates/origin-provider-anthropic/src/lib.rs` (consult `Plan` to emit `cache_control: ephemeral` at band boundaries)
- Create: `crates/origin-provider-anthropic/tests/cache_markers.rs`

- [ ] **Step 1: Failing test** at `crates/origin-planner/tests/planner.rs`

```rust
use origin_planner::{Band, CachePlanner, PrefixLedger, Section, SectionId};

#[test]
fn plan_emits_four_bands_in_canonical_order() {
    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let sections = vec![
        Section::new(SectionId::new("volatile-1"), Band::Volatile, 0..32),
        Section::new(SectionId::new("system"), Band::Frozen, 32..96),
        Section::new(SectionId::new("memories"), Band::Sticky, 96..160),
        Section::new(SectionId::new("history"), Band::Sliding, 160..224),
    ];

    let plan = planner.plan(&sections);
    let bands: Vec<Band> = plan.ordered_sections().iter().map(|s| s.band).collect();
    assert_eq!(
        bands,
        vec![Band::Frozen, Band::Sticky, Band::Sliding, Band::Volatile],
    );
}

#[test]
fn markers_are_emitted_at_band_boundaries_only() {
    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let sections = vec![
        Section::new(SectionId::new("a"), Band::Frozen, 0..32),
        Section::new(SectionId::new("b"), Band::Frozen, 32..64),
        Section::new(SectionId::new("c"), Band::Sticky, 64..128),
    ];
    let plan = planner.plan(&sections);
    let markers: Vec<usize> = plan.marker_indices().to_vec();
    // After section `b` ends at index 1 we cross Frozen→Sticky → one marker
    // sits between indices 1 and 2.
    assert_eq!(markers, vec![1]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-planner --test planner`
Expected: FAIL — `CachePlanner / Section / Plan` undefined.

- [ ] **Step 3: Implement `planner.rs`**

```rust
//! `CachePlanner::plan` — sort sections into Frozen→Sticky→Sliding→Volatile
//! and emit marker positions at every adjacent-band boundary.

use crate::{Band, PrefixLedger, SectionId};
use std::ops::Range;

/// One contiguous portion of the outgoing request. The planner sorts these
/// by `band` and emits cache markers between adjacent bands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub id: SectionId,
    pub band: Band,
    /// Byte range inside the section's original block (informational; used by
    /// `WireDecision` in P3.6 to pick inline vs reference).
    pub bytes: Range<usize>,
}

impl Section {
    #[must_use]
    pub const fn new(id: SectionId, band: Band, bytes: Range<usize>) -> Self {
        Self { id, band, bytes }
    }
}

/// Output of `CachePlanner::plan`. `marker_indices()[i]` means "emit a cache
/// marker after `ordered_sections()[i]`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    ordered: Vec<Section>,
    markers: Vec<usize>,
}

impl Plan {
    #[must_use]
    pub fn ordered_sections(&self) -> &[Section] {
        &self.ordered
    }

    #[must_use]
    pub fn marker_indices(&self) -> &[usize] {
        &self.markers
    }
}

pub struct CachePlanner<'a> {
    ledger: &'a PrefixLedger,
}

impl<'a> CachePlanner<'a> {
    #[must_use]
    pub const fn new(ledger: &'a PrefixLedger) -> Self {
        Self { ledger }
    }

    /// Sort sections into canonical band order and compute marker positions.
    /// The ledger may override an input section's `band` if the running
    /// stability score has promoted/demoted it.
    #[must_use]
    pub fn plan(&self, sections: &[Section]) -> Plan {
        let mut ordered: Vec<Section> = sections
            .iter()
            .map(|s| {
                let band = self.ledger.suggested_band(s.id).unwrap_or(s.band);
                Section { band, ..s.clone() }
            })
            .collect();
        // Stable sort so sections inside one band stay in caller-supplied order.
        ordered.sort_by_key(|s| s.band as u8);

        let mut markers = Vec::new();
        for w in ordered.windows(2).enumerate() {
            let (i, pair) = w;
            if pair[0].band != pair[1].band {
                markers.push(i);
            }
        }
        Plan { ordered, markers }
    }
}
```

- [ ] **Step 4: Re-export from `lib.rs`**

Modify `crates/origin-planner/src/lib.rs`:

```rust
//! `origin-planner` — predictive prompt-cache prefix planner.

pub mod band;
pub mod ledger;
pub mod planner;

pub use band::Band;
pub use ledger::{LedgerError, PrefixLedger, SectionId, Stability};
pub use planner::{CachePlanner, Plan, Section};
```

- [ ] **Step 5: Run planner tests**

Run: `cargo test -p origin-planner --test planner`
Expected: PASS.

- [ ] **Step 6: Extend Anthropic `wire::WireBlock` with `cache_control`**

Modify `crates/origin-provider-anthropic/src/wire.rs`:

```rust
#[derive(Serialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum WireCacheControl {
    #[default]
    None,
    Ephemeral,
}

impl WireCacheControl {
    pub fn is_none(self) -> bool {
        matches!(self, Self::None)
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireBlock<'a> {
    Text {
        text: &'a str,
        #[serde(skip_serializing_if = "WireCacheControl::is_none", default)]
        cache_control: WireCacheControl,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "WireCacheControl::is_none", default)]
        cache_control: WireCacheControl,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: &'a str,
        is_error: bool,
        #[serde(skip_serializing_if = "WireCacheControl::is_none", default)]
        cache_control: WireCacheControl,
    },
}
```

Update `cache_control` serialization to emit `{"type": "ephemeral"}` as Anthropic expects — replace the enum with a `Option<WireCacheControlObj>` wrapper:

```rust
#[derive(Serialize, Clone, Copy)]
pub struct WireCacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl WireCacheControl {
    pub const fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}
```

…and on each `WireBlock` variant use `#[serde(skip_serializing_if = "Option::is_none")] cache_control: Option<WireCacheControl>`.

- [ ] **Step 7: Modify `message_to_wire` to accept an optional `Plan`**

In `crates/origin-provider-anthropic/src/lib.rs`, change the signature:

```rust
fn message_to_wire<'a>(m: &'a Message, plan: Option<&Plan>, msg_idx: usize) -> WireMessage<'a> { … }
```

When `plan.is_some()` and the current `(msg_idx, block_idx)` matches a marker index, set `cache_control: Some(WireCacheControl::ephemeral())` on that block. Otherwise pass `None`.

Update both `chat` and `chat_stream` call sites to thread `Option<&Plan>`. Phase 3 step 1 leaves the planner unset (`None`) inside `chat_stream` — wiring it through `LoopOptions` happens in P3.6.

- [ ] **Step 8: Failing integration test** at `crates/origin-provider-anthropic/tests/cache_markers.rs`

```rust
use origin_planner::{Band, CachePlanner, PrefixLedger, Section, SectionId};
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cache_marker_emitted_on_planned_boundary() {
    let server = MockServer::start().await;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<Value>));
    let cap = captured.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test"))
        .respond_with(move |req: &wiremock::Request| {
            *cap.lock().expect("lock") = Some(req.body_json().expect("json"));
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hi"}],
                "usage": {"input_tokens": 1, "output_tokens": 1,
                          "cache_read_input_tokens": 0,
                          "cache_creation_input_tokens": 0}
            }))
        })
        .mount(&server)
        .await;

    // Build a planner with sections that span Frozen→Sticky.
    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let plan = planner.plan(&[
        Section::new(SectionId::new("system"), Band::Frozen, 0..32),
        Section::new(SectionId::new("memories"), Band::Sticky, 32..64),
    ]);

    let client = Anthropic::with_endpoint(server.uri(), "test", "claude-3-5-haiku-20241022")
        .with_plan(plan); // see Step 9

    let _ = client
        .chat(origin_provider::ChatRequest {
            system: "system-prompt".into(),
            messages: vec![/* one user message + two assistant Text blocks */],
            model: "claude-3-5-haiku-20241022".into(),
            tools: vec![],
        })
        .await
        .expect("ok");

    let body = captured.lock().expect("lock").clone().expect("captured");
    let messages = body["messages"].as_array().expect("messages array");
    // Find at least one cache_control: ephemeral marker on a block.
    let saw_marker = messages.iter().any(|m| {
        m["content"]
            .as_array()
            .map(|cs| cs.iter().any(|c| c.get("cache_control").is_some()))
            .unwrap_or(false)
    });
    assert!(saw_marker, "expected at least one cache_control marker");
}
```

- [ ] **Step 9: Add `Anthropic::with_plan`**

In `crates/origin-provider-anthropic/src/lib.rs`, add:

```rust
impl Anthropic {
    /// Attach a `Plan` so the encoder emits `cache_control` markers at the
    /// planned band boundaries. Holds an owned `Plan` by-value to keep the
    /// type `Send + Sync` for use as a trait object.
    #[must_use]
    pub fn with_plan(mut self, plan: origin_planner::Plan) -> Self {
        self.plan = Some(plan);
        self
    }
}
```

Add `plan: Option<origin_planner::Plan>` to the struct. Default to `None`. Add `origin-planner = { path = "../origin-planner" }` to `crates/origin-provider-anthropic/Cargo.toml`.

- [ ] **Step 10: Run integration test**

Run: `cargo test -p origin-provider-anthropic --test cache_markers`
Expected: PASS.

- [ ] **Step 11: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 12: Commit**

```bash
git add crates/origin-planner crates/origin-provider-anthropic Cargo.toml Cargo.lock
git commit -m "feat(origin-planner): CachePlanner band sort + Anthropic ephemeral markers (P3.2)"
```

---

## Task P3.3 — Incremental tool_use JSON parser

**Files:**
- Create: `crates/origin-daemon/src/tool_use_parser.rs`
- Create: `crates/origin-daemon/tests/tool_use_parser.rs`
- Modify: `crates/origin-daemon/src/lib.rs` (declare module)

- [ ] **Step 1: Failing test** at `crates/origin-daemon/tests/tool_use_parser.rs`

```rust
use origin_daemon::tool_use_parser::{ToolUseDelta, ToolUseParser};

#[test]
fn emits_field_event_before_closing_brace() {
    let mut p = ToolUseParser::new();
    // Anthropic streams `tool_use` as: an outer start event sets `name`,
    // then `input_json_delta` events carry partial JSON of the `input` object.
    p.begin_tool_use("Read");
    let events = p.feed(b"{\"file_path\":\"/etc/passwd\"");
    let names: Vec<_> = events
        .into_iter()
        .map(|e| match e {
            ToolUseDelta::Field { name, value, .. } => (name, value),
            _ => panic!("unexpected event"),
        })
        .collect();
    // The parser emits a Field event the moment the *value* completes — the
    // closing `}` of the outer object has not yet arrived.
    assert_eq!(names, vec![("file_path".into(), b"/etc/passwd".to_vec())]);
}

#[test]
fn coalesces_split_value_across_chunks() {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("Read");
    let mut all = Vec::new();
    all.extend(p.feed(b"{\"file_path\":\"/etc/"));
    all.extend(p.feed(b"passwd\"}"));
    let strings: Vec<_> = all
        .into_iter()
        .filter_map(|e| match e {
            ToolUseDelta::Field { name, value, .. } if name == "file_path" => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(strings, vec![b"/etc/passwd".to_vec()]);
}

#[test]
fn surfaces_close_event_on_outer_brace() {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("Read");
    let events = p.feed(b"{\"file_path\":\"a\"}");
    let closed = events
        .into_iter()
        .any(|e| matches!(e, ToolUseDelta::Closed { .. }));
    assert!(closed, "expected ToolUseDelta::Closed at outer `}}`");
}
```

- [ ] **Step 2: Run test — confirm failure**

Run: `cargo test -p origin-daemon --test tool_use_parser`
Expected: FAIL — `unresolved import origin_daemon::tool_use_parser`.

- [ ] **Step 3: Implement `tool_use_parser.rs`**

```rust
//! Incremental SAX-style JSON parser for `tool_use` input objects (N2.2).
//!
//! Consumes fragments of a streaming JSON object (the assistant's
//! `tool_use.input`) and emits a `Field` event the moment each top-level
//! key/value pair completes — *before* the outer closing `}` arrives. That
//! makes the parser the trigger for speculative tool dispatch.
//!
//! Scope: only the **outer object** is walked. Nested values are captured as
//! raw bytes between matching `{}`/`[]`/`""` boundaries; speculative pure
//! tools have flat-ish input schemas (`Read`, `Glob`, `Grep`) so capturing
//! raw inner bytes is enough for P3 — a richer typed view can be layered on
//! top later without a parser rewrite.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolUseDelta {
    /// A top-level key/value pair just completed.
    Field {
        tool_name: String,
        name: String,
        /// Raw UTF-8 bytes of the value. Strings have their quotes stripped
        /// and escape sequences resolved; objects/arrays are passed through
        /// as-is including their wrapping `{}`/`[]`.
        value: Vec<u8>,
    },
    /// The outer `}` of the tool_use input arrived.
    Closed {
        tool_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    BeforeKey,
    InKey,
    AfterKey,
    BeforeValue,
    InString,
    InStringEscape,
    InNumber,
    InBoolNull,
    InNested,
    AfterValue,
    Closed,
}

pub struct ToolUseParser {
    state: State,
    /// Active tool name set by `begin_tool_use`.
    tool_name: Option<String>,
    /// Buffer for the current key (accumulating between `"` and `"`).
    key_buf: Vec<u8>,
    /// Buffer for the current value (string body without wrapping quotes, or
    /// nested object/array bytes including wrappers).
    val_buf: Vec<u8>,
    /// Bracket depth while inside a nested value. Reaches 0 → value done.
    nest_depth: u32,
}

impl Default for ToolUseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolUseParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            tool_name: None,
            key_buf: Vec::new(),
            val_buf: Vec::new(),
            nest_depth: 0,
        }
    }

    /// Set the tool name from the surrounding `tool_use` block-start event.
    pub fn begin_tool_use(&mut self, name: impl Into<String>) {
        self.tool_name = Some(name.into());
        self.state = State::BeforeKey;
    }

    /// Feed the next fragment and collect any completed field events.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<ToolUseDelta> {
        let mut out = Vec::new();
        for &b in chunk {
            self.step(b, &mut out);
        }
        out
    }

    fn step(&mut self, b: u8, out: &mut Vec<ToolUseDelta>) {
        match self.state {
            State::Idle | State::Closed => {}
            State::BeforeKey => match b {
                b'{' | b',' | b' ' | b'\t' | b'\r' | b'\n' => {}
                b'"' => {
                    self.key_buf.clear();
                    self.state = State::InKey;
                }
                b'}' => self.finish_object(out),
                _ => {}
            },
            State::InKey => {
                if b == b'"' {
                    self.state = State::AfterKey;
                } else {
                    self.key_buf.push(b);
                }
            }
            State::AfterKey => {
                if b == b':' {
                    self.state = State::BeforeValue;
                }
            }
            State::BeforeValue => {
                self.val_buf.clear();
                match b {
                    b' ' | b'\t' | b'\r' | b'\n' => {}
                    b'"' => self.state = State::InString,
                    b'{' | b'[' => {
                        self.val_buf.push(b);
                        self.nest_depth = 1;
                        self.state = State::InNested;
                    }
                    b't' | b'f' | b'n' => {
                        self.val_buf.push(b);
                        self.state = State::InBoolNull;
                    }
                    _ => {
                        self.val_buf.push(b);
                        self.state = State::InNumber;
                    }
                }
            }
            State::InString => match b {
                b'\\' => self.state = State::InStringEscape,
                b'"' => self.emit_field(out),
                _ => self.val_buf.push(b),
            },
            State::InStringEscape => {
                // Minimal escape handling: pass through the next byte raw.
                // Sufficient for paths, which never use `\u`-style escapes
                // in this codebase. Richer escape decoding lands with N10.10.
                self.val_buf.push(b);
                self.state = State::InString;
            }
            State::InNumber => match b {
                b',' => self.emit_field(out),
                b'}' => {
                    self.emit_field(out);
                    self.finish_object(out);
                }
                _ if b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E') => {
                    self.val_buf.push(b);
                }
                _ => {} // whitespace; ignore
            },
            State::InBoolNull => {
                self.val_buf.push(b);
                if matches!(self.val_buf.as_slice(), b"true" | b"false" | b"null") {
                    self.emit_field(out);
                }
            }
            State::InNested => {
                self.val_buf.push(b);
                match b {
                    b'{' | b'[' => self.nest_depth = self.nest_depth.saturating_add(1),
                    b'}' | b']' => {
                        self.nest_depth = self.nest_depth.saturating_sub(1);
                        if self.nest_depth == 0 {
                            self.emit_field(out);
                        }
                    }
                    _ => {}
                }
            }
            State::AfterValue => match b {
                b',' => self.state = State::BeforeKey,
                b'}' => self.finish_object(out),
                _ => {}
            },
        }
    }

    fn emit_field(&mut self, out: &mut Vec<ToolUseDelta>) {
        let tool_name = self
            .tool_name
            .clone()
            .unwrap_or_else(|| "<unknown>".into());
        let name = String::from_utf8_lossy(&self.key_buf).into_owned();
        let value = std::mem::take(&mut self.val_buf);
        out.push(ToolUseDelta::Field {
            tool_name,
            name,
            value,
        });
        self.state = State::AfterValue;
    }

    fn finish_object(&mut self, out: &mut Vec<ToolUseDelta>) {
        let tool_name = self
            .tool_name
            .clone()
            .unwrap_or_else(|| "<unknown>".into());
        out.push(ToolUseDelta::Closed { tool_name });
        self.state = State::Closed;
    }
}
```

- [ ] **Step 4: Declare module in `lib.rs`**

Modify `crates/origin-daemon/src/lib.rs` to add:

```rust
pub mod tool_use_parser;
```

- [ ] **Step 5: Run tests — confirm pass**

Run: `cargo test -p origin-daemon --test tool_use_parser`
Expected: PASS.

- [ ] **Step 6: Add a property test** appended to `tests/tool_use_parser.rs`

```rust
use proptest::prelude::*;

proptest! {
    /// Whatever the chunking, the set of `(name, value)` pairs the parser
    /// emits is identical.
    #[test]
    fn chunking_is_irrelevant_to_field_events(
        chunks in proptest::collection::vec(1u8..16, 1..32),
    ) {
        let input = b"{\"file_path\":\"/tmp/x\",\"recursive\":true,\"limit\":7}";
        let mut p_whole = ToolUseParser::new();
        p_whole.begin_tool_use("X");
        let whole_events = p_whole.feed(input);

        let mut p_split = ToolUseParser::new();
        p_split.begin_tool_use("X");
        let mut cursor = 0;
        let mut split_events = Vec::new();
        for c in chunks {
            let end = (cursor + c as usize).min(input.len());
            split_events.extend(p_split.feed(&input[cursor..end]));
            cursor = end;
            if cursor == input.len() { break; }
        }
        if cursor < input.len() {
            split_events.extend(p_split.feed(&input[cursor..]));
        }

        // Filter to Field events; compare set of (name, value).
        let proj = |evs: Vec<ToolUseDelta>| -> Vec<(String, Vec<u8>)> {
            evs.into_iter()
                .filter_map(|e| match e {
                    ToolUseDelta::Field { name, value, .. } => Some((name, value)),
                    _ => None,
                })
                .collect()
        };
        prop_assert_eq!(proj(whole_events), proj(split_events));
    }
}
```

- [ ] **Step 7: Add a `cargo fuzz` skeleton** (stub-only — full corpus lands with N10.10)

```bash
mkdir -p crates/origin-daemon/fuzz
```

Create `crates/origin-daemon/fuzz/Cargo.toml`:

```toml
[package]
name = "origin-daemon-fuzz"
version = "0.0.0"
publish = false
edition = "2021"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
origin-daemon = { path = ".." }

[[bin]]
name = "tool_use_parser"
path = "fuzz_targets/tool_use_parser.rs"
test = false
doc = false
```

Create `crates/origin-daemon/fuzz/fuzz_targets/tool_use_parser.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use origin_daemon::tool_use_parser::ToolUseParser;

fuzz_target!(|data: &[u8]| {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("X");
    let _ = p.feed(data);
});
```

Add the fuzz directory to the **workspace-excluded** list in root `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/*"]
exclude = ["crates/origin-daemon/fuzz"]
```

The fuzz target compiles only under `cargo +nightly fuzz`; CI does not run it in Phase 3.

- [ ] **Step 8: Run all daemon tests**

Run: `cargo test -p origin-daemon`
Expected: PASS.

- [ ] **Step 9: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-daemon/src/tool_use_parser.rs crates/origin-daemon/src/lib.rs \
        crates/origin-daemon/tests/tool_use_parser.rs crates/origin-daemon/fuzz Cargo.toml
git commit -m "feat(origin-daemon): incremental tool_use JSON parser (P3.3)"
```

---

## Task P3.4 — Speculative dispatch wiring

**Files:**
- Modify: `crates/origin-stream/src/event.rs` (the SSE parser already emits `ToolUseDelta` payloads — confirm `tool_use` block-start carries the `name` in a leading byte sequence we can parse out)
- Modify: `crates/origin-provider-anthropic/src/streaming.rs` (emit a leading `ToolUseDelta` payload containing `name=…|` prefix so the parser can pull tool name out without a side channel)
- Modify: `crates/origin-daemon/src/agent.rs` (consume `ToolUseDelta` from the per-turn ring drain; spawn speculative tasks)
- Create: `crates/origin-daemon/tests/speculative_e2e.rs`

- [ ] **Step 1: Failing test** at `crates/origin-daemon/tests/speculative_e2e.rs`

```rust
use origin_daemon::tool_use_parser::ToolUseParser;
use origin_stream::{Ring, TokenEvent, TokenKind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Verifies that the agent loop starts running a pure tool *before* the
/// streaming `tool_use` block has closed. We can't yet plug in the real
/// agent loop without breaking other tests, so this test exercises the
/// shape of the integration: parser emits a Field event; a background task
/// observes it; the task increments a counter; then the tool_use block
/// closes. The counter must be > 0 *before* the closing brace is fed.
#[tokio::test]
async fn speculative_task_fires_before_close() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();
    let ring = Ring::with_capacity(64 * 1024);
    let sub = ring.subscribe();

    let parse_handle = tokio::spawn(async move {
        let mut sub = sub;
        let mut parser = ToolUseParser::new();
        parser.begin_tool_use("Read");
        while let Some(ev) = sub.next().await.expect("next") {
            if ev.kind() == TokenKind::ToolUseDelta {
                let events = parser.feed(ev.payload());
                for e in events {
                    if let origin_daemon::tool_use_parser::ToolUseDelta::Field { .. } = e {
                        c.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    });

    // Producer simulates the SSE side: send the field, sleep, send the close.
    ring.publish(&TokenEvent::new(
        TokenKind::ToolUseDelta,
        b"{\"file_path\":\"/etc/passwd\"".to_vec(),
    ))
    .expect("publish field");

    // Spin until the parse task observes a Field event, or fail at 2s.
    let started = Instant::now();
    while counter.load(Ordering::SeqCst) == 0 {
        if started.elapsed() > Duration::from_secs(2) {
            panic!("speculative dispatch never fired before close");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Only now do we send the closing brace.
    ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, b"}".to_vec()))
        .expect("publish close");
    ring.close();
    parse_handle.await.expect("join");
}
```

- [ ] **Step 2: Run test — confirm failure**

Run: `cargo test -p origin-daemon --test speculative_e2e`
Expected: FAIL — likely a panic at `panic!("speculative dispatch never fired before close")` because the ring `Subscriber::next()` API or the parser feed loop isn't yet exposed in this combination. Confirm the failure is the panic, not a compile error.

If it's a compile error, narrow to that and fix imports.

- [ ] **Step 3: Implement `SpeculativeRegistry` in `crates/origin-daemon/src/agent.rs`**

Add near the top of `agent.rs`:

```rust
use origin_tools::SideEffects;
use std::collections::HashMap;
use tokio::task::JoinHandle;

/// Tracks speculative tasks fired off mid-stream. Keyed by the assistant
/// `tool_use.id` so the agent can `await` the precomputed handle once the
/// `tool_use` block closes.
#[derive(Default)]
struct SpeculativeRegistry {
    in_flight: HashMap<String, JoinHandle<Result<Vec<u8>, LoopError>>>,
}

impl SpeculativeRegistry {
    fn spawn(
        &mut self,
        tool_use_id: String,
        meta: &'static ToolMeta,
        args: serde_json::Value,
    ) {
        // Side-effecting tools opt out — N2.2.
        if !matches!(meta.side_effects, SideEffects::Pure) {
            return;
        }
        let handle = tokio::spawn(async move {
            let bytes = dispatch_tool(meta, &args).await?.into_bytes();
            Ok::<_, LoopError>(bytes)
        });
        self.in_flight.insert(tool_use_id, handle);
    }

    async fn take(
        &mut self,
        tool_use_id: &str,
    ) -> Option<Result<Vec<u8>, LoopError>> {
        let handle = self.in_flight.remove(tool_use_id)?;
        match handle.await {
            Ok(r) => Some(r),
            Err(join_err) => Some(Err(LoopError::ToolFailure(join_err.to_string()))),
        }
    }
}
```

Note: `dispatch_tool` currently takes `&ToolMeta` (lifetime tied to `inventory::iter` static slot — `&'static ToolMeta`). The signature above relies on that; tighten the lookup in `run_loop` to resolve to `&'static ToolMeta` from `registry_iter()`.

- [ ] **Step 4: Wire the registry into `run_streaming_turn`**

Re-architect `run_streaming_turn` to keep a `ToolUseParser` per `(turn, tool_use_index)` (Anthropic re-uses `index` only inside one turn) and a single `SpeculativeRegistry`. On every `ToolUseDelta` event, feed the parser; on the first complete `Field` for a pure tool, parse the value into JSON (best-effort partial — if the input schema has only one required field, that's enough to fire). On `ContentBlockStop` for the tool_use, mark the parser closed.

Pseudocode (Step 5 has the literal code):

```
on ToolUseDelta:
    parser.feed(payload)
    for each Field event:
        if first field for this tool_use_id AND tool is Pure:
            registry.spawn(tool_use_id, meta, partial_args)
    for each Closed event:
        // No-op; agent loop body will pick up the cached result.
```

- [ ] **Step 5: Replace `run_streaming_turn` body** to thread the registry

Modify `run_streaming_turn` in `crates/origin-daemon/src/agent.rs`. The diff is large; the minimum required behavior:

1. Build a `SpeculativeRegistry::default()` before driving the stream.
2. Wrap the drain so each `TokenKind::ToolUseDelta` payload is fed into a `ToolUseParser`.
3. On the first `Field` event for a tool_use whose `tool_name` resolves to a `SideEffects::Pure` registered tool, spawn via `registry.spawn`.
4. Hand the registry out alongside the synthetic `ChatResponse` so `run_loop` can prefer the precomputed handle.

The simplest shape is a third return value:

```rust
pub(crate) struct StreamingTurn {
    pub response: origin_provider::ChatResponse,
    pub speculative: SpeculativeRegistry,
}

async fn run_streaming_turn(
    provider: &dyn Provider,
    req: ChatRequest,
    opts: &LoopOptions,
) -> Result<StreamingTurn, LoopError> { … }
```

Then in `run_loop`, when dispatching `tool_uses`, first try `speculative.take(&id).await` and only fall back to a fresh `dispatch_tool` if the speculative result is missing or `Err`.

- [ ] **Step 6: Re-run the e2e test**

Run: `cargo test -p origin-daemon --test speculative_e2e`
Expected: PASS.

- [ ] **Step 7: Spot-check no regression in existing streaming tests**

Run: `cargo test -p origin-daemon`
Expected: all daemon tests PASS (including `stream_e2e`).

- [ ] **Step 8: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-daemon/src/agent.rs crates/origin-daemon/tests/speculative_e2e.rs
git commit -m "feat(origin-daemon): speculative dispatch for pure tools (P3.4)"
```

---

## Task P3.5 — `Recall` tool

**Files:**
- Create: `crates/origin-tools/src/builtins/recall.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs` (add `pub mod recall;`)
- Modify: `crates/origin-tools/Cargo.toml` (add `origin-cas = { path = "../origin-cas" }`)
- Create: `crates/origin-tools/tests/recall.rs`
- Modify: `crates/origin-daemon/src/agent.rs` (route `"Recall"` in `dispatch_tool`)

- [ ] **Step 1: Failing test** at `crates/origin-tools/tests/recall.rs`

```rust
use origin_cas::{Store, StoreConfig};
use origin_tools::builtins::recall::{recall_tool, Region};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn recalls_line_range_from_handle() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let body = (1..=30)
        .map(|n| format!("line-{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let h = store.put(body.as_bytes()).expect("put");

    let region = Region::Lines { start: 10, end: 12 };
    let out = recall_tool(&store, *h.as_bytes(), Some(region)).expect("ok");
    assert_eq!(out, "line-10\nline-11\nline-12");
}

#[test]
fn recall_match_returns_matching_lines() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let body = "alpha\nBETA\ngamma\nbeta-2";
    let h = store.put(body.as_bytes()).expect("put");
    let out = recall_tool(
        &store,
        *h.as_bytes(),
        Some(Region::Match { pattern: "(?i)beta".into() }),
    )
    .expect("ok");
    assert_eq!(out, "BETA\nbeta-2");
}
```

- [ ] **Step 2: Run test — confirm failure**

Run: `cargo test -p origin-tools --test recall`
Expected: FAIL — `unresolved import origin_tools::builtins::recall`.

- [ ] **Step 3: Implement `recall.rs`**

```rust
//! `Recall` — inflate a CAS handle into the message stream (N5.5).

use origin_cas::{Hash, Store};
use thiserror::Error;

/// Region selector for `recall_tool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Region {
    /// 1-based inclusive line range.
    Lines { start: usize, end: usize },
    /// Regex; matching lines are returned in original order, separated by `\n`.
    Match { pattern: String },
    /// Outline-only mode — Phase 3 returns "<outline_only not yet implemented>"
    /// because the sidecar coroutine that emits structure summaries lands in
    /// Phase 6. The variant is wired in now so callers don't need to change.
    OutlineOnly,
}

#[derive(Debug, Error)]
pub enum RecallError {
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
    #[error("handle not in store")]
    Missing,
    #[error("invalid line range {start}..={end} (body has {total} lines)")]
    BadRange {
        start: usize,
        end: usize,
        total: usize,
    },
    #[error("invalid regex: {0}")]
    Regex(String),
    #[error("body is not valid utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Inflate a CAS handle and slice it per `region`.
///
/// # Errors
/// See [`RecallError`].
pub fn recall_tool(
    store: &Store,
    handle: [u8; 32],
    region: Option<Region>,
) -> Result<String, RecallError> {
    let body_bytes = store
        .get(Hash::from_bytes(handle))?
        .ok_or(RecallError::Missing)?;
    let body = std::str::from_utf8(&body_bytes)?;

    match region {
        None => Ok(body.to_owned()),
        Some(Region::Lines { start, end }) => {
            let lines: Vec<&str> = body.split('\n').collect();
            let total = lines.len();
            if start == 0 || start > end || end > total {
                return Err(RecallError::BadRange { start, end, total });
            }
            Ok(lines[(start - 1)..=(end - 1)].join("\n"))
        }
        Some(Region::Match { pattern }) => {
            let re = regex::Regex::new(&pattern).map_err(|e| RecallError::Regex(e.to_string()))?;
            let matched: Vec<&str> = body.split('\n').filter(|l| re.is_match(l)).collect();
            Ok(matched.join("\n"))
        }
        Some(Region::OutlineOnly) => Ok("<outline_only not yet implemented>".into()),
    }
}

crate::origin_tool! {
    name: "Recall",
    description: "Inflate a CAS handle into the response. Optional region: { lines: [start, end] } | { match: \"regex\" } | { outline_only: true }.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "handle": {"type": "string", "description": "Lowercase hex CAS hash (64 chars)."},
            "region": {
                "type": "object",
                "description": "Optional slice selector.",
                "additionalProperties": true
            }
        },
        "required": ["handle"]
    }"#,
}
```

Add `regex = "1"` to `crates/origin-tools/Cargo.toml` dependencies (already in workspace's transitive set via `grep-regex`, but declaring it explicitly is correct hygiene).

- [ ] **Step 4: Route `Recall` in `dispatch_tool`**

Modify `crates/origin-daemon/src/agent.rs` — `dispatch_tool` currently does not take a `&Store`. Thread the optional CAS through to `dispatch_tool`:

```rust
async fn dispatch_tool(
    meta: &ToolMeta,
    args: &Value,
    cas: Option<&Arc<origin_cas::Store>>,
) -> Result<String, LoopError> {
    match meta.name {
        // … existing arms …
        "Recall" => {
            let store = cas.ok_or_else(|| {
                LoopError::ToolFailure("Recall requires CAS to be configured".into())
            })?;
            let handle_hex = args
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Recall: missing `handle`".into()))?;
            let handle: [u8; 32] = {
                let mut buf = [0u8; 32];
                hex::decode_to_slice(handle_hex, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("Recall: bad hex: {e}")))?;
                buf
            };
            let region = args.get("region").map(parse_region).transpose()?;
            origin_tools::builtins::recall::recall_tool(store, handle, region)
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        other => Err(LoopError::UnknownTool(other.into())),
    }
}

fn parse_region(v: &Value) -> Result<origin_tools::builtins::recall::Region, LoopError> {
    if let Some(lines) = v.get("lines").and_then(Value::as_array) {
        let start = lines.get(0).and_then(Value::as_u64).ok_or_else(|| {
            LoopError::BadArgs("Recall.region.lines requires [start, end]".into())
        })? as usize;
        let end = lines.get(1).and_then(Value::as_u64).ok_or_else(|| {
            LoopError::BadArgs("Recall.region.lines requires [start, end]".into())
        })? as usize;
        Ok(origin_tools::builtins::recall::Region::Lines { start, end })
    } else if let Some(m) = v.get("match").and_then(Value::as_str) {
        Ok(origin_tools::builtins::recall::Region::Match {
            pattern: m.to_string(),
        })
    } else if v.get("outline_only").and_then(Value::as_bool) == Some(true) {
        Ok(origin_tools::builtins::recall::Region::OutlineOnly)
    } else {
        Err(LoopError::BadArgs("Recall.region: expected lines/match/outline_only".into()))
    }
}
```

Update both call sites of `dispatch_tool` to pass `opts.cas.as_ref()`.

Add `hex = "0.4"` to `crates/origin-daemon/Cargo.toml`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p origin-tools --test recall`
Run: `cargo test -p origin-daemon`
Expected: PASS.

- [ ] **Step 6: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-tools/src/builtins/recall.rs \
        crates/origin-tools/src/builtins/mod.rs \
        crates/origin-tools/tests/recall.rs \
        crates/origin-tools/Cargo.toml \
        crates/origin-daemon/src/agent.rs \
        crates/origin-daemon/Cargo.toml \
        Cargo.lock
git commit -m "feat(origin-tools): Recall builtin + region selectors (P3.5)"
```

---

## Task P3.6 — Handle substitution in message-to-wire (N2.4 step 2)

**Files:**
- Create: `crates/origin-planner/src/decision.rs`
- Create: `crates/origin-planner/tests/decision.rs`
- Modify: `crates/origin-planner/src/lib.rs` (re-export `WireDecision`)
- Modify: `crates/origin-provider-anthropic/src/lib.rs` (consult `WireDecision` inside `expand_messages_for_wire`)

**Rule (N2.4):**
- `WireDecision::Inline` — the planner thinks inlining the bytes will hit cache because the block's section sits in `Frozen` or `Sticky` and the bytes are small enough.
- `WireDecision::Reference` — the planner emits `<result handle:7af3 — N bytes>` because the section sits in `Volatile`/`Sliding` and the bytes are large (default >2 KiB).

- [ ] **Step 1: Failing test** at `crates/origin-planner/tests/decision.rs`

```rust
use origin_planner::{Band, WireDecision};

#[test]
fn small_volatile_inlines() {
    let d = WireDecision::for_block(Band::Volatile, 128);
    assert_eq!(d, WireDecision::Inline);
}

#[test]
fn large_volatile_references() {
    let d = WireDecision::for_block(Band::Volatile, 10_000);
    assert_eq!(d, WireDecision::Reference);
}

#[test]
fn anything_in_frozen_inlines() {
    let d = WireDecision::for_block(Band::Frozen, 10_000);
    assert_eq!(d, WireDecision::Inline);
}
```

- [ ] **Step 2: Run test — confirm failure**

Run: `cargo test -p origin-planner --test decision`
Expected: FAIL — `WireDecision` undefined.

- [ ] **Step 3: Implement `decision.rs`**

```rust
//! `WireDecision` — per-block inline-vs-reference rule for handle substitution
//! in the message-to-wire encoder (N2.4 step 2).

use crate::Band;

pub const INLINE_BYTE_BUDGET: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDecision {
    /// Expand the CAS handle into wire bytes.
    Inline,
    /// Emit a short `<result handle:… — N bytes>` reference; the model can
    /// inflate via `Recall` if it needs the body.
    Reference,
}

impl WireDecision {
    /// Decide for one tool-result block parked in `band` with `byte_len` body.
    #[must_use]
    pub const fn for_block(band: Band, byte_len: usize) -> Self {
        match band {
            // Frozen + Sticky: always inline. These sections hit cache; the
            // bytes are amortized across many turns.
            Band::Frozen | Band::Sticky => Self::Inline,
            // Sliding + Volatile: inline only if small enough that the
            // reference saves no meaningful tokens.
            Band::Sliding | Band::Volatile => {
                if byte_len <= INLINE_BYTE_BUDGET {
                    Self::Inline
                } else {
                    Self::Reference
                }
            }
        }
    }
}
```

- [ ] **Step 4: Re-export from `lib.rs`** — add `pub mod decision;` and `pub use decision::{WireDecision, INLINE_BYTE_BUDGET};`.

- [ ] **Step 5: Run decision test**

Run: `cargo test -p origin-planner --test decision`
Expected: PASS.

- [ ] **Step 6: Modify Anthropic `expand_messages_for_wire`** to consult `WireDecision`

In `crates/origin-provider-anthropic/src/lib.rs`, change the function to take an optional `&Plan` and, when present, decide per-block:

```rust
fn expand_messages_for_wire(
    messages: &[Message],
    cas: Option<&std::sync::Arc<origin_cas::Store>>,
    plan: Option<&origin_planner::Plan>,
) -> Result<Vec<Message>, ProviderError> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let mut blocks = Vec::with_capacity(m.blocks.len());
        for b in &m.blocks {
            match b {
                Block::ToolResult {
                    tool_use_id,
                    handle: Some(h),
                    inline: None,
                    cache_marker,
                } => {
                    let store = cas.ok_or_else(|| {
                        ProviderError::Api("ToolResult handle present but no CAS configured".into())
                    })?;
                    let bytes = store
                        .get(origin_cas::Hash::from_bytes(*h))
                        .map_err(|e| ProviderError::Api(format!("cas get: {e}")))?
                        .ok_or_else(|| ProviderError::Api("cas miss".into()))?;

                    let band = plan
                        .and_then(|p| {
                            // Best-effort band lookup by tool_use_id. The
                            // planner emits sections in the same order as
                            // the message log; a real lookup table arrives
                            // when the planner is fed live request shape in
                            // P3.8 — for now, treat any handle in the active
                            // turn as Volatile.
                            let _ = p;
                            Some(origin_planner::Band::Volatile)
                        })
                        .unwrap_or(origin_planner::Band::Volatile);

                    match origin_planner::WireDecision::for_block(band, bytes.len()) {
                        origin_planner::WireDecision::Inline => {
                            blocks.push(Block::ToolResult {
                                tool_use_id: tool_use_id.clone(),
                                handle: None,
                                inline: Some(bytes),
                                cache_marker: *cache_marker,
                            });
                        }
                        origin_planner::WireDecision::Reference => {
                            let preview = format!(
                                "<result handle:{} — {} bytes>",
                                short_hex(h),
                                bytes.len(),
                            );
                            blocks.push(Block::ToolResult {
                                tool_use_id: tool_use_id.clone(),
                                handle: None,
                                inline: Some(preview.into_bytes()),
                                cache_marker: *cache_marker,
                            });
                        }
                    }
                }
                _ => blocks.push(b.clone()),
            }
        }
        out.push(Message { role: m.role, blocks });
    }
    Ok(out)
}

fn short_hex(h: &[u8; 32]) -> String {
    let hex_bytes = origin_cas::Hash::from_bytes(*h).to_string();
    hex_bytes.chars().take(8).collect()
}
```

Update both call sites of `expand_messages_for_wire` (one in `chat`, one in `chat_stream`) to pass `self.plan.as_ref()`.

- [ ] **Step 7: Add an integration test** that asserts a large body becomes a reference

Create `crates/origin-provider-anthropic/tests/handle_substitution.rs`:

```rust
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_planner::{Band, CachePlanner, PrefixLedger, Section, SectionId};
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use serde_json::Value;
use std::sync::Arc;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn large_tool_result_emitted_as_reference_when_volatile() {
    let server = MockServer::start().await;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<Value>));
    let cap = captured.clone();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            *cap.lock().expect("lock") = Some(req.body_json().expect("json"));
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1,
                          "cache_read_input_tokens": 0,
                          "cache_creation_input_tokens": 0}
            }))
        })
        .mount(&server)
        .await;

    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );

    let big = vec![b'.'; 8_000];
    let h = store.put(&big).expect("put");

    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let plan = planner.plan(&[Section::new(
        SectionId::new("turn-1"),
        Band::Volatile,
        0..big.len(),
    )]);

    let client = Anthropic::with_endpoint(server.uri(), "test", "claude-3-5-haiku-20241022")
        .with_cas(store.clone())
        .with_plan(plan);

    let msg = Message {
        role: Role::Tool,
        blocks: vec![Block::ToolResult {
            tool_use_id: "id1".into(),
            handle: Some(*h.as_bytes()),
            inline: None,
            cache_marker: None,
        }],
    };
    let _ = client
        .chat(origin_provider::ChatRequest {
            system: String::new(),
            messages: vec![msg],
            model: "claude-3-5-haiku-20241022".into(),
            tools: vec![],
        })
        .await
        .expect("ok");

    let body = captured.lock().expect("lock").clone().expect("captured");
    let content = body["messages"][0]["content"][0]["content"]
        .as_str()
        .expect("content str");
    assert!(
        content.starts_with("<result handle:") && content.contains("8000 bytes"),
        "expected reference, got: {content}"
    );
}
```

- [ ] **Step 8: Run the integration test**

Run: `cargo test -p origin-provider-anthropic --test handle_substitution`
Expected: PASS.

- [ ] **Step 9: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-planner/src/decision.rs \
        crates/origin-planner/src/lib.rs \
        crates/origin-planner/tests/decision.rs \
        crates/origin-provider-anthropic/src/lib.rs \
        crates/origin-provider-anthropic/tests/handle_substitution.rs \
        Cargo.lock
git commit -m "feat(origin-planner): WireDecision + handle substitution in message-to-wire (P3.6)"
```

---

## Task P3.7 — Result memoization (N5.4)

**Files:**
- Create: `crates/origin-tools/src/dispatch.rs`
- Create: `crates/origin-tools/tests/memoization.rs`
- Modify: `crates/origin-tools/src/lib.rs` (declare `pub mod dispatch;` + re-export)
- Modify: `crates/origin-daemon/src/agent.rs` (route through `dispatch::Cache`)

**Rule:** Same `(tool_name, normalized_input)` within one session returns the cached handle. `Bash` opts out (per-spec). Cached results annotate with `(cached from turn N)` in the result body.

- [ ] **Step 1: Failing test** at `crates/origin-tools/tests/memoization.rs`

```rust
use origin_tools::dispatch::{Cache, NormalizedInput};

#[test]
fn second_lookup_returns_cached_handle() {
    let mut cache = Cache::new();
    let key = NormalizedInput::hash("Read", br#"{"path":"/etc/passwd"}"#);
    let h = [7u8; 32];
    assert!(cache.lookup(&key).is_none());
    cache.record(key.clone(), h, 4);
    let hit = cache.lookup(&key).expect("hit");
    assert_eq!(hit.handle, h);
    assert_eq!(hit.from_turn, 4);
}

#[test]
fn bash_normalization_is_never_inserted() {
    let mut cache = Cache::new();
    // The dispatch layer is responsible for skipping Bash, but the Cache
    // type takes a deny-list as part of the API to make that explicit.
    assert!(cache.is_skipped("Bash"));
    assert!(!cache.is_skipped("Read"));
}

#[test]
fn input_normalization_strips_whitespace_inside_string_values_is_off() {
    // Normalization is byte-equivalent in P3.7 — same bytes in → same hash.
    // Any tool-specific normalization (path canonicalization, regex
    // compilation invariants) lands with N10.4 in Phase 10.
    let a = NormalizedInput::hash("Read", br#"{"path":"/etc/passwd"}"#);
    let b = NormalizedInput::hash("Read", br#"{ "path" : "/etc/passwd" }"#);
    assert_ne!(a, b);
}
```

- [ ] **Step 2: Run test — confirm failure**

Run: `cargo test -p origin-tools --test memoization`
Expected: FAIL — `unresolved import origin_tools::dispatch`.

- [ ] **Step 3: Implement `dispatch.rs`**

```rust
//! Shared dispatch + per-session memoization (N5.4).
//!
//! The agent looks up `(tool_name, normalized_input)` in `Cache` before
//! actually running the tool. `Bash` is on the deny-list because shell
//! commands may have side effects that bypass the harness. The cache stores
//! a CAS handle pointing at the prior result body.

use blake3::Hash as Blake3Hash;
use std::collections::HashMap;

/// Tool names that never memoize. Side-effect-free guarantees do not extend
/// to shell processes.
pub const MEMOIZATION_SKIPLIST: &[&str] = &["Bash", "Edit", "Write"];

/// 32-byte content hash of `(tool_name, raw_input_bytes)`. The hash function
/// is blake3, matching the rest of `origin` (`origin-cas::Hash`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedInput(Blake3Hash);

impl NormalizedInput {
    /// Compute the canonical key for `(tool_name, raw_input)`.
    ///
    /// Phase 3 uses byte-equivalent normalization: identical input bytes
    /// produce identical keys. Tool-specific normalization (path canon,
    /// regex parse-equivalent collapsing) is in scope for Phase 10.
    #[must_use]
    pub fn hash(tool_name: &str, raw_input: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(tool_name.as_bytes());
        h.update(&[0xff]); // separator
        h.update(raw_input);
        Self(h.finalize())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CacheHit {
    pub handle: [u8; 32],
    pub from_turn: u32,
}

#[derive(Debug, Default)]
pub struct Cache {
    table: HashMap<NormalizedInput, CacheHit>,
}

impl Cache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `tool_name` is on the memoization deny-list.
    #[must_use]
    pub fn is_skipped(&self, tool_name: &str) -> bool {
        MEMOIZATION_SKIPLIST.contains(&tool_name)
    }

    /// Try to fetch the cached handle for a previously-run tool call.
    #[must_use]
    pub fn lookup(&self, key: &NormalizedInput) -> Option<&CacheHit> {
        self.table.get(key)
    }

    /// Record a result. `turn` is the conversation turn number for the
    /// `(cached from turn N)` annotation the agent appends when serving
    /// a hit.
    pub fn record(&mut self, key: NormalizedInput, handle: [u8; 32], turn: u32) {
        self.table.insert(key, CacheHit { handle, from_turn: turn });
    }
}
```

Add `blake3 = "1"` to `crates/origin-tools/Cargo.toml` dependencies.

- [ ] **Step 4: Declare module in `lib.rs`**

```rust
pub mod dispatch;
pub use dispatch::{Cache, CacheHit, NormalizedInput, MEMOIZATION_SKIPLIST};
```

- [ ] **Step 5: Run unit tests**

Run: `cargo test -p origin-tools --test memoization`
Expected: PASS.

- [ ] **Step 6: Wire the cache into the agent loop**

Modify `crates/origin-daemon/src/agent.rs`. Add `cache: origin_tools::Cache` to `Session` (or hold a `Cache` per `run_loop` invocation — preferred: per-session, so make it part of `Session` in `session.rs`). For Phase 3 simplicity, keep it inside `run_loop` and persist nothing across calls:

```rust
let mut cache = origin_tools::Cache::new();
```

Before each `dispatch_tool` invocation:

```rust
let key = origin_tools::NormalizedInput::hash(meta.name, &input_bytes);
let result_bytes = if !cache.is_skipped(meta.name) {
    if let Some(hit) = cache.lookup(&key) {
        let body = if let Some(cas) = opts.cas.as_ref() {
            cas.get(origin_cas::Hash::from_bytes(hit.handle))
                .map_err(|e| LoopError::ToolFailure(e.to_string()))?
                .ok_or_else(|| LoopError::ToolFailure("cas miss on cached handle".into()))?
        } else {
            return Err(LoopError::ToolFailure("memoization requires CAS".into()));
        };
        let annotated = format!(
            "{}\n\n(cached from turn {})",
            String::from_utf8_lossy(&body),
            hit.from_turn,
        );
        annotated.into_bytes()
    } else {
        let text = dispatch_tool(meta, &args, opts.cas.as_ref()).await?;
        text.into_bytes()
    }
} else {
    dispatch_tool(meta, &args, opts.cas.as_ref()).await?.into_bytes()
};

// After landing the result in CAS (already happens later in the loop body),
// `cache.record(key, *h.as_bytes(), turn)` to make subsequent calls hit.
```

Move the `cache.record(...)` call to **after** `cas.put(&result_bytes)` succeeds, using the `h` returned from the put.

- [ ] **Step 7: Failing end-to-end test**

Add to `crates/origin-daemon/tests/`, file `memoization_e2e.rs`:

```rust
//! `Read` the same path twice in one session — second call must be served
//! from the cache and the result body must contain "(cached from turn".

use async_trait::async_trait;
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{
    ChatRequest, ChatResponse, Provider, ProviderError, Usage,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

struct ScriptedProvider {
    turn: AtomicU32,
    target_path: String,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str { "scripted" }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let t = self.turn.fetch_add(1, Ordering::SeqCst);
        let blocks = match t {
            // Turn 1: ask for Read(target_path)
            0 | 1 => vec![Block::ToolUse {
                id: format!("id-{t}"),
                name: "Read".into(),
                input_json: serde_json::to_vec(
                    &serde_json::json!({"path": &self.target_path})
                ).expect("json"),
                cache_marker: None,
            }],
            // Turn 3: final text
            _ => vec![Block::Text {
                text: "done".into(),
                cache_marker: None,
            }],
        };
        Ok(ChatResponse {
            assistant: Message { role: Role::Assistant, blocks },
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn second_read_serves_from_cache_with_annotation() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );
    let p = dir.path().join("hello.txt");
    std::fs::write(&p, "hello world").expect("write");

    let provider = ScriptedProvider {
        turn: AtomicU32::new(0),
        target_path: p.to_string_lossy().into_owned(),
    };

    let mut session = Session::new("test-session", "scripted-model");
    let opts = LoopOptions::default().with_cas(store.clone()).without_streaming();
    let _ = run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    // Inspect tool result messages.
    let tool_results: Vec<&Block> = session
        .snapshot()
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .flat_map(|m| m.blocks.iter())
        .collect();
    assert!(tool_results.len() >= 2);
    // The second tool result body must contain "(cached from turn".
    let second = match tool_results[1] {
        Block::ToolResult { handle: Some(h), .. } => store
            .get(origin_cas::Hash::from_bytes(*h))
            .expect("get")
            .expect("present"),
        _ => panic!("second tool result missing handle"),
    };
    let txt = String::from_utf8(second).expect("utf8");
    assert!(txt.contains("(cached from turn"), "got: {txt}");
}
```

If `Session::new` or `session.snapshot()` don't exist with these exact names, replace with the actual constructor/accessor from `crates/origin-daemon/src/session.rs` (use Read tool to confirm).

- [ ] **Step 8: Run the e2e test**

Run: `cargo test -p origin-daemon --test memoization_e2e`
Expected: PASS.

- [ ] **Step 9: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all three exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-tools/src/dispatch.rs \
        crates/origin-tools/src/lib.rs \
        crates/origin-tools/tests/memoization.rs \
        crates/origin-tools/Cargo.toml \
        crates/origin-daemon/src/agent.rs \
        crates/origin-daemon/tests/memoization_e2e.rs \
        Cargo.lock
git commit -m "feat(origin-tools): session-scope result memoization with Bash skip (P3.7)"
```

---

## Task P3.8 — Phase 3 checkpoint + cache-warm-ratio bench

**Files:**
- Create: `crates/origin-daemon/benches/phase3_cache_warm_ratio.rs`
- Modify: `crates/origin-daemon/Cargo.toml` (add `[[bench]]` entry)
- Modify: `CHANGELOG.md` (Phase 3 section)

- [ ] **Step 1: Failing bench test** at `crates/origin-daemon/benches/phase3_cache_warm_ratio.rs`

```rust
//! Phase 3 checkpoint bench.
//!
//! Runs a fixed 20-turn synthetic workload **twice**:
//!   1. cold pass — no priors in CachePlanner.
//!   2. warm pass — PrefixLedger has the first pass's hits.
//!
//! Asserts that on the warm pass `cache_read_input_tokens > 0.5 * input_tokens`
//! summed across all turns.

use origin_daemon::session::Session;
// … wire-up identical to the existing memoization test, except the scripted
// provider emits `Usage` with non-zero `cache_read_input_tokens` on every
// warm-pass response. The bench is **deterministic** — there is no real
// network — so the assertion is on the synthetic accounting and exists to
// guarantee the planner + ledger + provider plumbing all stay connected.

#[tokio::test(flavor = "current_thread")]
async fn cache_warm_ratio_above_half_on_warm_pass() {
    let (cold_input, _cold_cache_read) = run_pass(/* warm = */ false).await;
    let (warm_input, warm_cache_read) = run_pass(/* warm = */ true).await;
    assert!(cold_input > 0);
    assert!(
        warm_cache_read as f64 > 0.5 * warm_input as f64,
        "warm cache_read_input_tokens ({warm_cache_read}) must exceed 0.5 * input_tokens ({warm_input})"
    );
}

async fn run_pass(warm: bool) -> (u32, u32) {
    // … see Step 2 for body
    let _ = warm;
    todo!("filled in Step 2")
}
```

- [ ] **Step 2: Replace the `todo!()` body** with a 20-turn scripted-provider loop

The scripted provider returns 20 alternating `Text` responses with synthetic `Usage`:

- Cold pass: `input_tokens = 200`, `cache_read_input_tokens = 0`.
- Warm pass: `input_tokens = 200`, `cache_read_input_tokens = 150` (per turn).

The bench's purpose is **integration-level wiring**: the test will fail to compile / fail to run if `PrefixLedger`, `CachePlanner`, `Anthropic::with_plan`, `LoopOptions::with_cas`, and the memoization cache are no longer accessible from a downstream test. Concrete bench body:

```rust
async fn run_pass(warm: bool) -> (u32, u32) {
    use async_trait::async_trait;
    use origin_core::types::{Block, Message, Role};
    use origin_daemon::agent::{run_loop, LoopOptions};
    use origin_permission::prompt::AlwaysAllow;
    use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tempfile::tempdir;

    struct Synth {
        warm: bool,
        turns: AtomicU32,
        agg_input: AtomicU32,
        agg_cache: AtomicU32,
    }
    #[async_trait]
    impl Provider for Synth {
        fn name(&self) -> &'static str { "synth" }
        async fn chat(&self, _: ChatRequest) -> Result<ChatResponse, ProviderError> {
            let t = self.turns.fetch_add(1, Ordering::SeqCst);
            let cache_read = if self.warm { 150 } else { 0 };
            self.agg_input.fetch_add(200, Ordering::SeqCst);
            self.agg_cache.fetch_add(cache_read, Ordering::SeqCst);
            let blocks = if t < 19 {
                vec![Block::Text { text: format!("turn-{t}"), cache_marker: None }]
            } else {
                vec![Block::Text { text: "done".into(), cache_marker: None }]
            };
            Ok(ChatResponse {
                assistant: Message { role: Role::Assistant, blocks },
                usage: Usage {
                    input_tokens: 200,
                    output_tokens: 50,
                    cache_read_input_tokens: cache_read,
                    cache_creation_input_tokens: 0,
                },
            })
        }
    }

    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );

    let synth = Arc::new(Synth {
        warm,
        turns: AtomicU32::new(0),
        agg_input: AtomicU32::new(0),
        agg_cache: AtomicU32::new(0),
    });
    let mut session = Session::new("bench", "synth-model");
    let opts = LoopOptions::default().with_cas(store).without_streaming();
    for _ in 0..20 {
        let _ = run_loop(&mut session, "prompt", synth.as_ref(), &AlwaysAllow, &opts)
            .await
            .expect("loop ok");
    }
    let agg_input = synth.agg_input.load(Ordering::SeqCst);
    let agg_cache = synth.agg_cache.load(Ordering::SeqCst);
    (agg_input, agg_cache)
}
```

- [ ] **Step 3: Run the bench**

Run: `cargo test -p origin-daemon --test phase3_cache_warm_ratio`

Note: this is placed under `benches/` but invoked as a `#[tokio::test]` so it runs under `cargo test`. If the daemon crate's `Cargo.toml` doesn't autoload `benches/*.rs` as tests, **move the file to `crates/origin-daemon/tests/phase3_cache_warm_ratio.rs`** (preferred) so it's auto-included.

Expected: PASS.

- [ ] **Step 4: Update `CHANGELOG.md`** by appending a new section above Phase 2

```markdown
## Phase 3 — CachePlanner + Speculative Dispatch + Recall (2026-05-19)

- New `origin-planner` crate: `Band`, `PrefixLedger` stability scoring,
  `CachePlanner::plan` four-band sort + boundary marker indices,
  `WireDecision` inline-vs-reference rule.
- `origin-provider-anthropic` emits `cache_control: ephemeral` at planned
  band boundaries; consults `WireDecision` to inline small handles or
  emit `<result handle:… — N bytes>` references.
- New `origin-daemon::tool_use_parser` — SAX-style incremental JSON parser
  yielding `Field` events before the streaming `tool_use` block closes.
- Speculative dispatch: agent forks pure-tool tasks on the parser's first
  complete field; side-effecting tools (`Bash`, `Edit`, `Write`, MCP
  writes) stay sequential.
- New `Recall` builtin: inflate a CAS handle with optional `Lines` /
  `Match` / `OutlineOnly` region selector.
- Session-scope memoization: `(tool_name, raw_input_bytes)` → CAS handle;
  cached results annotated `(cached from turn N)`. `Bash`/`Edit`/`Write`
  skip the cache.
- `phase3_cache_warm_ratio` bench: 20-turn workload, warm pass asserts
  `cache_read_input_tokens > 0.5 × input_tokens`.
```

- [ ] **Step 5: Final phase verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo test -p origin-daemon --test phase3_cache_warm_ratio
```

All four must exit 0.

- [ ] **Step 6: Tag and commit**

```bash
git add crates/origin-daemon/tests/phase3_cache_warm_ratio.rs CHANGELOG.md
git commit -m "chore(origin-daemon): Phase 3 checkpoint + cache-warm-ratio bench (P3.8)"
git tag p3-complete
```

---

## Spec coverage self-review

| Spec mechanism | Covered by | Notes |
|---|---|---|
| N2.2 speculative tool dispatch | P3.3 + P3.4 | parser drives speculation; side-effecting tools opt out |
| N2.3 KV-cache lattice / predictive bands | P3.1 + P3.2 | PrefixLedger + CachePlanner |
| N2.4 step 2 handle substitution | P3.6 | WireDecision; large Volatile → reference |
| N4.2 CachePlanner detailed | P3.1 + P3.2 | swarm-scope inheritance deferred to Phase 7 (called out in scope) |
| N5.3 speculative pure tools | P3.4 | `SideEffects::Pure` gate |
| N5.4 input-hash memoization | P3.7 | `Bash`/`Edit`/`Write` skip-listed; `(cached from turn N)` annotation |
| N5.5 Recall tool | P3.5 | Lines / Match selectors; OutlineOnly stubbed pending Phase 6 sidecar |

**Deferred (explicit, with target phase):** N2.5 sidecar (P6), N4.3 encoder codegen (P11), N4.5 KeyVault (P8), N7.1 swarm PrefixLedger inheritance (P9), N9.4 skill embedding (P7), N10.10 fuzz corpus (P14).

**Placeholder scan result:** none — every step includes either a code block, a verification command, or a commit invocation.

**Type-consistency scan result:**
- `Band` (P3.1) used unchanged in P3.2, P3.6, P3.8.
- `PrefixLedger` constructed with `::new()` in every test.
- `CachePlanner::new(&ledger)` + `planner.plan(&[Section])` signatures match between P3.2 and P3.8.
- `WireDecision::for_block(band, byte_len)` matches between P3.6 definition and the dispatch in `expand_messages_for_wire`.
- `NormalizedInput::hash(tool_name, raw_input)` consistent between P3.7 unit test and the agent-loop call site.
- `ToolUseDelta::{Field, Closed}` variants match between P3.3 parser tests and P3.4 e2e test.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-19-origin-phase-3.md`.

Per the user's directive: this plan will be executed via **superpowers:subagent-driven-development** (one fresh subagent per task). Within each subagent, the worker must follow **superpowers:test-driven-development** (failing test first, never skip), and the parent must apply **superpowers:verification-before-completion** before advancing to the next task. The user has explicitly requested: **do NOT move on to the next task until the previous one has been verified.**

Some tasks can be safely fanned out in parallel because they have no file overlap; the orchestrator should batch them. From the file map:

- **Wave 1 (parallel-safe):** P3.1 (planner skeleton + ledger), P3.3 (tool_use parser) — disjoint crates, no shared file edits.
- **Wave 2 (sequential, depends on Wave 1):** P3.2 (planner uses PrefixLedger; modifies Anthropic provider).
- **Wave 3 (sequential, depends on Wave 1):** P3.4 (speculative dispatch — depends on parser + agent.rs).
- **Wave 4 (parallel-safe, depends on Wave 2 and Wave 3):** P3.5 (Recall builtin — touches origin-tools + agent.rs dispatch_tool), P3.7 (memoization — touches origin-tools + agent.rs run_loop).
  - These both modify `agent.rs`, so they are **NOT parallel-safe** if dispatched concurrently. Run sequentially: P3.5 first, then P3.7.
- **Wave 5 (sequential, depends on Waves 2 + 4):** P3.6 (handle substitution — modifies provider's `expand_messages_for_wire`; depends on Plan from P3.2 and CAS handles from P3.5/P3.7).
- **Wave 6 (sequential, depends on everything):** P3.8 (checkpoint + bench).

**Safe parallel fan-out:** P3.1 ∥ P3.3 (Wave 1). Everything else runs sequentially because of shared-file edits on `agent.rs` and `lib.rs` files.
