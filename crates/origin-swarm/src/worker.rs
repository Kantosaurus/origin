// SPDX-License-Identifier: Apache-2.0
//! Worker abstraction (P9.6).
//!
//! A `WorkerFn` is a `Pin<Box<dyn Future<...>>>`-returning closure. The
//! coordinator builds the `WorkerContext` and hands it to the closure inside a
//! `tokio::spawn`. The default closure ([`default_noop_worker`]) is the P9.6
//! placeholder that simply returns `ReportStatus::Completed`; P9.8 replaces
//! it via `Coordinator::set_default_worker`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use origin_plan::ActorId;
use origin_smr::Ring;

use crate::collab::{FileRegistry, Mailbox};
use crate::coordinator::WorkerId;
use crate::error::SwarmError;
use crate::prefix_inherit::PrefixSnapshot;
use crate::report::CompletionReport;
use crate::rpc::PlanHandle;
use crate::spec::{Budget, ReportStatus, Usage, WorkerSpec};

/// Room-wide shared map `WorkerId → Mailbox` (WS-L, jcode L238).
///
/// Wrapped in a `std::sync::Mutex` (not a tokio one — every access is a quick,
/// non-await insert/get) so that a worker spawned *after* this one is still
/// visible to it for file-shift delivery. All workers in one room share a
/// single `Arc`.
pub type SharedMailboxes = Arc<Mutex<HashMap<WorkerId, Arc<Mailbox>>>>;

/// Real-time swarm-collaboration handle handed to a worker via its
/// [`WorkerContext`] (WS-L, jcode L238).
///
/// Populated by the [`Coordinator`](crate::Coordinator) **only** when the
/// process env `ORIGIN_SWARM_COLLAB` is set at coordinator-construction time;
/// otherwise [`WorkerContext::collab`] is `None` and the daemon's behavior is
/// byte-identical. The worker turns this into the daemon's task-local collab
/// context so the per-tool hook can record reads/edits and deliver file-shift
/// notices.
#[derive(Clone)]
pub struct WorkerCollab {
    /// This worker's own id (the reader/editor in every registry call).
    pub worker_id: WorkerId,
    /// Room-wide file-read registry shared by every worker in the room.
    pub registry: Arc<FileRegistry>,
    /// Room-wide, live `WorkerId → Mailbox` map. Live (behind a `Mutex`) so a
    /// later-spawned sibling is visible for delivery.
    pub mailboxes: SharedMailboxes,
}

/// Coordinator-provided context for a single worker.
///
/// Cloning is intentionally not supported: the worker owns its slot for the
/// duration of the spawn. The plan handle inside is cheap to clone if the
/// worker needs to fan it out internally.
#[derive(Clone)]
pub struct WorkerContext {
    /// Shared plan handle (workers see and author plan ops through this).
    pub plan: PlanHandle,
    /// SMR producer ring. `None` in P9.6 noop tests; P9.8 populates it.
    pub smr_producer: Option<Arc<Ring>>,
    /// Resource ceiling.
    pub budget: Budget,
    /// Parent's `ActorId` — the worker uses this to thread Lamport ordering.
    pub parent_actor: ActorId,
    /// Verbatim spec the worker was launched with.
    pub spec: WorkerSpec,
    /// Coordinator's `PrefixLedger` snapshot at spawn time — populated with
    /// the parent's `Frozen` + `Sticky` band entries (N7.1, P9.7). Workers
    /// call [`PrefixSnapshot::seed_into`] against a fresh `PrefixLedger` on
    /// their first turn to reuse the coordinator's stable prefix bytes.
    pub inherited_ledger: PrefixSnapshot,
    /// Real-time swarm-collaboration handle (WS-L, jcode L238). `Some` only
    /// when the coordinator was built with `ORIGIN_SWARM_COLLAB` set; `None`
    /// (the default) ⇒ the worker scopes no collab context ⇒ byte-identical.
    pub collab: Option<WorkerCollab>,
}

impl WorkerContext {
    /// Read-only access to the inherited `PrefixLedger` snapshot.
    #[must_use]
    pub const fn inherited_ledger(&self) -> &PrefixSnapshot {
        &self.inherited_ledger
    }
}

/// Worker future: takes a `WorkerContext`, returns a `CompletionReport`.
pub type WorkerFn = Arc<
    dyn Fn(WorkerContext) -> Pin<Box<dyn Future<Output = Result<CompletionReport, SwarmError>> + Send>>
        + Send
        + Sync,
>;

/// Default worker used by `Coordinator::spawn` when no explicit `WorkerFn`
/// has been registered. Emits no plan ops and returns `Completed` immediately.
#[must_use]
pub fn default_noop_worker() -> WorkerFn {
    Arc::new(|ctx: WorkerContext| {
        Box::pin(async move {
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: Usage::default(),
            })
        })
    })
}
