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
//! P9.7 wires in `PrefixLedger` inheritance; P9.8 substitutes the real
//! agent-loop `WorkerFn`; P9.9 wires the TUI panel against the same
//! `PlanHandle`.

use std::collections::HashMap;
use std::sync::Arc;

use origin_planner::PrefixLedger;
use origin_runtime::{spawn_in, TaskClass};
use tokio::sync::{watch, Mutex};
use ulid::Ulid;

use crate::error::SwarmError;
use crate::lifecycle::Lifecycle;
use crate::prefix_inherit::PrefixSnapshot;
use crate::report::CompletionReport;
use crate::rpc::PlanHandle;
use crate::spec::WorkerSpec;
use crate::worker::{default_noop_worker, WorkerContext, WorkerFn};

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
    /// Parent ledger retained so observers can re-snapshot post-builder if
    /// the upstream code mutates a clone before spawn. Currently not exposed
    /// publicly — the cached `parent_snapshot` is what workers actually see.
    parent_ledger: Option<PrefixLedger>,
    /// Eagerly cached `PrefixSnapshot` computed once at
    /// [`Coordinator::with_parent_ledger`] time. Cloned cheaply into every
    /// `WorkerContext::inherited_ledger` on spawn (`Vec<(SectionId, Band)>`
    /// clone — both `Copy` payloads).
    parent_snapshot: Option<PrefixSnapshot>,
}

impl Coordinator {
    /// Construct a new coordinator wrapping `plan` and bound to a logical
    /// `ring_name` (the SMR shared-memory region the coordinator will create
    /// when workers actually need it — P9.6 lazily defers ring creation since
    /// the noop worker never sends).
    #[must_use]
    pub fn new(plan: PlanHandle, ring_name: impl Into<String>) -> Self {
        Self {
            plan,
            ring_name: ring_name.into(),
            workers: Arc::new(Mutex::new(HashMap::new())),
            default_worker: default_noop_worker(),
            last_completion: Arc::new(Mutex::new(None)),
            parent_ledger: None,
            parent_snapshot: None,
        }
    }

    /// Logical SMR ring name (diagnostic accessor).
    #[must_use]
    pub fn ring_name(&self) -> &str {
        &self.ring_name
    }

    /// Builder-style setter for the parent's `PrefixLedger`.
    ///
    /// The snapshot of stable bands (`Frozen` + `Sticky`) is computed
    /// **once, eagerly** here and cached for the lifetime of the
    /// coordinator. Subsequent `spawn`/`spawn_with` calls clone the cached
    /// snapshot into each `WorkerContext` rather than re-walking the
    /// ledger — workers should see a stable inheritance set per coordinator
    /// (N7.1, P9.7).
    #[must_use]
    pub fn with_parent_ledger(mut self, l: PrefixLedger) -> Self {
        self.parent_snapshot = Some(Self::take_prefix_snapshot(&l));
        self.parent_ledger = Some(l);
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
        let id = WorkerId::generate();
        let (lc_tx, lc_rx) = watch::channel(Lifecycle::Spawning);

        let ctx = WorkerContext {
            plan: self.plan.clone(),
            smr_producer: None,
            budget: spec.budget,
            parent_actor: spec.parent_actor,
            spec: spec.clone(),
            inherited_ledger: self.parent_snapshot.clone().unwrap_or_default(),
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
        spawn_in(TaskClass::Critical, async move {
            // We immediately publish `Running` so spawn callers can rely on
            // observing it (the test only awaits `Done` / `Failed`, but
            // future P9.8 paths need the transition to be observable).
            let _ = lc_tx_for_spawn.send(Lifecycle::Running);
            let fut = (worker)(ctx);
            let result = fut.await;
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

    /// Extract a `PrefixSnapshot` from a parent `PrefixLedger`, retaining
    /// only `Frozen` + `Sticky` band entries (N7.1, P9.7).
    ///
    /// Free-function-style: takes a borrow, returns an owned snapshot.
    /// Idempotent and side-effect free.
    #[must_use]
    pub fn take_prefix_snapshot(l: &PrefixLedger) -> PrefixSnapshot {
        PrefixSnapshot::from_ledger(l)
    }
}
