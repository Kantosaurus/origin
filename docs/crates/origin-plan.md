# origin-plan

> CRDT op-log and deterministic fold for the shared swarm plan.

## Purpose

`origin-plan` is the conflict-free replicated data type behind the shared plan
that a coordinator and its swarm workers all author against concurrently. Each
actor appends operations to an op-log; folding that log produces a deterministic
materialised `Plan` regardless of the order ops arrive in. It is the substrate
for swarm collaboration: workers add and mark steps, leave notes, reorder, and
lease steps without coordinator round-trips, and every replica converges on the
same plan.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `fold` | fn | Fold an op-log iterator into a deterministic `Plan`. |
| `Op` | enum | Op alphabet: `AddStep`, `MarkStep`, `EditContent`, `AddNote`, `Reorder`, `LeaseStep`. |
| `OpEnvelope` | struct | Wraps an op with its `(lamport, actor)` coordinates. |
| `Plan` / `Step` | struct | Materialised post-fold state. |
| `StepId` | struct | 128-bit producer-chosen step id (hex-serialized). |
| `Status` | enum | `Pending` / `InProgress` / `Done` / `Cancelled`. |
| `Lamport` / `ActorId` / `OpKey` | struct | Lamport-clock ordering primitives. |
| `LogootKey` / `PathComponent` | struct | Dense totally-ordered list positions. |
| `LeaseRecord` / `LeaseOutcome` | struct/enum | Step-lease bookkeeping for P9.2. |
| `Snapshot` | struct | Compaction snapshot (P9.3). |
| `PlanStore` / `PlanStoreError` | struct/enum | rusqlite-backed op-log persistence. |

Module map: `fold`, `lamport`, `lease`, `logoot`, `ops`, `plan`, `snapshot`, `store`.

## Key types

```rust
pub enum Op { AddStep(AddStep), MarkStep(MarkStep), EditContent(EditContent),
              AddNote(AddNote), Reorder(Reorder), LeaseStep(LeaseStep) }

pub struct OpEnvelope { /* op + (lamport, actor) key */ }
impl OpEnvelope { pub fn key(&self) -> OpKey; }

#[must_use]
pub fn fold<I: IntoIterator<Item = OpEnvelope>>(envs: I) -> Plan;

pub struct StepId(u128);   // serialized as 32-char lowercase hex (u128 JSON-safe)
pub enum Status { Pending, InProgress, Done, Cancelled }
```

## How it works

`fold` is the canonical CRDT projection and is **commutative under permutation**
of its input — any reordering yields an identical `Plan` — because it first
sorts every envelope by `(lamport, actor)` (with the op-kind discriminator as a
degenerate tie-breaker), then applies in straight-line order.

```
[ OpEnvelope, ... in any arrival order ]
        │  sort by (lamport, actor) then op-kind
        ▼
   apply loop ─► Plan { steps: StepId → Step, roots }
```

Determinism rules:

- **LWW fields.** `EditContent`, `MarkStep`, and `Reorder` are last-writer-wins
  on the `(lamport, actor)` key; each step tracks the highest key seen per field.
- **`AddNote` appends** in fold order, so notes are stably sorted.
- **`AddStep` is first-writer-wins** on `StepId` (duplicate ids are a producer
  bug, tolerated rather than fatal).
- **Drop-on-floor.** Ops referencing an unknown `StepId` are dropped; when the
  missing `AddStep` later arrives, re-folding the complete log produces the
  correct state.

`LogootKey::between` produces dense, totally-ordered positions so `Reorder`
needs no coordinator round-trip. Lamport ordering also underpins the P9.2 lease
tokens and P9.3 snapshot compaction.

## Dependencies & features

- Runtime deps: `bincode` (op encoding), `origin-cas`, `origin-store`,
  `rusqlite` (bundled; op-log persistence), `serde`, `thiserror`.
- Dev-deps: `proptest` (permutation-invariance property tests), `tempfile`.
  No Cargo features.

## Used by

`Grep "origin-plan" glob "crates/*/Cargo.toml"` →

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-plan/Cargo.toml` (self)
- `crates/origin-planner/Cargo.toml`
- `crates/origin-provider-anthropic/Cargo.toml`
- `crates/origin-swarm/Cargo.toml`
- `crates/origin-tools/Cargo.toml`
- `crates/origin-tui/Cargo.toml`

`origin-swarm`'s `PlanHandle` wraps the fold for shared authoring; the daemon's
`plan_bus` broadcasts updates; the TUI subscribes to render the plan panel.

## Testing

`tests/` directory: `fold_property.rs` (proptest permutation-invariance),
`lease_race.rs` (concurrent lease resolution), and `snapshot_compact.rs`
(snapshot/compaction round-trip). Backed by in-file unit tests in each module.

## See also

- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Data & storage architecture](../architecture/data-and-storage.md)
- [origin-swarm](./origin-swarm.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
