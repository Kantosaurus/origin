//! Worker abstraction (P9.6).
//!
//! A `WorkerFn` is a `Pin<Box<dyn Future<...>>>`-returning closure. The
//! coordinator builds the `WorkerContext` and hands it to the closure inside a
//! `tokio::spawn`. The default closure ([`default_noop_worker`]) is the P9.6
//! placeholder that simply returns `ReportStatus::Completed`; P9.8 replaces
//! it via `Coordinator::set_default_worker`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use origin_plan::ActorId;
use origin_planner::{Band, PrefixLedger, SectionId};
use origin_smr::Ring;

use crate::error::SwarmError;
use crate::report::CompletionReport;
use crate::rpc::PlanHandle;
use crate::spec::{Budget, ReportStatus, Usage, WorkerSpec};

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
    /// Coordinator's `PrefixLedger` snapshot at spawn time (N7.1, fleshed out
    /// in P9.7).
    pub inherited_ledger: PrefixSnapshot,
}

impl WorkerContext {
    /// Read-only access to the inherited `PrefixLedger` snapshot.
    #[must_use]
    pub const fn inherited_ledger(&self) -> &PrefixSnapshot {
        &self.inherited_ledger
    }
}

/// Coordinator → worker `PrefixLedger` snapshot (N7.1, placeholder in P9.6).
///
/// P9.6 ships an **empty** snapshot so the protocol surface compiles and the
/// coordinator can already pass *something*; P9.7 will fill in the real seed
/// extraction (`Frozen` + `Sticky` band entries). The `entries` field uses
/// `pub(crate)` so P9.7 can extend it without breaking semver — public
/// extension is by methods, not direct field access.
#[derive(Debug, Clone, Default)]
pub struct PrefixSnapshot {
    pub(crate) entries: Vec<(SectionId, Band)>,
}

impl PrefixSnapshot {
    /// Empty snapshot — the P9.6 default. P9.7 will add a constructor that
    /// pulls real seeds out of a parent `PrefixLedger`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of inherited `(section, band)` entries. Always zero in P9.6.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no entries have been seeded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Seed `ledger` with every inherited `(section, band)` pair. No-op in
    /// P9.6 (snapshot is always empty); P9.7 fills in the real seeding logic.
    pub fn seed_into(&self, ledger: &mut PrefixLedger) {
        for (id, band) in &self.entries {
            ledger.record_band(*id, *band);
        }
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
