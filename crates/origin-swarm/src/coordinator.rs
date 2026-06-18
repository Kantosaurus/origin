// SPDX-License-Identifier: Apache-2.0
//! `Coordinator` — owns workers, dispatches them, awaits their reports
//! (P9.6).
//!
//! The coordinator is intentionally minimal in P9.6:
//! - one in-memory `HashMap<WorkerId, WorkerState>`,
//! - a default `WorkerFn` (defaults to [`crate::worker::default_noop_worker`]),
//! - `tokio::spawn` per worker with a `tokio::sync::watch` for lifecycle
//!   observation,
//! - and a `last_completion` slot test code can poke at without round-tripping
//!   through `await_completion`.
//!
//! P9.8 substitutes the real agent-loop `WorkerFn`; P9.9 wires the TUI panel
//! against the same `PlanHandle`.

use std::collections::HashMap;
use std::sync::Arc;

use origin_runtime::{spawn_in, TaskClass};
use tokio::sync::{watch, Mutex};
use ulid::Ulid;

use crate::admission::AdmissionGate;
use crate::collab::{FileRegistry, Mailbox};
use crate::error::SwarmError;
use crate::lifecycle::Lifecycle;
use crate::report::CompletionReport;
use crate::rpc::PlanHandle;
use crate::spec::WorkerSpec;
use crate::worker::{default_noop_worker, SharedMailboxes, WorkerCollab, WorkerContext, WorkerFn};

/// Env gate for real-time swarm collaboration (WS-L, jcode L238). At
/// coordinator-construction time, every worker spawned by this coordinator is
/// handed a [`WorkerCollab`] over a room-shared registry + mailbox map, so
/// concurrently-running workers notify each other of file shifts. Default-ON;
/// set this to `0`/`false` to disable, which leaves `WorkerContext::collab`
/// `None` ⇒ byte-identical to before this feature existed.
const SWARM_COLLAB_ENV: &str = "ORIGIN_SWARM_COLLAB";

/// Whether real-time swarm collaboration is active.
///
/// Default-ON; the `ORIGIN_SWARM_COLLAB` env var disables it only when
/// explicitly set to `0` or `false`. Exposed so the daemon's per-tool read/edit
/// recorder gates on the exact same condition as the coordinator that builds the
/// room state.
#[must_use]
pub fn collab_enabled() -> bool {
    std::env::var(SWARM_COLLAB_ENV).map_or(true, |v| v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Room-wide collaboration state shared across every worker in one coordinator.
///
/// Built once at [`Coordinator::new`] iff [`SWARM_COLLAB_ENV`] is set. The
/// `registry` tracks which worker read which path; the `mailboxes` map is the
/// live `WorkerId → Mailbox` directory each worker delivers file-shift notices
/// into. Both are `Arc`-shared so every worker sees the same room state.
#[derive(Clone)]
struct RoomCollab {
    registry: Arc<FileRegistry>,
    mailboxes: SharedMailboxes,
}

/// Opaque worker identifier (ULID under the hood).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkerId(u128);

impl WorkerId {
    /// Generate a fresh `WorkerId`.
    #[must_use]
    pub fn generate() -> Self {
        Self(Ulid::new().0)
    }

    /// Underlying numeric value (useful for diagnostics).
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

/// Opaque handle returned by [`Coordinator::spawn`].
///
/// Re-exported as `pub` so callers can hand the handle to
/// [`Coordinator::await_completion`]; the inner ID is intentionally opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerHandle {
    id: WorkerId,
}

impl WorkerHandle {
    /// Underlying worker id.
    #[must_use]
    pub const fn id(&self) -> WorkerId {
        self.id
    }

    /// Construct a handle from a raw id. TEST ONLY — exercises the background-job
    /// registry / orphan-reconcile paths without spawning a real worker. In
    /// production handles come only from [`Coordinator::spawn`].
    #[doc(hidden)]
    #[must_use]
    pub const fn from_raw_for_test(id: u128) -> Self {
        Self { id: WorkerId(id) }
    }
}

/// Per-worker bookkeeping kept inside the coordinator's map.
struct WorkerState {
    lifecycle_rx: watch::Receiver<Lifecycle>,
    /// Per-worker terminal report slot. Populated by the worker task before it
    /// publishes `Lifecycle::Done`; `await_completion` reads it once the
    /// transition is observed. Separate from `Coordinator::last_completion`
    /// (which is a coarse "most recent any-worker" test helper) so two
    /// concurrent workers don't clobber each other's reports.
    report_slot: Arc<Mutex<Option<CompletionReport>>>,
}

/// Swarm coordinator.
///
/// One `Coordinator` per "room" / session. In P9.6 a room is a single Tokio
/// runtime; Phase 11 promotes workers to sandboxed processes.
pub struct Coordinator {
    plan: PlanHandle,
    ring_name: String,
    workers: Arc<Mutex<HashMap<WorkerId, WorkerState>>>,
    default_worker: WorkerFn,
    last_completion: Arc<Mutex<Option<CompletionReport>>>,
    /// Memory-governed admission gate. Defaults to the process-shared gate so
    /// every room draws on one authoritative RAM budget; spawn admits through
    /// it (parking, holding nothing) before launching the worker, so the swarm
    /// runs as many sub-agents as fit and backs off before OOM.
    gate: Arc<AdmissionGate>,
    /// Room-wide real-time collaboration state (WS-L, jcode L238). `Some` only
    /// when `ORIGIN_SWARM_COLLAB` was set at construction; `None` (the default)
    /// ⇒ no worker is handed a collab handle ⇒ byte-identical.
    collab: Option<RoomCollab>,
}

impl Coordinator {
    /// Construct a new coordinator wrapping `plan` and tagged with a logical
    /// `ring_name` (a diagnostic room label; in-process workers communicate
    /// through the shared `PlanHandle`, so no shared-memory region is opened).
    #[must_use]
    pub fn new(plan: PlanHandle, ring_name: impl Into<String>) -> Self {
        // Real-time collaboration is default-ON: build the room state so workers
        // spawned by this coordinator cooperate (file-shift notices). Disabled
        // only when `ORIGIN_SWARM_COLLAB` is explicitly `0`/`false`, which leaves
        // `collab: None` and every spawn byte-identical to before this feature.
        let collab = if collab_enabled() {
            Some(RoomCollab {
                registry: Arc::new(FileRegistry::new()),
                mailboxes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            })
        } else {
            None
        };
        Self {
            plan,
            ring_name: ring_name.into(),
            workers: Arc::new(Mutex::new(HashMap::new())),
            default_worker: default_noop_worker(),
            last_completion: Arc::new(Mutex::new(None)),
            gate: AdmissionGate::shared(),
            collab,
        }
    }

    /// Logical room label this coordinator was tagged with (diagnostic accessor).
    #[must_use]
    pub fn ring_name(&self) -> &str {
        &self.ring_name
    }

    /// Override the memory-admission gate (test injection). Production uses the
    /// process-shared gate from [`Coordinator::new`]; tests pass an isolated
    /// gate built with a [`crate::ScriptedProbe`] so admission is deterministic
    /// without real allocation.
    #[must_use]
    pub fn with_memory_gate(mut self, gate: Arc<AdmissionGate>) -> Self {
        self.gate = gate;
        self
    }

    /// Override the worker used by [`Self::spawn`]. P9.8 calls this with the
    /// real agent-loop closure.
    pub fn set_default_worker(&mut self, w: WorkerFn) {
        self.default_worker = w;
    }

    /// Spawn a worker using the coordinator's current `default_worker`.
    ///
    /// # Errors
    /// Currently infallible (no IO during spawn), but returns `Result` so
    /// future sandbox / process-spawn errors don't break the API.
    pub async fn spawn(&self, spec: WorkerSpec) -> Result<WorkerHandle, SwarmError> {
        let worker = Arc::clone(&self.default_worker);
        self.spawn_with(spec, worker).await
    }

    /// Spawn a worker using an explicit `WorkerFn` (P9.8 entry point).
    ///
    /// # Errors
    /// Currently infallible; reserved for future spawn-time validation.
    pub async fn spawn_with(&self, spec: WorkerSpec, worker: WorkerFn) -> Result<WorkerHandle, SwarmError> {
        self.spawn_inner(spec, worker, None).await
    }

    /// Spawn a worker (default `WorkerFn`) with a live [`WorkerProgress`] sink so
    /// the spawner receives a signal each time the worker starts a tool. Used by
    /// the daemon to drive the TUI's per-agent swarm panel.
    ///
    /// # Errors
    /// Currently infallible; reserved for future spawn-time validation.
    pub async fn spawn_with_progress(
        &self,
        spec: WorkerSpec,
        progress: Option<crate::WorkerProgressTx>,
    ) -> Result<WorkerHandle, SwarmError> {
        let worker = Arc::clone(&self.default_worker);
        self.spawn_inner(spec, worker, progress).await
    }

    async fn spawn_inner(
        &self,
        spec: WorkerSpec,
        worker: WorkerFn,
        progress: Option<crate::WorkerProgressTx>,
    ) -> Result<WorkerHandle, SwarmError> {
        let id = WorkerId::generate();
        let (lc_tx, lc_rx) = watch::channel(Lifecycle::Spawning);

        // Real-time collaboration (WS-L, jcode L238): when the room has collab
        // state, register THIS worker's mailbox in the live shared map *before*
        // spawning (so a sibling that edits a path this worker later reads can
        // find it) and hand the worker its id + the shared registry/map. The
        // map is live behind a `Mutex`, so a worker spawned after this one is
        // still visible for delivery. `None` ⇒ no collab handle ⇒ unchanged.
        let collab = self.collab.as_ref().map(|room| {
            let mailbox = Arc::new(Mailbox::new());
            if let Ok(mut map) = room.mailboxes.lock() {
                map.insert(id, mailbox);
            }
            WorkerCollab {
                worker_id: id,
                registry: Arc::clone(&room.registry),
                mailboxes: Arc::clone(&room.mailboxes),
            }
        });

        let ctx = WorkerContext {
            plan: self.plan.clone(),
            budget: spec.budget,
            parent_actor: spec.parent_actor,
            spec: spec.clone(),
            collab,
            progress,
        };

        let report_slot: Arc<Mutex<Option<CompletionReport>>> = Arc::new(Mutex::new(None));
        {
            let mut map = self.workers.lock().await;
            map.insert(
                id,
                WorkerState {
                    lifecycle_rx: lc_rx,
                    report_slot: Arc::clone(&report_slot),
                },
            );
        }

        let last = Arc::clone(&self.last_completion);
        let lc_tx_for_spawn = lc_tx.clone();
        let per_worker_slot = Arc::clone(&report_slot);
        // When collab is on, drop this worker's read-tracking and mailbox once
        // it exits so a finished worker is never notified and never leaks into
        // another worker's notice set. `None` ⇒ nothing to clean up.
        let collab_cleanup = self.collab.clone();

        // Acquire memory admission BEFORE spawning. A parked admit holds NOTHING
        // (no execution permit, no task), so it can never be the resource a
        // running worker needs — load-bearing for deadlock-freedom. The gate's
        // `>= 1` floor guarantees the first worker always proceeds.
        let ticket = self.gate.admit().await;

        // Worker bodies run in the dedicated `Swarm` lane — NOT `Critical` and
        // NOT `Bulk`. A parent agent holds a `Critical` permit while it awaits a
        // child (`Task` → `await_completion`); a `Critical` child would deadlock
        // once the fixed `Critical` pool is exhausted, and a `Bulk` child would
        // be parked by the `BulkGate` while the parent (Critical) is in flight.
        // `Swarm` has an independent permit pool gated only by the memory
        // `AdmissionGate`, breaking the circular wait while letting concurrency
        // scale with available RAM.
        spawn_in(TaskClass::Swarm, async move {
            // Move the admission ticket into the task: its RAII `Drop` releases
            // the reserve and wakes parked admits on EVERY exit path (return,
            // panic unwind, cancellation), so the gate can never leak a slot.
            let _ticket = ticket;
            // We immediately publish `Running` so spawn callers can rely on
            // observing it (the test only awaits `Done` / `Failed`, but
            // future P9.8 paths need the transition to be observable).
            let _ = lc_tx_for_spawn.send(Lifecycle::Running);
            let fut = (worker)(ctx);
            let result = fut.await;
            // Collab cleanup (WS-L): forget this worker's reads and drop its
            // mailbox so its slot is reclaimed and it cannot be notified after
            // exit. Best-effort: a poisoned mailbox lock is recovered, never
            // propagated. No-op when collab is off.
            if let Some(room) = &collab_cleanup {
                room.registry.forget_worker(id);
                let mut map = room
                    .mailboxes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                map.remove(&id);
            }
            // Always publish `Reporting` before the terminal state so
            // observers can distinguish "still in flight" from "wrapping up".
            let _ = lc_tx_for_spawn.send(Lifecycle::Reporting);
            match result {
                Ok(report) => {
                    {
                        let mut slot = per_worker_slot.lock().await;
                        *slot = Some(report.clone());
                    }
                    {
                        let mut slot = last.lock().await;
                        *slot = Some(report);
                    }
                    let _ = lc_tx_for_spawn.send(Lifecycle::Done);
                }
                Err(e) => {
                    let _ = lc_tx_for_spawn.send(Lifecycle::Failed {
                        reason: e.to_string(),
                    });
                }
            }
        });

        // Hold `lc_tx` alive only inside the spawned task; the receivers we
        // stored in the map keep working as long as that task is running.
        drop(lc_tx);

        Ok(WorkerHandle { id })
    }

    /// Block until `handle`'s worker reaches a terminal state.
    ///
    /// On success returns the worker's `CompletionReport`. On `Failed`
    /// returns `SwarmError::Worker` with the reason published in the
    /// lifecycle channel.
    ///
    /// # Errors
    /// - [`SwarmError::Lifecycle`] if the worker isn't registered or the
    ///   lifecycle channel was dropped without a terminal state.
    /// - [`SwarmError::Worker`] if the worker reported `Failed`.
    pub async fn await_completion(&self, handle: &WorkerHandle) -> Result<CompletionReport, SwarmError> {
        let map = self.workers.lock().await;
        let state = map
            .get(&handle.id)
            .ok_or_else(|| SwarmError::Lifecycle(format!("unknown worker {:032x}", handle.id.value())))?;
        let mut rx = state.lifecycle_rx.clone();
        let slot = Arc::clone(&state.report_slot);
        drop(map);

        loop {
            {
                let snapshot = rx.borrow().clone();
                match snapshot {
                    Lifecycle::Done => break,
                    Lifecycle::Failed { reason } => {
                        return Err(SwarmError::Worker(reason));
                    }
                    _ => {}
                }
            }
            if rx.changed().await.is_err() {
                return Err(SwarmError::Lifecycle(
                    "lifecycle channel closed before terminal state".into(),
                ));
            }
        }

        let report = slot
            .lock()
            .await
            .clone()
            .ok_or_else(|| SwarmError::Lifecycle("done published but report slot empty".into()))?;
        Ok(report)
    }

    /// Non-blocking reap: peek a worker's lifecycle WITHOUT awaiting a change, so
    /// a background reaper can poll detached workers without parking.
    ///
    /// Returns `None` while the worker is still running (not yet terminal). On a
    /// terminal state it returns the same result [`Self::await_completion`]
    /// would: `Some(Ok(report))` on `Done`, `Some(Err(..))` on `Failed`. An
    /// unknown handle (never registered, or already evicted / lost to a restart)
    /// reads as a vanished worker — `Some(Err(Lifecycle))` — so a caller can
    /// reconcile it to a terminal result instead of waiting forever.
    pub async fn try_completion(
        &self,
        handle: &WorkerHandle,
    ) -> Option<Result<CompletionReport, SwarmError>> {
        let map = self.workers.lock().await;
        let Some(state) = map.get(&handle.id) else {
            return Some(Err(SwarmError::Lifecycle(format!(
                "unknown worker {:032x}",
                handle.id.value()
            ))));
        };
        let snapshot = state.lifecycle_rx.borrow().clone();
        let slot = Arc::clone(&state.report_slot);
        drop(map);
        match snapshot {
            Lifecycle::Done => Some(
                slot.lock()
                    .await
                    .clone()
                    .ok_or_else(|| SwarmError::Lifecycle("done published but report slot empty".into())),
            ),
            Lifecycle::Failed { reason } => Some(Err(SwarmError::Worker(reason))),
            _ => None,
        }
    }

    /// Remove a reaped worker's state from the registry — the `workers` map is
    /// otherwise never pruned, so a long session that fans out many sub-agents
    /// would accumulate dead entries. Call after reaping a terminal worker
    /// (`try_completion`/`await_completion`). Idempotent for an unknown id.
    pub async fn evict(&self, handle: &WorkerHandle) {
        self.workers.lock().await.remove(&handle.id);
    }

    /// Test-only helper: returns a clone of the most recently completed
    /// worker's report.
    ///
    /// Not `#[cfg(test)]` because P9.8 cross-crate tests need to call it
    /// without enabling a feature flag. Treat as test-only in production
    /// code — the slot is overwritten by every successful completion, so it
    /// is not a reliable channel for real consumers.
    #[must_use]
    pub fn last_completion_for_test(&self) -> Option<CompletionReport> {
        self.last_completion.try_lock().ok().and_then(|g| g.clone())
    }
}
