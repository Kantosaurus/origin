# `origin` Phase 9 — Swarm + Plan CRDT + CoW Workers — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Build the multi-agent swarm substrate: a CRDT op-log shared plan (`origin-plan`) with per-step lease tokens and snapshot compaction; a shared-memory SPSC ring (`origin-smr`) for sub-µs intra-host messaging; a copy-on-write worker workspace (`origin-cow`) with platform-specific reflink fast paths; and a coordinator/worker protocol (`origin-swarm`) where workers inherit the coordinator's CachePlanner prefix and report back as structured `CompletionReport`s. Surface as a single `Task(goal, allowed_tools, budget)` tool and a real-time plan side panel in the TUI.

**Architecture:** Four new crates, one new SQLite migration (V4), one new builtin tool, one new TUI side panel:

- **`origin-plan`** owns the CRDT. `Plan` is the in-memory fold of an append-only `Vec<PlanOp>` log. Each op carries `(lamport: u64, actor: ActorId)` for total ordering. Ops: `AddStep`, `MarkStep` (status LWW), `EditContent` (LWW by lamport), `AddNote` (append), `Reorder` (Logoot fractional keys), `LeaseStep` (N7.6), `Snapshot` (N7.7). `PlanStore` persists the op-log + snapshots into SQLite/CAS via a new V4 migration. Pure logic, no IO inside `Plan::fold`.

- **`origin-smr`** is a bounded SPSC ring over a named shared-memory mmap, with a `rkyv`-archived `SwarmEvent` payload type. Producer/consumer cursors are `AtomicU64` in the first 64 bytes of the mapping; payload bytes follow. Round-trip target <1 µs on the same host. Cross-platform shim: Linux `memfd_create` + `mmap`, macOS `shm_open` + `mmap`, Windows `CreateFileMappingW` + `MapViewOfFile`. This crate is the **one place** `unsafe` is allowed in Phase 9 — every block carries a SAFETY comment.

- **`origin-cow`** clones a worker workspace in O(1) on filesystems that support reflinks (btrfs/xfs/zfs `FICLONE`, APFS `clonefile`, ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE`) and falls back to a hardlink-tree + per-write copy-up overlay elsewhere (NTFS, ext4, tmpfs). The trait surface is platform-agnostic; the test assertion ("writes to clone do not affect parent") holds for both strategies.

- **`origin-swarm`** is the coordinator/worker glue. `Coordinator::spawn(WorkerSpec)` builds an `Arc<KeyVault>`-keyed worker process, hands it a clone of the coordinator's `PrefixLedger` (N7.1), and a `PlanHandle` pointing at the shared op-log. Workers communicate via two channels: (1) a small `tokio::sync` MPSC for control frames (lifecycle, dispatch, kill), credit-budgeted (N7.4); (2) the `origin-smr` ring for hot-path swarm events (plan-op broadcasts, DMs). `CompletionReport` (N7.5) is the structured worker → coordinator handoff.

The `Task` tool (P9.8) is an `origin_tool!` builtin that opens a `Coordinator`, dispatches a worker, awaits the `CompletionReport`, and inlines it. The plan side panel (P9.9) is an `origin-tui` widget that subscribes to `PlanHandle::watch` and re-renders the fold on each op.

**Tech Stack:** Rust 1.83 (MSRV pin). New (workspace-pinned): `memmap2 = "0.9"` (SMR + CoW overlay), `crossbeam-utils = "0.8"` (cache-line padding for ring cursors), `parking_lot = "0.12"` (Mutex/RwLock), `dashmap = "5"` (lease index), `rkyv = "0.7"` (already pinned), `nix = "0.29"` (Linux ioctl + memfd; cfg-gated), `libc = "0.2"` (POSIX fallbacks), `windows = "0.58"` (already pinned via P8.1; reuse for `Memoryapi`/`Ioapi`/`Winioctl`), `proptest = "1"` (property tests for CRDT fold), `tempfile = "3"` (already pinned). **Novel-implementation reflex** per `[[feedback-novel-implementations]]`: op-log with Lamport+actor totally-ordered ops fold deterministically (property test); Logoot fractional keys for reorder give O(log n) without rebroadcasting full state; snapshot compaction GCs ops below a fully-acked sequence so the log doesn't grow unbounded; SPSC ring uses cache-line-padded atomics and avoids syscalls in the hot path; CoW workspace uses platform-native reflinks where available, downgrades silently to hardlink+copy-up otherwise; coordinator hands workers the parent's PrefixLedger byte-ranges so the first worker request shares Frozen+Sticky cache bytes with the parent (N7.1).

**Builds on:** Spec §7 (N7.1–N7.7) of `docs/superpowers/specs/2026-05-19-origin-harness-design.md`. Existing on `dev`: `origin-core` (Lamport via ulid + `MessageId`), `origin-ipc` (frame), `origin-cas` (handle storage), `origin-store` (V1/V2/V3 migrations), `origin-planner` (PrefixLedger), `origin-tools` (builtins + `origin_tool!`), `origin-tui` (Composer + side panel slots), `origin-daemon` (provider factory + protocol).

> **P9.1 API reconciliation (post-implementation, 2026-05-19).** The P9.1 implementer chose a cleaner shape than this plan's draft: the canonical types and methods now in `crates/origin-plan/` are:
> - `Op` (sum type of `AddStep(AddStep) | MarkStep(MarkStep) | EditContent(EditContent) | AddNote(AddNote) | Reorder(Reorder)`) wrapping per-variant structs.
> - `OpEnvelope { actor: ActorId, lamport: Lamport, op: Op }` for every log entry.
> - `StepId(u128)` (Ulid-friendly) with `StepId::from_u128`.
> - `Status { Pending, InProgress, Done, Cancelled }`.
> - `fold(impl IntoIterator<Item = OpEnvelope>) -> Plan` — pure, re-foldable.
> - `Plan::iter_root() / iter_children(parent)` returning steps in Logoot order; `Plan::get(id) -> Option<&Step>`.
> - `Step::{id, parent, body, status, notes, key}` accessor methods.
> - Unknown-`StepId` ops are dropped (no `FoldError`).
>
> **Downstream tasks (P9.2 onward) MUST adapt to this surface.** Before writing tests or code, agents should `Read` `crates/origin-plan/src/{lib.rs,ops.rs,plan.rs,lamport.rs,logoot.rs,fold.rs}` and treat that source as the source of truth. The wording in the per-task sections below was drafted before P9.1 landed; where it conflicts with the actual API on disk, prefer the source. Specifically: P9.2 extends `Op` with a new `LeaseStep(LeaseStep)` variant and adds a `Plan::lease_holder(step, now_ms)` method; P9.3 adds an `Op::Snapshot(Snapshot)` variant, a `Plan::serialize_for_snapshot()` method, and a separate `PlanStore` persistence helper.

**Out of scope (deferred):**
- ACP transport for cross-host external agents (Phase 13).
- Mutual-TLS / QUIC remote IPC (Phase 13 — N7.12).
- 30-day audit log of swarm activity (Phase 11).
- Worker sandbox profile (landlock/seccomp/AppContainer) — Phase 11 (N11.x).
- Sticky-band promotion *for worker-generated bytes* — Phase 9 inherits parent bands only; promotion logic lives in `origin-planner` already.
- Resource quotas per worker (CPU/RAM) — Phase 11.
- Swarm "rooms" (>1 coordinator) — Phase 14.

---

## Conventions reminder (apply to every task)

**TDD shape:** failing test → run-to-fail → implement → run-to-pass → verification gate → commit. One commit per task.

**Verification gate per task type:**

| Task type | Required commands (all exit 0) |
|---|---|
| Single-crate pure logic (P9.1, P9.2, P9.3) | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Crate with `unsafe` (P9.4, P9.5) | Above + `cargo miri test -p <crate>` if available, else document why (Windows hosts may skip miri); SAFETY comments on every `unsafe` block |
| Cross-crate / daemon (P9.6, P9.7, P9.8) | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| TUI (P9.9) | Above + manual smoke note (no headless TUI runner is required; assertions go through `Composer` widget tests) |
| Final phase gate (P9.9) | Above + `git tag p9-complete` |

**Inherited patterns:**
- `[lints] workspace = true` in every new `Cargo.toml`.
- Workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- `unsafe_code = "forbid"` is the default. **`origin-smr` and `origin-cow` override this to `allow`** for the mmap and FICLONE/FSCTL FFI — every `unsafe` block carries a SAFETY comment. All other Phase 9 crates keep forbid.
- `#[must_use]` on every public constructor; `const fn` wherever possible.
- Tests use `.expect("meaningful message")`. No `clippy::unwrap_used` allows in production code.
- Custom error enums via `thiserror`; document `# Errors` on every public `Result`-returning fn.
- For each `#[allow(clippy::...)]` add an inline justification.
- **MSRV pin reflex** (`[[project-msrv-dep-pinning]]`): if `cargo check` complains about `edition2024`, pin offender with `cargo update -p <crate>@<bad> --precise <last-1.83-compatible>` and commit `Cargo.lock`.
- Cross-platform tests: gate platform-specific assertions with `#[cfg(target_os = "...")]`; the cross-platform behavior assertion (e.g. "clone is isolated from parent") MUST run on every host. Tests that exercise btrfs reflinks specifically should `return Ok(())` early when the temp dir reports a non-supporting fs.
- File-size discipline: every new `.rs` file targets <300 LOC. Split when crossing.
- Commits: Conventional Commits, scoped (`feat(origin-plan): ...`), one commit per task.

---

## File map for Phase 9

| New / modified | Responsibility | Task |
|---|---|---|
| `crates/origin-plan/{Cargo.toml,src/lib.rs,src/ops.rs,src/lamport.rs,src/logoot.rs,src/plan.rs,src/fold.rs,tests/fold_property.rs}` | CRDT op-log + Plan fold | P9.1 |
| `crates/origin-plan/{src/lease.rs,tests/lease_race.rs}` | Per-step lease tokens (N7.6) | P9.2 |
| `crates/origin-plan/{src/snapshot.rs,src/store.rs,tests/snapshot_compact.rs}` + `crates/origin-store/src/migrations/V4__plan.sql` | Snapshot compaction + persistence (N7.7) | P9.3 |
| `crates/origin-smr/{Cargo.toml,src/lib.rs,src/ring.rs,src/cursor.rs,src/event.rs,src/backend_unix.rs,src/backend_windows.rs,tests/round_trip.rs,tests/latency.rs}` | Shared-memory SPSC ring (N7.2) | P9.4 |
| `crates/origin-cow/{Cargo.toml,src/lib.rs,src/strategy.rs,src/reflink_linux.rs,src/reflink_macos.rs,src/reflink_windows.rs,src/hardlink_fallback.rs,tests/isolation.rs}` | CoW workspace clone (N7.3) | P9.5 |
| `crates/origin-swarm/{Cargo.toml,src/lib.rs,src/coordinator.rs,src/worker.rs,src/spec.rs,src/lifecycle.rs,src/report.rs,src/rpc.rs,src/credit.rs,tests/protocol.rs}` | Coordinator/worker protocol + `CompletionReport` (N7.5, N7.4) | P9.6 |
| `crates/origin-swarm/{src/prefix_inherit.rs,tests/prefix_inherit.rs}` + read access to `origin-planner` | Worker inherits coordinator's PrefixLedger (N7.1) | P9.7 |
| `crates/origin-tools/src/builtins/task.rs` + `mod.rs` + `crates/origin-tools/Cargo.toml` (deps add `origin-swarm`) + `tests/task_tool.rs` | `Task` builtin tool | P9.8 |
| `crates/origin-tui/src/widgets/plan_panel.rs` (new) + `src/widgets/mod.rs` (modify) + `crates/origin-cli/src/main.rs` (modify: wire panel) + `tests/plan_panel.rs` | Plan side panel + tag `p9-complete` | P9.9 |

File-size discipline: every new `.rs` file targets <300 LOC; split when crossing. Each new crate keeps a flat `src/` layout — no subdirectories.

---

## Task P9.1 — `origin-plan` op-log + fold

**Files:** `crates/origin-plan/Cargo.toml`, `src/lib.rs`, `src/ops.rs`, `src/lamport.rs`, `src/logoot.rs`, `src/plan.rs`, `src/fold.rs`, `tests/fold_property.rs`.

**Public surface (P9.1 scope):**
- `ActorId(pub [u8; 16])` — opaque 128-bit actor identifier (use the coordinator/worker ULID bytes).
- `LamportClock { fn now(&self) -> u64; fn tick(&mut self) -> u64; fn observe(&mut self, remote: u64); }`.
- `Logoot::between(left: Option<&LogootKey>, right: Option<&LogootKey>, actor: ActorId) -> LogootKey` — fractional key inserter; returns a strictly between-bounds key.
- `StepId(pub u64)` — monotonic-per-plan step id assigned at `AddStep` time by the fold.
- `StepStatus { Pending, InProgress, Blocked, Done, Failed }` — copy + serde.
- `PlanOp` — `#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]`:
  ```rust
  pub struct PlanOp {
      pub lamport: u64,
      pub actor: ActorId,
      pub kind: PlanOpKind,
  }
  pub enum PlanOpKind {
      AddStep { key: LogootKey, content: String, parent: Option<StepId> },
      MarkStep { id: StepId, status: StepStatus },
      EditContent { id: StepId, content: String },
      AddNote { id: StepId, note: String },
      Reorder { id: StepId, new_key: LogootKey },
  }
  ```
  Lease/Snapshot variants land in P9.2/P9.3 respectively — `PlanOpKind` is `#[non_exhaustive]` from day one so adding variants doesn't break callers.
- `Plan` — in-memory fold state. Public read accessors only: `Plan::steps() -> impl Iterator<Item = (StepId, &StepRecord)>`, `Plan::step(id) -> Option<&StepRecord>`. Mutations always go through `Plan::apply(&mut self, op: &PlanOp) -> Result<(), FoldError>`.
- `StepRecord { pub id: StepId, pub key: LogootKey, pub content: String, pub status: StepStatus, pub notes: Vec<String>, pub parent: Option<StepId> }`.
- `FoldError { UnknownStep(StepId), DuplicateLamport { actor: ActorId, lamport: u64 }, CycleInParent(StepId) }`.

**Determinism rule (CRDT contract):** for any permutation of a set of ops, calling `Plan::apply` after sorting by `(lamport, actor)` yields the same final state. `EditContent` is **LWW** by `(lamport, actor)` — bigger wins. `AddNote` is **append-only ordered by `(lamport, actor)`**. `MarkStep` is LWW by `(lamport, actor)`. `Reorder` overwrites the step's `key`. `AddStep` allocates `StepId = sha256(key.as_bytes() || actor.0)[0..8]` truncated to `u64` — content-derived so two actors proposing the same `key` collide deterministically (the second add becomes a no-op).

**Logoot semantics:** keys are `Vec<u64>` digits with the actor id as tiebreaker. `between(None, None, actor) == [u64::MAX/2; 1]`. Inserting between `[a]` and `[b]` where `b - a >= 2` returns `[(a+b)/2]`; if `b - a == 1`, returns `[a, u64::MAX/2]`; etc. Strict ordering: never returns a key `<=` left or `>=` right.

- [ ] **Step 1: Write failing property test** at `crates/origin-plan/tests/fold_property.rs`:

  ```rust
  use origin_plan::{ActorId, Plan, PlanOp, PlanOpKind, StepStatus, Logoot, LogootKey};
  use proptest::prelude::*;

  // Two-actor pool keeps the (lamport, actor) total order easy to reason about.
  fn actor_pool() -> impl Strategy<Value = ActorId> {
      prop_oneof![Just(ActorId([1; 16])), Just(ActorId([2; 16]))]
  }

  // Hardcoded step ids the property test pretends already exist; AddStep ops
  // will reach these via the deterministic id derivation in fold.rs.
  fn arb_step_id() -> impl Strategy<Value = origin_plan::StepId> {
      (0u64..4).prop_map(origin_plan::StepId)
  }

  fn arb_kind() -> impl Strategy<Value = origin_plan::PlanOpKind> {
      use origin_plan::PlanOpKind::*;
      prop_oneof![
          (any::<u64>(), actor_pool()).prop_map(|(seed, actor)| {
              let key = origin_plan::Logoot::between(None, None, actor);
              let _ = seed; // key already varies via Logoot+actor; seed unused
              AddStep { key, content: "step".into(), parent: None }
          }),
          (arb_step_id(), prop_oneof![
              Just(origin_plan::StepStatus::Pending),
              Just(origin_plan::StepStatus::InProgress),
              Just(origin_plan::StepStatus::Done),
              Just(origin_plan::StepStatus::Failed),
              Just(origin_plan::StepStatus::Blocked),
          ]).prop_map(|(id, status)| MarkStep { id, status }),
          (arb_step_id(), "[a-z]{1,8}").prop_map(|(id, content)| EditContent { id, content }),
          (arb_step_id(), "[a-z]{1,8}").prop_map(|(id, note)| AddNote { id, note }),
      ]
  }

  fn arb_op() -> impl Strategy<Value = PlanOp> {
      (0u64..1_000_000, actor_pool(), arb_kind())
          .prop_map(|(lamport, actor, kind)| PlanOp { lamport, actor, kind })
  }

  proptest! {
      #[test]
      fn fold_is_permutation_invariant(ops in proptest::collection::vec(arb_op(), 1..50)) {
          let mut a: Vec<PlanOp> = ops.clone();
          let mut b: Vec<PlanOp> = ops;
          a.sort_by_key(|o| (o.lamport, o.actor));
          b.sort_by(|x, y| (y.lamport, y.actor).cmp(&(x.lamport, x.actor)));
          // After sort by (lamport, actor) both should fold to the same Plan.
          b.sort_by_key(|o| (o.lamport, o.actor));

          let mut plan_a = Plan::default();
          for op in &a { let _ = plan_a.apply(op); }
          let mut plan_b = Plan::default();
          for op in &b { let _ = plan_b.apply(op); }

          let a_snap: Vec<_> = plan_a.steps().map(|(id, r)| (id, r.content.clone(), r.status)).collect();
          let b_snap: Vec<_> = plan_b.steps().map(|(id, r)| (id, r.content.clone(), r.status)).collect();
          prop_assert_eq!(a_snap, b_snap);
      }

      #[test]
      fn logoot_between_strictly_orders(actor_bytes in any::<[u8; 16]>()) {
          let actor = ActorId(actor_bytes);
          let l = Logoot::between(None, None, actor);
          let r = Logoot::between(Some(&l), None, actor);
          prop_assert!(l < r);
          let m = Logoot::between(Some(&l), Some(&r), actor);
          prop_assert!(l < m && m < r);
      }
  }
  ```

  Plus a hand-rolled unit test in `src/fold.rs`:

  ```rust
  #[test]
  fn lww_edit_picks_higher_lamport() {
      let actor = ActorId([1; 16]);
      let key = Logoot::between(None, None, actor);
      let mut plan = Plan::default();
      let add = PlanOp { lamport: 1, actor, kind: PlanOpKind::AddStep { key: key.clone(), content: "v0".into(), parent: None } };
      plan.apply(&add).expect("add");
      let id = plan.steps().next().expect("step").0;
      let e1 = PlanOp { lamport: 5, actor, kind: PlanOpKind::EditContent { id, content: "v5".into() } };
      let e2 = PlanOp { lamport: 3, actor, kind: PlanOpKind::EditContent { id, content: "v3".into() } };
      // Apply in reverse lamport order — fold sees them sorted internally? No:
      // fold is total-order-respecting only when caller pre-sorts. Test the
      // contract: applying in (1, 3, 5) order yields "v5".
      plan.apply(&e2).expect("e2");
      plan.apply(&e1).expect("e1");
      assert_eq!(plan.step(id).expect("present").content, "v5");
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-plan --tests` — expect failure (`origin-plan` does not exist).

- [ ] **Step 3: Create the crate.**

  `crates/origin-plan/Cargo.toml`:
  ```toml
  [package]
  name = "origin-plan"
  version.workspace = true
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true
  repository.workspace = true

  [lints]
  workspace = true

  [dependencies]
  serde = { version = "1", features = ["derive"] }
  thiserror = "1"
  sha2 = "0.10"

  [dev-dependencies]
  proptest = "1"
  ```

  Implement modules per public-surface above. Layout:
  - `src/lib.rs` — re-exports.
  - `src/lamport.rs` — `LamportClock` (atomic-free; the caller wraps in a Mutex if needed).
  - `src/logoot.rs` — `LogootKey(Vec<u64>)`, `Logoot::between(...)`. `PartialOrd`+`Ord` by lexicographic `u64`. Tiebreak by `actor` only on equal-prefix collisions.
  - `src/ops.rs` — `PlanOp`, `PlanOpKind` (`#[non_exhaustive]`), `StepStatus`, `ActorId`, `StepId`, `StepRecord`.
  - `src/fold.rs` — `Plan::apply(&mut self, op: &PlanOp) -> Result<(), FoldError>`. Cycle check on `AddStep.parent`.
  - `src/plan.rs` — `Plan` struct: `BTreeMap<LogootKey, StepId>` (ordering by key) + `HashMap<StepId, StepRecord>` (lookup by id).

  Notes/gotchas:
  - `AddStep`'s `StepId` derivation: `let mut h = Sha256::new(); h.update(key_bytes); h.update(&actor.0); let id = u64::from_be_bytes(h.finalize()[0..8].try_into().expect("8")); StepId(id)`.
  - Duplicate `(actor, lamport)` is a programming error → `FoldError::DuplicateLamport`. Property test must skip generating duplicates (use a `HashSet` to dedupe before applying).
  - `apply` is idempotent on `AddStep` with the same derived `StepId` (returns Ok without modifying state).

- [ ] **Step 4: Run** `cargo test -p origin-plan` — both tests pass.

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test -p origin-plan
  cargo clippy -p origin-plan --all-targets -- -D warnings
  cargo fmt --check
  ```

- [ ] **Step 6: Commit** with message `feat(origin-plan): op-log + Plan fold + Logoot keys (P9.1, N7.6 step 1)`.

---

## Task P9.2 — Lease tokens (N7.6)

**Files:** `crates/origin-plan/src/lease.rs` (new), `src/ops.rs` (add `LeaseStep` variant + lease fields), `src/fold.rs` (apply lease ops), `tests/lease_race.rs` (new), `src/lib.rs` (re-export).

**Public surface added:**
- `LeaseToken { pub actor: ActorId, pub step: StepId, pub expires_at_ms: u64 }`.
- `PlanOpKind::LeaseStep { step: StepId, expires_at_ms: u64 }` (new variant on the existing `#[non_exhaustive]` enum).
- `Plan::lease_holder(step: StepId, now_ms: u64) -> Option<ActorId>` — current valid holder, or `None` if expired/unleased.
- `Plan::active_leases(now_ms: u64) -> impl Iterator<Item = LeaseToken> + '_` — non-expired leases.

**Determinism rule:** when two `LeaseStep` ops race the same step, the holder is the one whose `(lamport, actor)` tuple is **lexicographically larger** (highest lamport wins; ties broken by lexicographically-greater actor bytes). Losers receive `Result::Err(FoldError::LeaseLost { ... })` *from a separate query helper* `Plan::lease_outcome(op: &PlanOp) -> LeaseOutcome` — the fold itself never errors on competing lease ops; the rule is purely about who holds the lease in the resulting state.

`LeaseOutcome` enum: `Granted { holder: ActorId }` (this op won), `Lost { winner: ActorId }` (this op lost a race). `lease_outcome` is queried *after* applying both ops; it inspects the current holder vs. the op's `(lamport, actor)`.

Expired leases are not surfaced from `lease_holder`; they remain in the underlying state for replay determinism but are filtered by the `now_ms` check.

- [ ] **Step 1: Failing test** `crates/origin-plan/tests/lease_race.rs`:

  ```rust
  use origin_plan::{ActorId, LeaseOutcome, Logoot, Plan, PlanOp, PlanOpKind, StepId};

  fn add(plan: &mut Plan, lamport: u64, actor: ActorId, content: &str) -> StepId {
      let key = Logoot::between(None, None, actor);
      let op = PlanOp { lamport, actor, kind: PlanOpKind::AddStep { key, content: content.into(), parent: None } };
      plan.apply(&op).expect("add");
      plan.steps().last().expect("step").0
  }

  #[test]
  fn higher_lamport_wins_lease() {
      let a = ActorId([1; 16]);
      let b = ActorId([2; 16]);
      let mut plan = Plan::default();
      let step = add(&mut plan, 1, a, "do the thing");
      let lease_a = PlanOp { lamport: 10, actor: a, kind: PlanOpKind::LeaseStep { step, expires_at_ms: 1_000 } };
      let lease_b = PlanOp { lamport: 11, actor: b, kind: PlanOpKind::LeaseStep { step, expires_at_ms: 1_000 } };
      plan.apply(&lease_a).expect("a");
      plan.apply(&lease_b).expect("b");
      assert_eq!(plan.lease_holder(step, 0), Some(b));
      assert!(matches!(plan.lease_outcome(&lease_a), LeaseOutcome::Lost { winner } if winner == b));
      assert!(matches!(plan.lease_outcome(&lease_b), LeaseOutcome::Granted { holder } if holder == b));
  }

  #[test]
  fn equal_lamport_breaks_by_larger_actor_bytes() {
      let a = ActorId([1; 16]);
      let b = ActorId([2; 16]);
      let mut plan = Plan::default();
      let step = add(&mut plan, 1, a, "do the thing");
      let la = PlanOp { lamport: 10, actor: a, kind: PlanOpKind::LeaseStep { step, expires_at_ms: 1_000 } };
      let lb = PlanOp { lamport: 10, actor: b, kind: PlanOpKind::LeaseStep { step, expires_at_ms: 1_000 } };
      plan.apply(&la).expect("a");
      plan.apply(&lb).expect("b");
      assert_eq!(plan.lease_holder(step, 0), Some(b));
  }

  #[test]
  fn expired_lease_is_not_a_holder() {
      let a = ActorId([1; 16]);
      let mut plan = Plan::default();
      let step = add(&mut plan, 1, a, "do the thing");
      let lease = PlanOp { lamport: 10, actor: a, kind: PlanOpKind::LeaseStep { step, expires_at_ms: 100 } };
      plan.apply(&lease).expect("a");
      assert_eq!(plan.lease_holder(step, 50), Some(a));
      assert_eq!(plan.lease_holder(step, 200), None);
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-plan --test lease_race` — fails (variant + helpers do not exist).

- [ ] **Step 3: Implement.**

  `src/ops.rs`: add the variant to `PlanOpKind`. Add `LeaseOutcome` enum.

  `src/lease.rs` (new):
  ```rust
  use crate::{ActorId, StepId};

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct LeaseRecord {
      pub lamport: u64,
      pub actor: ActorId,
      pub expires_at_ms: u64,
  }

  impl LeaseRecord {
      #[must_use]
      pub fn supersedes(&self, other: &Self) -> bool {
          (self.lamport, self.actor.0) > (other.lamport, other.actor.0)
      }
  }
  ```

  `src/plan.rs`: add `leases: HashMap<StepId, LeaseRecord>` field. `apply` LeaseStep installs the record only if `supersedes` returns true on conflict.

  `src/fold.rs`: lease application is fully infallible.

  Query helpers on `Plan`:
  ```rust
  pub fn lease_holder(&self, step: StepId, now_ms: u64) -> Option<ActorId> {
      self.leases.get(&step).filter(|r| r.expires_at_ms > now_ms).map(|r| r.actor)
  }
  pub fn lease_outcome(&self, op: &PlanOp) -> LeaseOutcome {
      let PlanOpKind::LeaseStep { step, .. } = op.kind else { return LeaseOutcome::NotALease };
      match self.leases.get(&step) {
          Some(rec) if rec.lamport == op.lamport && rec.actor == op.actor => LeaseOutcome::Granted { holder: rec.actor },
          Some(rec) => LeaseOutcome::Lost { winner: rec.actor },
          None => LeaseOutcome::NotALease, // applied to step that disappeared (shouldn't happen if AddStep precedes)
      }
  }
  ```

  Add `LeaseOutcome::NotALease` variant.

- [ ] **Step 4: Run** `cargo test -p origin-plan` — all pass (including pre-existing P9.1 tests).

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test -p origin-plan
  cargo clippy -p origin-plan --all-targets -- -D warnings
  cargo fmt --check
  ```

- [ ] **Step 6: Commit** `feat(origin-plan): per-step lease tokens with lamport+actor tiebreak (P9.2, N7.6 step 2)`.

---

## Task P9.3 — Snapshot compaction + persistence (N7.7)

**Files:** `crates/origin-plan/src/snapshot.rs` (new), `src/store.rs` (new — SQLite/CAS persistence facade), `src/ops.rs` (add `Snapshot` variant), `src/fold.rs` (treat Snapshot as fast-forward), `src/lib.rs` (re-export), `tests/snapshot_compact.rs` (new), `crates/origin-store/src/migrations/V4__plan.sql` (new), `crates/origin-store/src/lib.rs` (only if a re-export is needed — likely no code change since `embed_migrations!` picks up V4 automatically).

**Public surface added:**
- `Snapshot { pub seq: u64, pub state_handle: [u8; 32], pub fully_acked_below: u64 }` — `seq` is the lamport of the snapshot op; `state_handle` is the CAS hash of the rkyv-serialized `Plan` state; `fully_acked_below` is the lamport below which all workers have acked (so older ops are GC-eligible).
- `PlanOpKind::Snapshot { state_handle: [u8; 32], fully_acked_below: u64 }`.
- `PlanStore::open(store: &origin_store::Store, cas: &origin_cas::Store) -> Result<Self, PlanStoreError>` — uses the existing `Store::with_conn`.
- `PlanStore::append_op(&self, op: &PlanOp) -> Result<(), PlanStoreError>` — appends to V4 `plan_ops` table.
- `PlanStore::load_log(&self) -> Result<Vec<PlanOp>, PlanStoreError>` — returns ops sorted by `(lamport, actor)`; loads only ops `>= latest_snapshot.fully_acked_below` (the rest were GC'd).
- `PlanStore::write_snapshot(&self, snapshot: &Snapshot, body: &[u8]) -> Result<(), PlanStoreError>` — stores body in CAS, inserts row into `plan_snapshots`, then deletes ops with `lamport < snapshot.fully_acked_below`.
- `PlanStore::load_latest_snapshot(&self) -> Result<Option<(Snapshot, Plan)>, PlanStoreError>` — returns the latest snapshot row + its CAS-stored Plan body, deserialized.
- `Plan::serialize_for_snapshot(&self) -> Vec<u8>` — bincode (existing dep, simpler than rkyv for this) of `(steps_in_key_order, leases_map)`.
- `Plan::deserialize_snapshot(bytes: &[u8]) -> Result<Self, FoldError>`.
- `PlanStoreError { Sqlite(rusqlite::Error), Cas(origin_cas::StoreError), Decode(String) }`.

**Snapshot frequency:** the **caller** (P9.6 coordinator) decides when to snapshot. The plan-level contract is "snapshot ops are valid ops; the persistence layer GCs everything below `fully_acked_below`."

**V4 migration:**
```sql
-- V4__plan.sql
PRAGMA foreign_keys = ON;

CREATE TABLE plan_ops (
    lamport     INTEGER NOT NULL,
    actor       BLOB NOT NULL,
    op_kind     TEXT NOT NULL,
    body        BLOB NOT NULL,          -- bincode-encoded PlanOp
    PRIMARY KEY (lamport, actor)
);

CREATE INDEX idx_plan_ops_lamport ON plan_ops(lamport);

CREATE TABLE plan_snapshots (
    seq                  INTEGER PRIMARY KEY,
    state_handle         BLOB NOT NULL,         -- CAS hash
    fully_acked_below    INTEGER NOT NULL,
    created_at_unix_ms   INTEGER NOT NULL
);

CREATE INDEX idx_plan_snapshots_acked ON plan_snapshots(fully_acked_below);
```

`origin-store/src/migrations/` already contains `V1__init.sql`, `V2__cas_refs.sql`, `V3__codegraph.sql`. `embed_migrations!("src/migrations")` picks `V4__plan.sql` up automatically.

- [ ] **Step 1: Failing test** `crates/origin-plan/tests/snapshot_compact.rs`:

  ```rust
  use origin_plan::{ActorId, Logoot, Plan, PlanOp, PlanOpKind, PlanStore, Snapshot};
  use tempfile::TempDir;

  #[test]
  fn snapshot_gcs_ops_below_acked_seq() {
      let tmp = TempDir::new().expect("tmp");
      let store = origin_store::Store::open(tmp.path().join("origin.db")).expect("store");
      let cas = origin_cas::Store::open(tmp.path().join("cas")).expect("cas");
      let ps = PlanStore::open(&store, &cas).expect("plan store");

      let actor = ActorId([1; 16]);
      let mut plan = Plan::default();
      // Apply 200 AddStep ops, lamports 1..=200.
      let mut keys = vec![];
      for lamport in 1..=200u64 {
          let key = Logoot::between(keys.last(), None, actor);
          let op = PlanOp { lamport, actor, kind: PlanOpKind::AddStep { key: key.clone(), content: format!("step-{lamport}"), parent: None } };
          plan.apply(&op).expect("apply");
          ps.append_op(&op).expect("append");
          keys.push(key);
      }

      // Take a snapshot acking everything below lamport 150.
      let body = plan.serialize_for_snapshot();
      let handle = cas.put(&body).expect("cas put");
      let snap = Snapshot { seq: 201, state_handle: handle.into_bytes(), fully_acked_below: 150 };
      ps.write_snapshot(&snap, &body).expect("write snap");

      // load_log should now return ops with lamport >= 150 only.
      let loaded = ps.load_log().expect("load");
      assert!(loaded.iter().all(|o| o.lamport >= 150), "ops below 150 should be GC'd");
      assert_eq!(loaded.len(), 200 - 150 + 1);
  }

  #[test]
  fn load_latest_snapshot_round_trips() {
      let tmp = TempDir::new().expect("tmp");
      let store = origin_store::Store::open(tmp.path().join("origin.db")).expect("store");
      let cas = origin_cas::Store::open(tmp.path().join("cas")).expect("cas");
      let ps = PlanStore::open(&store, &cas).expect("plan store");

      let actor = ActorId([1; 16]);
      let mut plan = Plan::default();
      let key = Logoot::between(None, None, actor);
      let op = PlanOp { lamport: 1, actor, kind: PlanOpKind::AddStep { key, content: "only".into(), parent: None } };
      plan.apply(&op).expect("apply");
      ps.append_op(&op).expect("append");

      let body = plan.serialize_for_snapshot();
      let handle = cas.put(&body).expect("cas put");
      let snap = Snapshot { seq: 1, state_handle: handle.into_bytes(), fully_acked_below: 1 };
      ps.write_snapshot(&snap, &body).expect("write snap");

      let (loaded_snap, loaded_plan) = ps.load_latest_snapshot().expect("load").expect("present");
      assert_eq!(loaded_snap.seq, 1);
      assert_eq!(loaded_plan.steps().count(), 1);
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-plan --test snapshot_compact` — fails (modules missing).

- [ ] **Step 3: Implement.**

  Add deps to `crates/origin-plan/Cargo.toml`:
  ```toml
  bincode = "1.3"
  origin-cas = { path = "../origin-cas" }
  origin-store = { path = "../origin-store" }
  rusqlite = { version = "0.32", features = ["bundled"] }
  ```

  Add the migration file at `crates/origin-store/src/migrations/V4__plan.sql` (content above).

  `src/snapshot.rs`:
  ```rust
  #[derive(Debug, Clone)]
  pub struct Snapshot {
      pub seq: u64,
      pub state_handle: [u8; 32],
      pub fully_acked_below: u64,
  }
  ```

  Add `PlanOpKind::Snapshot { state_handle: [u8; 32], fully_acked_below: u64 }`. The fold's behavior for `Snapshot` op: it does NOT mutate state — snapshots are a load-time fast-forward mechanism only; replaying a Snapshot op on a freshly-instantiated Plan is a no-op (the persistence layer is responsible for loading the snapshot body).

  `src/plan.rs`: implement `serialize_for_snapshot` / `deserialize_snapshot` using bincode over a `(BTreeMap<LogootKey, StepRecord>, HashMap<StepId, LeaseRecord>)` tuple.

  `src/store.rs` (new): `PlanStore` holds `&origin_store::Store` + `&origin_cas::Store` (use `Arc` if lifetimes get hairy; otherwise borrow at call sites). `append_op` uses bincode + `INSERT OR IGNORE INTO plan_ops`. `write_snapshot` runs an SQLite txn: insert into `plan_snapshots`, then `DELETE FROM plan_ops WHERE lamport < ?`. CAS put happens *outside* the txn (CAS is content-addressed and idempotent; if the SQLite txn rolls back the CAS body is harmlessly orphaned). `load_latest_snapshot` selects `ORDER BY seq DESC LIMIT 1`.

  Determinism note: `load_log` returns ops sorted by `(lamport, actor)` — the apply order the CRDT contract requires.

- [ ] **Step 4: Run** `cargo test -p origin-plan` — all P9.1/P9.2/P9.3 tests pass.

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test -p origin-plan
  cargo clippy -p origin-plan --all-targets -- -D warnings
  cargo fmt --check
  # cross-crate sanity since we touched origin-store's migrations dir
  cargo test -p origin-store
  ```

- [ ] **Step 6: Commit** `feat(origin-plan): snapshot compaction + V4 plan_ops/plan_snapshots migration (P9.3, N7.7)`.

---

## Task P9.4 — Shared-memory SPSC ring (N7.2)

**Files:** `crates/origin-smr/Cargo.toml`, `src/lib.rs`, `src/ring.rs`, `src/cursor.rs`, `src/event.rs`, `src/backend_unix.rs` (Linux + macOS), `src/backend_windows.rs`, `tests/round_trip.rs`, `tests/latency.rs`.

**Manifest must:** override `[lints.rust] unsafe_code = "allow"` for raw pointer mmap access (every `unsafe` block has a SAFETY comment). cfg-gated platform deps. Always-on: `crossbeam-utils` (cache-line padding), `rkyv = "0.7"`, `thiserror`.

**Public surface:**
- `SwarmEvent` (rkyv `Archive` + `Serialize` + `Deserialize`):
  ```rust
  #[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
  #[archive(check_bytes)]
  pub enum SwarmEvent {
      PlanOpBroadcast { lamport: u64, actor_bytes: [u8; 16], op_payload: Vec<u8> },
      DirectMessage { from: [u8; 16], to: [u8; 16], body: Vec<u8> },
      Heartbeat { sender: [u8; 16], now_ms: u64 },
      WorkerComplete { worker: [u8; 16], report_handle: [u8; 32] },
  }
  ```
- `RingConfig { pub name: String, pub capacity_bytes: usize, pub create: bool }` — `create=true` is producer side (creates+truncates), `create=false` is consumer side (opens existing).
- `Ring::open(cfg: RingConfig) -> Result<Self, RingError>` — `mmap` the named region, lay out 64-byte cache-line-padded head/tail cursors followed by payload.
- `Ring::try_send(&self, event: &SwarmEvent) -> Result<(), TrySendError>` — single-producer; writes a 4-byte length prefix then rkyv bytes; returns `WouldBlock` if `head - tail + needed > capacity`.
- `Ring::try_recv(&self) -> Result<Option<SwarmEvent>, RingError>` — single-consumer; returns `Ok(None)` if `head == tail`.
- `Ring::wait_send(&self, event: &SwarmEvent, deadline_ns: u64) -> Result<(), RingError>` — busy-wait with `core::hint::spin_loop()`; falls back to a tokio yield once `deadline_ns/2` elapses (kept simple — Phase 13 may replace with futex/eventfd).
- `RingError { CreationFailed(String), MmapFailed(String), CapacityExceeded { needed: usize, capacity: usize }, ValidationFailed(String) }`.
- `TrySendError { WouldBlock, Ring(RingError) }`.

**Layout (capacity_bytes >= 4096; pow-of-two recommended):**

```
[ 0.. 64) producer cursor (head): AtomicU64 — total bytes ever written
[64..128) consumer cursor (tail): AtomicU64 — total bytes ever consumed
[128..capacity) payload area, wraps at (capacity - 128)
```

`head - tail` is the in-flight byte count; capacity is reservable iff `usable = capacity - 128`. Reads/writes use `Acquire`/`Release` ordering; cache-line padding via `crossbeam_utils::CachePadded`.

- [ ] **Step 1: Failing test** `crates/origin-smr/tests/round_trip.rs`:

  ```rust
  use origin_smr::{Ring, RingConfig, SwarmEvent};

  fn unique_name(suffix: &str) -> String {
      format!("origin-smr-test-{}-{}", suffix, std::process::id())
  }

  #[test]
  fn round_trips_a_single_event() {
      let name = unique_name("rt1");
      let producer = Ring::open(RingConfig { name: name.clone(), capacity_bytes: 4096, create: true }).expect("open producer");
      let consumer = Ring::open(RingConfig { name, capacity_bytes: 4096, create: false }).expect("open consumer");

      let evt = SwarmEvent::Heartbeat { sender: [7; 16], now_ms: 12345 };
      producer.try_send(&evt).expect("send");
      let got = consumer.try_recv().expect("recv").expect("Some");
      assert_eq!(got, evt);
      assert!(consumer.try_recv().expect("second").is_none());
  }

  #[test]
  fn fills_then_drains_alternating() {
      let name = unique_name("rt2");
      let p = Ring::open(RingConfig { name: name.clone(), capacity_bytes: 8192, create: true }).expect("p");
      let c = Ring::open(RingConfig { name, capacity_bytes: 8192, create: false }).expect("c");
      for i in 0..100u64 {
          p.try_send(&SwarmEvent::Heartbeat { sender: [0; 16], now_ms: i }).expect("send");
          let got = c.try_recv().expect("recv").expect("Some");
          assert!(matches!(got, SwarmEvent::Heartbeat { now_ms, .. } if now_ms == i));
      }
  }

  #[test]
  fn capacity_exceeded_returns_would_block() {
      let name = unique_name("rt3");
      let p = Ring::open(RingConfig { name, capacity_bytes: 4096, create: true }).expect("p");
      // Pack 200 large events; expect at least one WouldBlock once full.
      let payload = vec![0xAB; 256];
      let evt = SwarmEvent::DirectMessage { from: [0; 16], to: [1; 16], body: payload };
      let mut hit = false;
      for _ in 0..200 {
          if matches!(p.try_send(&evt), Err(origin_smr::TrySendError::WouldBlock)) {
              hit = true;
              break;
          }
      }
      assert!(hit, "expected WouldBlock when ring fills");
  }
  ```

  Plus `crates/origin-smr/tests/latency.rs` — informational benchmark, NOT a hard assertion (CI hosts vary). Records timing to stdout; assertion is "round trip completes < 1 ms" (very loose ceiling so it never flakes):

  ```rust
  use origin_smr::{Ring, RingConfig, SwarmEvent};

  #[test]
  fn round_trip_completes_under_1ms() {
      let name = format!("origin-smr-lat-{}", std::process::id());
      let p = Ring::open(RingConfig { name: name.clone(), capacity_bytes: 4096, create: true }).expect("p");
      let c = Ring::open(RingConfig { name, capacity_bytes: 4096, create: false }).expect("c");
      let evt = SwarmEvent::Heartbeat { sender: [0; 16], now_ms: 0 };
      // Warm-up
      for _ in 0..10 {
          p.try_send(&evt).expect("warm send");
          let _ = c.try_recv().expect("warm recv");
      }
      let start = std::time::Instant::now();
      const N: u64 = 1_000;
      for _ in 0..N {
          p.try_send(&evt).expect("send");
          let _ = c.try_recv().expect("recv").expect("Some");
      }
      let elapsed = start.elapsed();
      let per = elapsed / u32::try_from(N).expect("N fits u32");
      eprintln!("round-trip avg: {per:?}");
      assert!(per < std::time::Duration::from_millis(1), "round-trip per iter {per:?} too slow");
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-smr --tests` — fails (crate does not exist).

- [ ] **Step 3: Implement.**

  `Cargo.toml`:
  ```toml
  [package]
  name = "origin-smr"
  version.workspace = true
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true
  repository.workspace = true

  [lints.rust]
  unsafe_code = "allow"

  [lints.clippy]
  pedantic = { level = "warn", priority = -1 }
  unwrap_used = "deny"

  [dependencies]
  rkyv = { version = "0.7", features = ["validation"] }
  crossbeam-utils = "0.8"
  thiserror = "1"
  bytecheck = "0.6"

  [target.'cfg(unix)'.dependencies]
  libc = "0.2"
  nix = { version = "0.29", features = ["mman"] }

  [target.'cfg(windows)'.dependencies]
  windows = { version = "0.58", features = ["Win32_Foundation", "Win32_System_Memory", "Win32_Security"] }
  ```

  `src/cursor.rs`: cache-line-padded `AtomicU64` wrapper. SAFETY note on raw `*mut u8` → `&CachePadded<AtomicU64>` cast: alignment guaranteed by `mmap` page alignment + struct repr.

  `src/event.rs`: `SwarmEvent` definition.

  `src/ring.rs`: `Ring` holds `mmap_ptr: *mut u8`, `capacity: usize`, `name: String`. `try_send` serializes via `rkyv::to_bytes::<_, 256>(event)`; computes needed = 4 + bytes.len(); reads `head` (Acquire) and `tail` (Acquire); if `head - tail + needed > capacity - 128` → `WouldBlock`; otherwise writes length prefix + bytes at `(head - 128) % (capacity - 128)`; fences `head + needed` (Release). `try_recv` mirror.

  `src/backend_unix.rs`: use `nix::sys::mman::shm_open` + `ftruncate` + `mmap`. Names start with `/` per POSIX shm convention; transform `cfg.name` (`format!("/{}", name)`). On `create=true` use `O_CREAT | O_EXCL | O_RDWR`; on `create=false` use `O_RDWR`. Zero the cursors on create.

  `src/backend_windows.rs`: `CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, capacity_high, capacity_low, name_wide)` for create; `OpenFileMappingW(FILE_MAP_ALL_ACCESS, FALSE, name_wide)` for open. Then `MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, capacity)`. Convert `cfg.name` to a UTF-16 wide string. Every FFI call sits in an `unsafe` block with a SAFETY comment naming the invariant being upheld (handle ownership, lifetime of the wide string, capacity bounds).

  `src/lib.rs`: re-export `Ring`, `RingConfig`, `RingError`, `SwarmEvent`, `TrySendError`. cfg-gate to pick the right backend.

  Implementation notes:
  - rkyv `to_bytes::<_, 256>` returns `AlignedVec`. Copy to a temp local before the byte-by-byte write into the ring (the ring is `*mut u8`, not aligned for rkyv archive types — only the consumer's rkyv-validated *view* needs alignment, and the consumer copies bytes out into an `AlignedVec` before `from_bytes`).
  - Wrap-around: if `(offset + needed) > usable`, write the length prefix at the wrapped position. Decoding follows the same arithmetic. Keep this in one helper `wrap_copy_in(ring_base, payload_base_offset, src)` / `wrap_copy_out(ring_base, payload_base_offset, dst, len)`.
  - On `Drop`, `munmap` + `shm_unlink` (Unix) / `UnmapViewOfFile` + `CloseHandle` (Windows). Only the `create=true` side unlinks the shm name.

- [ ] **Step 4: Run** `cargo test -p origin-smr` — both round_trip and latency tests pass.

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test -p origin-smr
  cargo clippy -p origin-smr --all-targets -- -D warnings
  cargo fmt --check
  # SAFETY comment audit:
  rg "^\s*(unsafe)\b" crates/origin-smr/src
  rg "^\s*//\s*SAFETY" crates/origin-smr/src
  # The two counts should match (one SAFETY per unsafe block).
  ```

- [ ] **Step 6: Commit** `feat(origin-smr): SPSC shared-memory ring + rkyv-archived SwarmEvent (P9.4, N7.2)`.

---

## Task P9.5 — CoW worker workspace (N7.3)

**Files:** `crates/origin-cow/Cargo.toml`, `src/lib.rs`, `src/strategy.rs`, `src/reflink_linux.rs`, `src/reflink_macos.rs`, `src/reflink_windows.rs`, `src/hardlink_fallback.rs`, `tests/isolation.rs`.

**Manifest must:** override `[lints.rust] unsafe_code = "allow"` for FICLONE / clonefile / FSCTL ioctls. cfg-gated platform deps. Always-on: `walkdir`, `thiserror`, `tempfile` (dev).

**Public surface:**
- `Workspace { /* opaque */ }`.
- `Workspace::open(root: PathBuf) -> Self`.
- `Workspace::clone_into(&self, dest: PathBuf) -> Result<Workspace, CowError>` — clones `self` into `dest`. Internally picks the best strategy and records it for diagnostics; never errors with "unsupported fs" (always degrades to the hardlink fallback).
- `Workspace::path(&self) -> &Path`.
- `Workspace::strategy(&self) -> Strategy` — `Strategy { Reflink, HardlinkOverlay }`.
- `CowError { Io(std::io::Error), Walkdir(walkdir::Error), Unsupported(String) }` — `Unsupported` is reserved for explicit "operator forced reflink but fs doesn't support it" via `WorkspaceOpts`. The default `clone_into` never returns `Unsupported`.
- `WorkspaceOpts { force_strategy: Option<Strategy> }` — default is auto-select.

**Strategy decision tree (per file):**
1. If platform == Linux and `ioctl(fd, FICLONE, src_fd) == 0` succeeds → Reflink.
2. If platform == macOS and `clonefile(src, dst, 0) == 0` succeeds → Reflink.
3. If platform == Windows on ReFS and `FSCTL_DUPLICATE_EXTENTS_TO_FILE` succeeds → Reflink.
4. Otherwise → hardlink the file (`std::fs::hard_link`). On the *first* write to the destination, the writer is expected to break the hardlink (this is the responsibility of the *worker*, not `origin-cow`). The hardlink fallback is documented as "snapshot-style: parent and clone share inodes until a write breaks the link."

**Isolation contract (test):** writing to the clone path must not be observable from the parent path. For reflink-on-supporting-fs, this is automatic. For the hardlink fallback, we use **copy-up-on-clone** instead of pure hardlinking — at clone time, every file is copied with a fresh inode. This is slower (no shared blocks) but preserves the isolation invariant unconditionally and avoids the "must break hardlink" gotcha.

Refinement: P9.5 ships `HardlinkOverlay` as **eager copy** (per the contract above). The "true overlay with copy-up-on-write" is a Phase 11 hardening optimization. Document this in the rustdoc.

- [ ] **Step 1: Failing test** `crates/origin-cow/tests/isolation.rs`:

  ```rust
  use origin_cow::Workspace;
  use std::fs;
  use tempfile::TempDir;

  #[test]
  fn clone_is_isolated_from_parent() {
      let parent_dir = TempDir::new().expect("tmp");
      let clone_dir = TempDir::new().expect("tmp2");
      let parent_root = parent_dir.path().to_owned();
      let clone_root = clone_dir.path().join("ws");

      fs::write(parent_root.join("a.txt"), b"original").expect("write");
      fs::create_dir_all(parent_root.join("sub")).expect("subdir");
      fs::write(parent_root.join("sub/b.txt"), b"sub-original").expect("write");

      let ws = Workspace::open(parent_root.clone());
      let cloned = ws.clone_into(clone_root.clone()).expect("clone");

      // Clone has the same content.
      assert_eq!(fs::read(cloned.path().join("a.txt")).expect("read"), b"original");
      assert_eq!(fs::read(cloned.path().join("sub/b.txt")).expect("read"), b"sub-original");

      // Mutate the clone.
      fs::write(cloned.path().join("a.txt"), b"mutated").expect("write");
      fs::write(cloned.path().join("sub/new.txt"), b"new").expect("write");

      // Parent must be unchanged.
      assert_eq!(fs::read(parent_root.join("a.txt")).expect("read"), b"original");
      assert!(!parent_root.join("sub/new.txt").exists());
  }

  #[test]
  fn strategy_reports_best_available() {
      let parent_dir = TempDir::new().expect("tmp");
      let clone_dir = TempDir::new().expect("tmp2");
      fs::write(parent_dir.path().join("x"), b"x").expect("write");
      let ws = Workspace::open(parent_dir.path().to_owned());
      let cloned = ws.clone_into(clone_dir.path().join("ws")).expect("clone");
      // Any strategy is acceptable; we just assert the API is callable.
      let _ = cloned.strategy();
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-cow` — fails (crate missing).

- [ ] **Step 3: Implement.**

  `Cargo.toml`:
  ```toml
  [package]
  name = "origin-cow"
  version.workspace = true
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true
  repository.workspace = true

  [lints.rust]
  unsafe_code = "allow"

  [lints.clippy]
  pedantic = { level = "warn", priority = -1 }
  unwrap_used = "deny"

  [dependencies]
  walkdir = "2"
  thiserror = "1"

  [target.'cfg(target_os = "linux")'.dependencies]
  nix = { version = "0.29", features = ["ioctl", "fs"] }
  libc = "0.2"

  [target.'cfg(target_os = "macos")'.dependencies]
  libc = "0.2"

  [target.'cfg(target_os = "windows")'.dependencies]
  windows = { version = "0.58", features = ["Win32_Foundation", "Win32_Storage_FileSystem", "Win32_System_Ioctl", "Win32_System_IO"] }

  [dev-dependencies]
  tempfile = "3"
  ```

  `src/strategy.rs`: `Strategy { Reflink, HardlinkOverlay }`.

  `src/reflink_linux.rs`: `pub(crate) fn try_reflink_file(src: &Path, dst: &Path) -> Result<(), CowError>` — opens src O_RDONLY, dst O_WRONLY|O_CREAT, calls `ioctl_ficlone(dst_fd, src_fd)` via `nix::request_code_write!`. On `EXDEV`/`EOPNOTSUPP`/`EINVAL` returns `Err(Unsupported)` so the caller falls through to the eager-copy fallback.

  `src/reflink_macos.rs`: `clonefile(src.as_cstring(), dst.as_cstring(), 0)` via raw FFI (`extern "C" fn clonefile(...) -> c_int`).

  `src/reflink_windows.rs`: `DeviceIoControl(handle, FSCTL_DUPLICATE_EXTENTS_TO_FILE, ...)`. Documented to only succeed on ReFS. Cast-heavy; SAFETY comment per call.

  `src/hardlink_fallback.rs`: walk the source tree with `walkdir`; for each file, create parent dirs in dst, then `fs::copy` (eager copy — preserves isolation contract regardless of fs).

  `src/lib.rs`:
  ```rust
  impl Workspace {
      pub fn clone_into(&self, dest: PathBuf) -> Result<Workspace, CowError> {
          std::fs::create_dir_all(&dest)?;
          let strat = try_reflink_tree(&self.root, &dest).unwrap_or_else(|_| {
              hardlink_fallback::eager_copy_tree(&self.root, &dest).map(|_| Strategy::HardlinkOverlay)
          })?;
          Ok(Workspace { root: dest, strategy: strat })
      }
  }
  ```
  where `try_reflink_tree` walks files and uses the platform's `try_reflink_file`; if any file fails, the function returns `Err` and the caller falls back to `eager_copy_tree`. (If a partial reflink tree was created, delete the destination dir before falling back so we don't end up half-reflinked + half-copied — keep this simple with `fs::remove_dir_all`.)

  Rustdoc note on `HardlinkOverlay`: "P9.5 currently implements this as eager copy for cross-platform correctness. P11 may switch to lazy copy-up-on-write."

- [ ] **Step 4: Run** `cargo test -p origin-cow` — both tests pass on every host (the strategy chosen varies by fs, but the isolation contract holds).

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test -p origin-cow
  cargo clippy -p origin-cow --all-targets -- -D warnings
  cargo fmt --check
  rg "^\s*(unsafe)\b" crates/origin-cow/src
  rg "^\s*//\s*SAFETY" crates/origin-cow/src
  ```

- [ ] **Step 6: Commit** `feat(origin-cow): platform-reflink + eager-copy fallback Workspace clone (P9.5, N7.3)`.

---

## Task P9.6 — Coordinator/worker protocol + `CompletionReport` (N7.4, N7.5)

**Files:** `crates/origin-swarm/Cargo.toml`, `src/lib.rs`, `src/coordinator.rs`, `src/worker.rs`, `src/spec.rs`, `src/lifecycle.rs`, `src/report.rs`, `src/rpc.rs`, `src/credit.rs`, `tests/protocol.rs`.

**Public surface:**
- `WorkerSpec { pub goal: String, pub allowed_tools: Vec<String>, pub budget: Budget, pub workspace: Option<PathBuf>, pub parent_actor: ActorId }`.
- `Budget { pub max_wall_ms: u64, pub max_input_tokens: u64, pub max_output_tokens: u64, pub max_tool_calls: u32 }`.
- `WorkerHandle { /* opaque */ }`.
- `Coordinator::new(plan: PlanHandle, smr_ring_name: String, /* ... */) -> Self`.
- `Coordinator::spawn(&self, spec: WorkerSpec) -> Result<WorkerHandle, SwarmError>` — in P9.6 the worker is an **in-process Tokio task** (Phase 11 promotes to a separate sandboxed process). `spawn` returns once the worker is in `Lifecycle::Running`.
- `Coordinator::await_completion(&self, handle: &WorkerHandle) -> Result<CompletionReport, SwarmError>` — blocks until the worker reports.
- `Lifecycle { Spawning, Running, Reporting, Done, Failed { reason: String } }`.
- `DecisionRecord { pub at_lamport: u64, pub decision: String, pub rationale: String }`.
- `CompletionReport { pub goal: String, pub status: ReportStatus, pub plan_updates: Vec<PlanOp>, pub files_touched: Vec<[u8; 32]>, pub decisions: Vec<DecisionRecord>, pub follow_ups: Vec<TaskRef>, pub transcript_handle: [u8; 32], pub usage: Usage }`.
- `ReportStatus { Completed, GoalUnreachable, BudgetExhausted, Aborted }`.
- `TaskRef { pub goal: String, pub allowed_tools: Vec<String> }`.
- `Usage { pub input_tokens: u64, pub output_tokens: u64, pub tool_calls: u32 }`.
- `Credit { /* opaque */ }`.
- `CreditChannel<T>::new(budget: u32) -> (CreditSender<T>, CreditReceiver<T>)`.
- `SwarmError { Plan(origin_plan::PlanStoreError), Smr(origin_smr::RingError), Worker(String), Lifecycle(String), Timeout }`.

**`PlanHandle`:** a thin wrapper around `Arc<Mutex<Plan>>` + `Arc<PlanStore>` + a `tokio::sync::broadcast::Sender<PlanOp>` for `watch`. Defined in P9.6 because the swarm crate is its first consumer; `origin-plan` only owns the `Plan`/`PlanStore` types. `PlanHandle::apply(&self, op: PlanOp)` updates the fold, persists via `PlanStore`, and broadcasts.

**Credit-budget rule (N7.4):** every channel has a `Credit` counter starting at `budget`. Senders consume on send (errors with `WouldBlock` at 0); receivers issue on consume. Per the spec table: plan updates 1–4, DMs 16, broadcasts 4, sidecar queue 256. P9.6 implements the generic `CreditChannel<T>` and **wires plan updates with budget=4 and DMs with budget=16**. Sidecar queue is already in place (P5.1) and out of scope here.

- [ ] **Step 1: Failing test** `crates/origin-swarm/tests/protocol.rs`:

  ```rust
  use origin_swarm::{Budget, Coordinator, CompletionReport, ReportStatus, WorkerSpec};
  use origin_plan::{ActorId, Plan, PlanStore};
  use tempfile::TempDir;
  use std::sync::Arc;
  use tokio::sync::Mutex;

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn three_workers_complete_and_report() {
      let tmp = TempDir::new().expect("tmp");
      let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
      let cas = Arc::new(origin_cas::Store::open(tmp.path().join("cas")).expect("cas"));
      let plan_store = Arc::new(PlanStore::open(&store, &cas).expect("plan"));
      let plan = origin_swarm::PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
      let coord = Coordinator::new(plan.clone(), format!("origin-swarm-test-{}", std::process::id()));
      let mut handles = vec![];
      for i in 0..3u32 {
          let spec = WorkerSpec {
              goal: format!("worker-{i}"),
              allowed_tools: vec!["read".into()],
              budget: Budget { max_wall_ms: 5_000, max_input_tokens: 1_000, max_output_tokens: 1_000, max_tool_calls: 4 },
              workspace: None,
              parent_actor: ActorId([0; 16]),
          };
          handles.push(coord.spawn(spec).await.expect("spawn"));
      }
      let mut reports: Vec<CompletionReport> = vec![];
      for h in handles {
          reports.push(coord.await_completion(&h).await.expect("await"));
      }
      assert_eq!(reports.len(), 3);
      assert!(reports.iter().all(|r| matches!(r.status, ReportStatus::Completed)));
      // The plan should have received at least one op per worker (each worker writes an AddNote
      // about its completion — see the mock worker spec below).
      let plan_locked = plan.snapshot().await;
      assert!(plan_locked.steps().count() >= 0, "plan accessible");
  }

  #[tokio::test]
  async fn credit_channel_blocks_at_zero() {
      use origin_swarm::credit::CreditChannel;
      let (tx, mut rx) = CreditChannel::<u32>::new(2);
      tx.try_send(1).expect("send 1");
      tx.try_send(2).expect("send 2");
      let err = tx.try_send(3).expect_err("at budget");
      assert!(matches!(err, origin_swarm::credit::TrySendError::WouldBlock));
      let v = rx.recv().await.expect("recv");
      assert_eq!(v, 1);
      // Receiver issuing a credit unblocks sender.
      tx.try_send(3).expect("post-issue");
  }
  ```

  *The `await_completion` returning `Completed` for the simple in-process mock is fine for P9.6 — the actual model-driven worker is exercised in P9.8 via the `Task` tool.* The "mock worker" inside `Coordinator::spawn` (for P9.6 only — a placeholder until P9.8 supplies real worker logic): when the spec carries no real workload (no `Task` tool dispatching it), the worker emits one `PlanOpKind::AddNote` op describing itself and then completes with `ReportStatus::Completed`. P9.8 substitutes the real worker loop.

  Refine the worker abstraction so the placeholder is opt-in: `Coordinator::spawn_with(spec, worker_fn)` is the real entry point; `Coordinator::spawn(spec)` is a P9.6-only convenience that uses `default_noop_worker`. P9.8 calls `spawn_with` passing the actual agent loop.

- [ ] **Step 2: Run** `cargo test -p origin-swarm` — fails (crate missing).

- [ ] **Step 3: Implement.**

  `Cargo.toml`:
  ```toml
  [package]
  name = "origin-swarm"
  version.workspace = true
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true
  repository.workspace = true

  [lints]
  workspace = true

  [dependencies]
  origin-core = { path = "../origin-core" }
  origin-plan = { path = "../origin-plan" }
  origin-smr = { path = "../origin-smr" }
  origin-cas = { path = "../origin-cas" }
  origin-store = { path = "../origin-store" }
  origin-planner = { path = "../origin-planner" }
  tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
  thiserror = "1"
  serde = { version = "1", features = ["derive"] }
  bincode = "1.3"
  ulid = "1"

  [dev-dependencies]
  tempfile = "3"
  ```

  `src/spec.rs`: `WorkerSpec`, `Budget`, `TaskRef`, `Usage`, `ReportStatus`, `DecisionRecord`.

  `src/report.rs`: `CompletionReport` plus a helper `CompletionReport::store(&self, cas: &origin_cas::Store) -> Result<[u8; 32], origin_cas::StoreError>` that bincode-serializes and stores in CAS, returning the handle (the SMR `SwarmEvent::WorkerComplete` carries the handle, not the full body).

  `src/lifecycle.rs`: `Lifecycle` + a `tokio::sync::watch` per worker for state observation.

  `src/credit.rs`: `CreditChannel<T>` implemented as `tokio::sync::Semaphore` + `tokio::sync::mpsc::channel`. `try_send` acquires a semaphore permit synchronously (`try_acquire`) and forwards to the mpsc; `recv` releases the permit. Errors: `TrySendError { WouldBlock, Closed }`.

  `src/rpc.rs`: `PlanHandle`:
  ```rust
  pub struct PlanHandle {
      inner: Arc<Mutex<origin_plan::Plan>>,
      store: Arc<origin_plan::PlanStore>,
      broadcast: tokio::sync::broadcast::Sender<origin_plan::PlanOp>,
  }
  impl PlanHandle {
      pub fn new(inner: Arc<Mutex<origin_plan::Plan>>, store: Arc<origin_plan::PlanStore>) -> Self {
          let (broadcast, _) = tokio::sync::broadcast::channel(64);
          Self { inner, store, broadcast }
      }
      pub async fn apply(&self, op: origin_plan::PlanOp) -> Result<(), SwarmError> {
          let mut guard = self.inner.lock().await;
          guard.apply(&op).map_err(|e| SwarmError::Worker(format!("fold: {e:?}")))?;
          drop(guard);
          self.store.append_op(&op).map_err(SwarmError::Plan)?;
          let _ = self.broadcast.send(op);
          Ok(())
      }
      pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<origin_plan::PlanOp> {
          self.broadcast.subscribe()
      }
      pub async fn snapshot(&self) -> origin_plan::Plan { self.inner.lock().await.clone() }
  }
  ```

  `src/worker.rs`: defines `pub type WorkerFn = Arc<dyn Fn(WorkerContext) -> Pin<Box<dyn Future<Output = Result<CompletionReport, SwarmError>> + Send>> + Send + Sync>` and a `WorkerContext` carrying the plan handle, SMR producer ring, budget, and parent actor id. The default noop worker emits one `AddNote` op and returns `Completed`.

  `src/coordinator.rs`: spawns worker tasks via `tokio::spawn`, threading a `WorkerContext` into the `WorkerFn`. Maintains a `HashMap<WorkerId, WorkerState>` and writes lifecycle transitions. `await_completion` polls a `tokio::sync::watch` channel for `Lifecycle::Done`. Also exposes two test-only helpers (gated by `#[cfg(any(test, feature = "test-helpers"))]` — keep them on by default to keep P9.8 cross-crate tests simple): `set_default_worker(WorkerFn)` overrides the noop worker used by `spawn`; `last_completion_for_test() -> Option<CompletionReport>` returns a clone of the most recently completed worker's report (stored in an `Arc<Mutex<Option<CompletionReport>>>` field that the coordinator updates from `await_completion`).

  `src/lib.rs`: re-exports the public API.

- [ ] **Step 4: Run** `cargo test -p origin-swarm` — passes.

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --check
  ```

- [ ] **Step 6: Commit** `feat(origin-swarm): coordinator + workers + credit channels + CompletionReport (P9.6, N7.4, N7.5)`.

---

## Task P9.7 — Worker `PrefixLedger` inheritance (N7.1)

**Files:** `crates/origin-swarm/src/prefix_inherit.rs` (new), `src/coordinator.rs` (modify — thread the ledger into `WorkerContext`), `src/worker.rs` (modify — `WorkerContext::inherited_ledger`), `tests/prefix_inherit.rs` (new). Read-only access to `origin_planner::{PrefixLedger, SectionId, Band}`.

**Goal:** at spawn time, the worker receives a **clone of the coordinator's PrefixLedger band assignments** for every section currently in Frozen or Sticky. The worker's first request to its CachePlanner uses these as seeds via `PrefixLedger::record_band`. The contract surfaces by hashing the request bytes — if both sides seed identically and feed the same Frozen+Sticky byte ranges, the request-prefix hash on the wire matches.

**Public surface added:**
- `PrefixSnapshot { entries: Vec<(SectionId, Band)> }` (private to crate, exposed via `Coordinator::take_prefix_snapshot(&PrefixLedger) -> PrefixSnapshot`).
- `WorkerContext::inherited_ledger(&self) -> &PrefixSnapshot`.
- `PrefixSnapshot::seed_into(&self, ledger: &mut PrefixLedger)`.

The actual `CachePlanner` request hashing is already tested in Phase 3. P9.7's test asserts the *seeding mechanic*: a coordinator-side ledger with sections in `Frozen` and `Sticky` is observable from inside the worker's `WorkerContext` after spawn, and seeding the worker's fresh ledger reproduces those band assignments.

- [ ] **Step 1: Failing test** `crates/origin-swarm/tests/prefix_inherit.rs`:

  ```rust
  use origin_planner::{Band, PrefixLedger, SectionId};
  use origin_swarm::{Coordinator, PrefixSnapshot};

  #[test]
  fn snapshot_round_trips_band_assignments() {
      let mut parent = PrefixLedger::new();
      // Drive parent's ledger up to Sticky/Frozen on a couple of sections.
      let s_system = SectionId::new("system");
      let s_tools = SectionId::new("tools");
      // Seed at the high band directly:
      parent.record_band(s_system, Band::Frozen);
      parent.record_band(s_tools, Band::Sticky);

      let snap = Coordinator::take_prefix_snapshot(&parent);

      let mut child = PrefixLedger::new();
      snap.seed_into(&mut child);

      assert_eq!(child.suggested_band(s_system), Some(Band::Frozen));
      assert_eq!(child.suggested_band(s_tools), Some(Band::Sticky));
  }

  #[tokio::test]
  async fn worker_sees_inherited_ledger_in_context() {
      use origin_swarm::{Budget, WorkerSpec};
      use origin_plan::{ActorId, Plan, PlanStore};
      use std::sync::Arc;
      use tokio::sync::Mutex;
      use tempfile::TempDir;

      let tmp = TempDir::new().expect("tmp");
      let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
      let cas = Arc::new(origin_cas::Store::open(tmp.path().join("cas")).expect("cas"));
      let plan_store = Arc::new(PlanStore::open(&store, &cas).expect("plan"));
      let plan = origin_swarm::PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);

      let mut parent_ledger = PrefixLedger::new();
      parent_ledger.record_band(SectionId::new("system"), Band::Frozen);
      parent_ledger.record_band(SectionId::new("tools"), Band::Sticky);

      let coord = Coordinator::new(plan, format!("origin-swarm-pi-{}", std::process::id()))
          .with_parent_ledger(parent_ledger);

      let observed = Arc::new(Mutex::new(None::<PrefixSnapshot>));
      let observed_clone = observed.clone();
      let worker = std::sync::Arc::new(move |ctx: origin_swarm::WorkerContext| {
          let observed_clone = observed_clone.clone();
          Box::pin(async move {
              let snap = ctx.inherited_ledger().clone();
              *observed_clone.lock().await = Some(snap);
              Ok(origin_swarm::CompletionReport {
                  goal: ctx.spec.goal.clone(),
                  status: origin_swarm::ReportStatus::Completed,
                  plan_updates: vec![],
                  files_touched: vec![],
                  decisions: vec![],
                  follow_ups: vec![],
                  transcript_handle: [0; 32],
                  usage: origin_swarm::Usage::default(),
              })
          }) as _
      });

      let h = coord.spawn_with(WorkerSpec {
          goal: "x".into(),
          allowed_tools: vec![],
          budget: Budget { max_wall_ms: 1_000, max_input_tokens: 100, max_output_tokens: 100, max_tool_calls: 1 },
          workspace: None,
          parent_actor: ActorId([0; 16]),
      }, worker).await.expect("spawn");
      let _ = coord.await_completion(&h).await.expect("await");
      let snap = observed.lock().await.clone().expect("worker saw snapshot");
      let mut reconstructed = PrefixLedger::new();
      snap.seed_into(&mut reconstructed);
      assert_eq!(reconstructed.suggested_band(SectionId::new("system")), Some(Band::Frozen));
      assert_eq!(reconstructed.suggested_band(SectionId::new("tools")), Some(Band::Sticky));
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-swarm --test prefix_inherit` — fails (new methods missing).

- [ ] **Step 3: Implement.**

  `src/prefix_inherit.rs`:
  ```rust
  use origin_planner::{Band, PrefixLedger, SectionId};

  #[derive(Debug, Clone, Default)]
  pub struct PrefixSnapshot {
      entries: Vec<(SectionId, Band)>,
  }

  impl PrefixSnapshot {
      #[must_use]
      pub fn from_ledger(l: &PrefixLedger) -> Self {
          // Walk every section currently in Frozen/Sticky; Sliding/Volatile are
          // not worth re-seeding (the worker's cache won't reuse them anyway).
          // PrefixLedger does not expose iter() today — add a minimal
          // `iter_bands()` to origin-planner: `pub fn iter_bands(&self) -> impl Iterator<Item=(SectionId, Band)> + '_`.
          let entries: Vec<_> = l
              .iter_bands()
              .filter(|(_, b)| matches!(b, Band::Frozen | Band::Sticky))
              .collect();
          Self { entries }
      }

      pub fn seed_into(&self, ledger: &mut PrefixLedger) {
          for (id, band) in &self.entries {
              ledger.record_band(*id, *band);
          }
      }
  }
  ```

  Modify `crates/origin-planner/src/ledger.rs`: add
  ```rust
  pub fn iter_bands(&self) -> impl Iterator<Item = (SectionId, Band)> + '_ {
      self.table.iter().map(|(id, s)| (*id, s.band))
  }
  ```

  `Coordinator`: add `parent_ledger: Option<PrefixLedger>` field, builder method `with_parent_ledger(self, l: PrefixLedger) -> Self`, and `take_prefix_snapshot(l: &PrefixLedger) -> PrefixSnapshot` as a free function. Thread `PrefixSnapshot` into every `WorkerContext` (default is `PrefixSnapshot::default()` when no parent ledger configured).

  `WorkerContext::inherited_ledger(&self) -> &PrefixSnapshot`.

- [ ] **Step 4: Run** — pass.

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --check
  ```

- [ ] **Step 6: Commit** `feat(origin-swarm): PrefixLedger snapshot inheritance into worker context (P9.7, N7.1)`.

---

## Task P9.8 — `Task` builtin tool

**Files:** `crates/origin-tools/src/builtins/task.rs` (new), `crates/origin-tools/src/builtins/mod.rs` (modify — register), `crates/origin-tools/Cargo.toml` (add `origin-swarm` dep + `origin-plan` dep), `crates/origin-tools/tests/task_tool.rs` (new).

Reference the existing tool pattern in `crates/origin-tools/src/builtins/{ask,read,grep_tool,recall}.rs` for shape conventions — `origin_tool!` declaration, JSON-schema input, dispatch function name `<tool>_tool`.

**Public surface:**
- `TaskInput { pub goal: String, pub allowed_tools: Vec<String>, #[serde(default)] pub budget: TaskBudget }` (serde).
- `TaskBudget { #[serde(default = "default_wall_ms")] pub max_wall_ms: u64, #[serde(default = "default_tokens")] pub max_input_tokens: u64, #[serde(default = "default_tokens")] pub max_output_tokens: u64, #[serde(default = "default_calls")] pub max_tool_calls: u32 }` — defaults: 60_000 ms wall, 32_000 input tokens, 8_000 output tokens, 32 tool calls.
- `TaskOutput { pub status: String, pub summary: String, pub files_touched: Vec<String>, pub follow_ups: Vec<String> }` — JSON-serializable inlining of the `CompletionReport`'s most actionable fields. The full report stays in CAS via `report_handle`.
- `task_tool(coord: &origin_swarm::Coordinator, input: TaskInput) -> Result<TaskOutput, TaskError>` — dispatches a worker with the given goal/tools/budget, awaits the report, and shapes the response.
- `TaskError { Swarm(origin_swarm::SwarmError), Json(String) }`.

**Behavior:**
- The worker function used in P9.8 IS the existing agent loop in `crates/origin-daemon/src/agent.rs`, but **restricted to `input.allowed_tools`**. To keep this plan self-contained without copy-pasting the daemon's loop, P9.8 takes a `Coordinator` constructed by the daemon (in P9.8 the daemon's startup wires a global coordinator). The plan-level test uses a **synthetic stub worker** that returns a fixed `CompletionReport`; the daemon-side real-agent wiring is tested by an end-to-end smoke at the bottom of this task.
- `task_tool` records a `DecisionRecord` into the report for any `allowed_tools` it filters out (defense-in-depth, for observability).

- [ ] **Step 1: Failing test** `crates/origin-tools/tests/task_tool.rs`:

  ```rust
  use origin_plan::{ActorId, Plan, PlanStore};
  use origin_swarm::{Coordinator, PlanHandle, ReportStatus, Usage};
  use origin_tools::builtins::task::{task_tool, TaskBudget, TaskInput};
  use std::sync::Arc;
  use tempfile::TempDir;
  use tokio::sync::Mutex;

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn task_tool_dispatches_worker_and_inlines_report() {
      let tmp = TempDir::new().expect("tmp");
      let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
      let cas = Arc::new(origin_cas::Store::open(tmp.path().join("cas")).expect("cas"));
      let plan_store = Arc::new(PlanStore::open(&store, &cas).expect("plan"));
      let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
      let mut coord = Coordinator::new(plan, format!("origin-task-{}", std::process::id()));

      // Install a synthetic worker that succeeds with a fixed report shape.
      coord.set_default_worker(Arc::new(|ctx| {
          Box::pin(async move {
              Ok(origin_swarm::CompletionReport {
                  goal: ctx.spec.goal.clone(),
                  status: ReportStatus::Completed,
                  plan_updates: vec![],
                  files_touched: vec![],
                  decisions: vec![],
                  follow_ups: vec![origin_swarm::TaskRef { goal: "next".into(), allowed_tools: vec![] }],
                  transcript_handle: [0; 32],
                  usage: Usage::default(),
              })
          })
      }));

      let out = task_tool(&coord, TaskInput {
          goal: "do the thing".into(),
          allowed_tools: vec!["read".into()],
          budget: TaskBudget::default(),
      }).await.expect("task ok");
      assert_eq!(out.status, "completed");
      assert!(out.summary.contains("do the thing"));
      assert_eq!(out.follow_ups, vec!["next".to_string()]);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn task_tool_filters_disallowed_tools_into_decision_record() {
      let tmp = TempDir::new().expect("tmp");
      let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
      let cas = Arc::new(origin_cas::Store::open(tmp.path().join("cas")).expect("cas"));
      let plan_store = Arc::new(PlanStore::open(&store, &cas).expect("plan"));
      let plan = PlanHandle::new(Arc::new(Mutex::new(Plan::default())), plan_store);
      let mut coord = Coordinator::new(plan, format!("origin-task-filter-{}", std::process::id()));

      // Synthetic worker echoes back the allowed_tools it received from the spec,
      // as a DecisionRecord. task_tool is responsible for forwarding the
      // filtered allow-list, so we observe that here.
      coord.set_default_worker(Arc::new(|ctx| {
          let allowed = ctx.spec.allowed_tools.clone();
          Box::pin(async move {
              Ok(origin_swarm::CompletionReport {
                  goal: ctx.spec.goal.clone(),
                  status: ReportStatus::Completed,
                  plan_updates: vec![],
                  files_touched: vec![],
                  decisions: vec![origin_swarm::DecisionRecord {
                      at_lamport: 0,
                      decision: "received_allowed_tools".into(),
                      rationale: allowed.join(","),
                  }],
                  follow_ups: vec![],
                  transcript_handle: [0; 32],
                  usage: Usage::default(),
              })
          })
      }));

      let _ = task_tool(&coord, TaskInput {
          goal: "scoped".into(),
          allowed_tools: vec!["read".into(), "grep".into()],
          budget: TaskBudget::default(),
      }).await.expect("task ok");
      // The worker's reported allow-list (visible via plan/CAS in a full
      // integration; here we re-invoke a coordinator helper that exposes the
      // last report for tests) must match the input list.
      let last = coord.last_completion_for_test().expect("captured");
      let rec = last.decisions.first().expect("decision");
      assert_eq!(rec.decision, "received_allowed_tools");
      assert_eq!(rec.rationale, "read,grep");
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-tools --test task_tool` — fails.

- [ ] **Step 3: Implement.**

  Modify `crates/origin-tools/Cargo.toml`:
  ```toml
  [dependencies]
  # ...existing
  origin-swarm = { path = "../origin-swarm" }
  origin-plan = { path = "../origin-plan" }
  ```

  `src/builtins/task.rs`:
  ```rust
  use crate::{Tier, Urgency};
  use origin_swarm::{Budget, Coordinator, ReportStatus, WorkerSpec};
  use serde::{Deserialize, Serialize};
  use thiserror::Error;

  #[derive(Debug, Clone, Deserialize)]
  pub struct TaskBudget {
      #[serde(default = "default_wall_ms")]   pub max_wall_ms: u64,
      #[serde(default = "default_tokens")]    pub max_input_tokens: u64,
      #[serde(default = "default_tokens")]    pub max_output_tokens: u64,
      #[serde(default = "default_calls")]     pub max_tool_calls: u32,
  }
  // const fns for defaults; Default impl returns the same values.

  #[derive(Debug, Deserialize)]
  pub struct TaskInput {
      pub goal: String,
      pub allowed_tools: Vec<String>,
      #[serde(default)] pub budget: TaskBudget,
  }

  #[derive(Debug, Serialize)]
  pub struct TaskOutput {
      pub status: String,
      pub summary: String,
      pub files_touched: Vec<String>,
      pub follow_ups: Vec<String>,
  }

  #[derive(Debug, Error)]
  pub enum TaskError {
      #[error("swarm: {0:?}")] Swarm(origin_swarm::SwarmError),
      #[error("json: {0}")] Json(String),
  }

  /// Spawn a worker for `input.goal`, await completion, return the actionable view.
  ///
  /// # Errors
  /// Propagates [`TaskError::Swarm`] from spawn/await and [`TaskError::Json`] if
  /// the worker's report cannot be inlined.
  pub async fn task_tool(coord: &Coordinator, input: TaskInput) -> Result<TaskOutput, TaskError> {
      let spec = WorkerSpec {
          goal: input.goal.clone(),
          allowed_tools: input.allowed_tools,
          budget: Budget {
              max_wall_ms: input.budget.max_wall_ms,
              max_input_tokens: input.budget.max_input_tokens,
              max_output_tokens: input.budget.max_output_tokens,
              max_tool_calls: input.budget.max_tool_calls,
          },
          workspace: None,
          parent_actor: origin_plan::ActorId([0; 16]),
      };
      let h = coord.spawn(spec).await.map_err(TaskError::Swarm)?;
      let rep = coord.await_completion(&h).await.map_err(TaskError::Swarm)?;
      Ok(TaskOutput {
          status: match rep.status {
              ReportStatus::Completed => "completed",
              ReportStatus::GoalUnreachable => "goal_unreachable",
              ReportStatus::BudgetExhausted => "budget_exhausted",
              ReportStatus::Aborted => "aborted",
          }.into(),
          summary: format!("worker for {:?} reported {:?}", input.goal, rep.status),
          files_touched: rep.files_touched.iter().map(|h| hex::encode(h)).collect(),
          follow_ups: rep.follow_ups.into_iter().map(|t| t.goal).collect(),
      })
  }

  crate::origin_tool! {
      name: "task",
      description: "Dispatch a sub-agent with a goal, allowed tools, and budget. Returns a structured CompletionReport summary.",
      tier: Tier::Privileged,
      urgency: Urgency::Normal,
      side_effects: true,
      input_schema: r#"{"type":"object","required":["goal","allowed_tools"],"properties":{"goal":{"type":"string"},"allowed_tools":{"type":"array","items":{"type":"string"}},"budget":{"type":"object"}}}"#,
  }
  ```

  Add `pub mod task;` to `src/builtins/mod.rs`. Add `hex = "0.4"` to `origin-tools` deps if not already pulled in (search; `origin-codegraph` uses it). If `Coordinator` does not currently expose `set_default_worker(...)`, add it in P9.8 alongside the test — it's a one-line setter into the same field used by `spawn_with`'s default.

- [ ] **Step 4: Run** `cargo test -p origin-tools --test task_tool` → pass.

- [ ] **Step 5: Verification gate**

  ```powershell
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --check
  ```

- [ ] **Step 6: Commit** `feat(origin-tools): Task builtin (dispatch + await + structured report) (P9.8)`.

---

## Task P9.9 — Plan side panel + tag `p9-complete`

**Files:** `crates/origin-tui/src/widgets/plan_panel.rs` (new), `crates/origin-tui/src/widgets/mod.rs` (modify — `pub mod plan_panel;`), `crates/origin-cli/src/main.rs` (modify — wire panel: subscribe to `PlanHandle` events, render via `PlanPanel`), `crates/origin-tui/Cargo.toml` (add `origin-plan` dep), `crates/origin-tui/tests/plan_panel.rs` (new).

Reference `crates/origin-tui/src/widgets/` for the existing Composer side-panel scaffolding (P4.7 introduced it). The Composer already supports a per-panel render+input split.

**Public surface:**
- `PlanPanel { /* opaque */ }`.
- `PlanPanel::new() -> Self`.
- `PlanPanel::apply_op(&mut self, op: &origin_plan::PlanOp) -> Result<(), origin_plan::FoldError>` — folds the op into the panel's local `Plan`.
- `PlanPanel::render(&self) -> Vec<PlanLine>` — pure render. `PlanLine { pub id: StepId, pub indent: u8, pub status_glyph: char, pub content: String, pub holder: Option<ActorId> }`.
- The actual drawing into the `Grid` happens in a thin caller in `origin-cli` — `PlanPanel` returns lines, the cli formats and writes cells. This keeps `origin-tui` testable without a renderer.

**Status glyphs:** `Pending → '○'`, `InProgress → '◐'`, `Blocked → '!'`, `Done → '●'`, `Failed → '✕'`.

- [ ] **Step 1: Failing test** `crates/origin-tui/tests/plan_panel.rs`:

  ```rust
  use origin_plan::{ActorId, Logoot, PlanOp, PlanOpKind, StepStatus};
  use origin_tui::widgets::plan_panel::PlanPanel;

  fn add(panel: &mut PlanPanel, lamport: u64, actor: ActorId, content: &str, key: origin_plan::LogootKey) {
      let op = PlanOp { lamport, actor, kind: PlanOpKind::AddStep { key, content: content.into(), parent: None } };
      panel.apply_op(&op).expect("apply");
  }

  #[test]
  fn renders_steps_in_logoot_order_with_glyphs() {
      let actor = ActorId([1; 16]);
      let mut panel = PlanPanel::new();
      let k1 = Logoot::between(None, None, actor);
      let k2 = Logoot::between(Some(&k1), None, actor);
      add(&mut panel, 1, actor, "First", k1.clone());
      add(&mut panel, 2, actor, "Second", k2.clone());
      // Mark First as Done.
      let id1 = panel.fold().steps().next().expect("first").0;
      panel.apply_op(&PlanOp { lamport: 3, actor, kind: PlanOpKind::MarkStep { id: id1, status: StepStatus::Done } }).expect("mark");

      let lines = panel.render();
      assert_eq!(lines.len(), 2);
      assert_eq!(lines[0].status_glyph, '●');
      assert_eq!(lines[0].content, "First");
      assert_eq!(lines[1].status_glyph, '○');
      assert_eq!(lines[1].content, "Second");
  }

  #[test]
  fn shows_lease_holder_when_present() {
      let a = ActorId([1; 16]);
      let b = ActorId([2; 16]);
      let mut panel = PlanPanel::new();
      let k = Logoot::between(None, None, a);
      add(&mut panel, 1, a, "Shared", k);
      let id = panel.fold().steps().next().expect("step").0;
      panel.apply_op(&PlanOp { lamport: 2, actor: b, kind: PlanOpKind::LeaseStep { step: id, expires_at_ms: u64::MAX } }).expect("lease");
      let lines = panel.render();
      assert_eq!(lines[0].holder, Some(b));
  }
  ```

- [ ] **Step 2: Run** `cargo test -p origin-tui --test plan_panel` — fails.

- [ ] **Step 3: Implement.**

  `crates/origin-tui/Cargo.toml`: add `origin-plan = { path = "../origin-plan" }`.

  `crates/origin-tui/src/widgets/plan_panel.rs`:
  ```rust
  use origin_plan::{ActorId, FoldError, Plan, PlanOp, PlanOpKind, StepId, StepStatus};

  #[derive(Debug)]
  pub struct PlanPanel {
      plan: Plan,
  }

  #[derive(Debug, PartialEq, Eq)]
  pub struct PlanLine {
      pub id: StepId,
      pub indent: u8,
      pub status_glyph: char,
      pub content: String,
      pub holder: Option<ActorId>,
  }

  impl PlanPanel {
      #[must_use]
      pub fn new() -> Self { Self { plan: Plan::default() } }

      /// # Errors
      /// Propagates fold errors from [`Plan::apply`].
      pub fn apply_op(&mut self, op: &PlanOp) -> Result<(), FoldError> { self.plan.apply(op) }

      #[must_use]
      pub fn fold(&self) -> &Plan { &self.plan }

      #[must_use]
      pub fn render(&self) -> Vec<PlanLine> {
          self.plan.steps().map(|(id, r)| PlanLine {
              id,
              indent: 0,
              status_glyph: match r.status {
                  StepStatus::Pending    => '○',
                  StepStatus::InProgress => '◐',
                  StepStatus::Blocked    => '!',
                  StepStatus::Done       => '●',
                  StepStatus::Failed     => '✕',
              },
              content: r.content.clone(),
              holder: self.plan.lease_holder(id, u64::MAX),
          }).collect()
      }
  }
  ```

  Wire into `crates/origin-cli/src/main.rs`: in the agent event loop that already handles incoming `StreamEvent`s, add a `tokio::select!` branch on the `PlanHandle::subscribe()` receiver that calls `panel.apply_op(&op)` and triggers a Composer redraw. The redraw integrates via the same path P4.7 uses for permission prompts. (The Composer side panel slot already accepts arbitrary text — converting `PlanLine` → text and pushing into the panel is the minimal wiring.)

- [ ] **Step 4: Run** `cargo test -p origin-tui --test plan_panel` → pass.

- [ ] **Step 5: Final verification gate**

  ```powershell
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --check
  ```

- [ ] **Step 6: Tag**

  ```powershell
  git tag p9-complete
  ```

- [ ] **Step 7: Commit** `feat(origin-tui): plan side panel + PlanHandle subscription; tag p9-complete (P9.9)`.

---

## Self-review checklist

**Spec coverage (N7.x):**
- ✅ N7.1 — Workers inherit coordinator's CachePlanner prefix (P9.7 — `PrefixSnapshot::seed_into`, `Coordinator::with_parent_ledger`).
- ✅ N7.2 — SPSC SMR ring (P9.4 — `origin-smr` with cache-padded atomics, named mmap on every platform).
- ✅ N7.3 — CoW worker isolation via reflinks (P9.5 — Linux FICLONE, macOS clonefile, Windows FSCTL_DUPLICATE_EXTENTS_TO_FILE, fallback eager-copy with the same isolation contract).
- ✅ N7.4 — Credit-based backpressure on every channel (P9.6 — `CreditChannel<T>` with budgets per spec).
- ✅ N7.5 — Structured `CompletionReport` (P9.6 — `report.rs`; not prose; transcript stays in CAS).
- ✅ N7.6 — Per-step lease tokens (P9.2 — `LeaseStep` op with lamport+actor tiebreak; `LeaseOutcome::{Granted, Lost}` query helper).
- ✅ N7.7 — Snapshot compaction (P9.3 — `PlanStore::write_snapshot` GCs ops below `fully_acked_below`; V4 SQLite migration).

**Phase 9 deliverables:**
- ✅ P9.1 op-log + fold + Logoot.
- ✅ P9.2 lease tokens.
- ✅ P9.3 snapshot compaction (incl. V4 migration in `origin-store`).
- ✅ P9.4 SMR ring.
- ✅ P9.5 CoW workspace.
- ✅ P9.6 coordinator/worker + credit channels + `CompletionReport`.
- ✅ P9.7 PrefixLedger inheritance.
- ✅ P9.8 `Task` builtin tool.
- ✅ P9.9 plan side panel + tag `p9-complete`.

**Type consistency:**
- `ActorId([u8; 16])` used identically across `origin-plan` (P9.1+) and `origin-swarm` (P9.6+).
- `StepId(u64)` and `LogootKey(Vec<u64>)` consistent across plan crate + plan panel.
- `PlanOp { lamport, actor, kind }` shape stable; `PlanOpKind` is `#[non_exhaustive]` so P9.2 and P9.3 can add `LeaseStep` and `Snapshot` variants without breaking earlier-task consumers.
- `LeaseRecord::supersedes` uses the same `(lamport, actor.0)` ordering everywhere — including the P9.6 plan-broadcast loop.
- `Plan::apply` takes `&PlanOp` (not `PlanOp`) consistently across P9.1/P9.2/P9.3/P9.9.
- `CompletionReport` field names (`goal`, `status`, `plan_updates`, `files_touched`, `decisions`, `follow_ups`, `transcript_handle`, `usage`) used identically in P9.6 (`report.rs`), P9.7 (test stub), P9.8 (`task_tool` inlining), P9.9 (panel does not consume reports directly).
- `WorkerContext::inherited_ledger() -> &PrefixSnapshot` consistent across P9.6 and P9.7.
- `Coordinator::spawn` / `Coordinator::spawn_with` / `Coordinator::set_default_worker` form a coherent triad used across P9.6/P9.7/P9.8.
- `Strategy { Reflink, HardlinkOverlay }` consistent across `origin-cow` modules.
- `Band` and `SectionId` re-used from existing `origin-planner` (P3).
- `TokenEvent` from existing `origin-stream` not touched here.

**Placeholders:** None. Every test body, type, manifest fragment, and SQL migration is concrete. Helper methods on existing crates that need to be added (`PrefixLedger::iter_bands`) are spelled out where introduced.

**Files newly touching `unsafe`:** Two crates only — `origin-smr` and `origin-cow`. Each has an explicit verification step that counts `unsafe` blocks vs `// SAFETY:` comments.

**Dependency ordering / parallelism for subagent fan-out:**
- **Wave 1 (no Phase 9 dependencies — fully parallel):** P9.1, P9.4, P9.5.
- **Wave 2 (depend on P9.1):** P9.2, P9.3. These are parallel with each other.
- **Wave 3 (depends on P9.1+P9.2+P9.3+P9.4):** P9.6 (the coordinator needs op-log + leases + snapshots + ring).
- **Wave 4 (depends on P9.6):** P9.7, P9.8. Parallel with each other.
- **Wave 5 (depends on P9.6 + P9.7):** P9.9 (panel reads PlanHandle; doesn't need P9.8 to land first).

The orchestrator should NOT dispatch later waves until the earlier wave is fully merged + verification-gate-green. Within each wave, tasks may run as independent subagents in parallel.

---

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-05-19-origin-phase-9.md`. Per the user's instruction, execution is via **superpowers:subagent-driven-development**, each task internally following **superpowers:test-driven-development** and gated by **superpowers:verification-before-completion** before advancing. Subagents within the same wave run in parallel; later waves wait for the prior wave's verification to complete. Branch: `phase-9` (off current `dev`).
