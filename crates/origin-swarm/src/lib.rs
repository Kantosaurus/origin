#![allow(clippy::module_name_repetitions)]
// Public surface intentionally mirrors module names — `WorkerSpec`, `CompletionReport`, etc. are documented vocab callers expect.
#![allow(clippy::future_not_send)] // Channels are generic over `T`; users are responsible for picking `Send` payloads.

//! `origin-swarm` — coordinator/worker protocol + credit channels +
//! `CompletionReport` (Phase 9.6, N7.4, N7.5).
//!
//! The crate wires four pieces together:
//! - [`Coordinator`] dispatches workers and aggregates their reports.
//! - [`PlanHandle`] is the shared, mutex-guarded plan fold every worker
//!   authors against.
//! - [`credit::CreditChannel`] enforces N7.4 backpressure: every channel
//!   carries a `Credit` counter; senders consume on send, receivers issue on
//!   consume.
//! - [`CompletionReport`] is the structured worker → coordinator handoff
//!   (N7.5) — no prose, just `plan_updates`, `files_touched`, `decisions`,
//!   `follow_ups`, a transcript CAS handle, and `Usage` accounting.
//!
//! ## Where each Phase-9 sub-task plugs in
//!
//! - **P9.6 (this crate):** protocol scaffold + noop worker that the
//!   test fixture in `tests/protocol.rs` exercises.
//! - **P9.7 (this crate):** [`prefix_inherit::PrefixSnapshot`] carries the
//!   coordinator's `Frozen`+`Sticky` band entries from
//!   [`origin_planner::PrefixLedger`] into each
//!   [`worker::WorkerContext::inherited_ledger`].
//! - **P9.8:** overrides the default worker via
//!   [`Coordinator::set_default_worker`] with the real agent-loop closure.
//! - **P9.9:** subscribes the TUI plan panel to [`PlanHandle::subscribe`].

pub mod coordinator;
pub mod credit;
pub mod error;
pub mod lifecycle;
pub mod prefix_inherit;
pub mod report;
pub mod rpc;
pub mod spec;
pub mod worker;

pub use coordinator::{Coordinator, WorkerHandle, WorkerId};
pub use error::SwarmError;
pub use lifecycle::Lifecycle;
pub use prefix_inherit::PrefixSnapshot;
pub use report::CompletionReport;
pub use rpc::PlanHandle;
pub use spec::{Budget, DecisionRecord, ReportStatus, TaskRef, Usage, WorkerSpec};
pub use worker::{default_noop_worker, WorkerContext, WorkerFn};
