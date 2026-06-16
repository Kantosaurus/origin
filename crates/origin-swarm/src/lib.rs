// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::module_name_repetitions)]
// Public surface intentionally mirrors module names — `WorkerSpec`, `CompletionReport`, etc. are documented vocab callers expect.
#![allow(clippy::future_not_send)] // Channels are generic over `T`; users are responsible for picking `Send` payloads.

//! `origin-swarm` — coordinator/worker protocol + `CompletionReport`
//! (Phase 9.6, N7.5).
//!
//! The crate wires three pieces together:
//! - [`Coordinator`] dispatches workers and aggregates their reports.
//! - [`PlanHandle`] is the shared, mutex-guarded plan fold every worker
//!   authors against.
//! - [`CompletionReport`] is the structured worker → coordinator handoff
//!   (N7.5) — no prose, just `plan_updates`, `files_touched`, `decisions`,
//!   `follow_ups`, a transcript CAS handle, and `Usage` accounting.
//!
//! ## Where each Phase-9 sub-task plugs in
//!
//! - **P9.6 (this crate):** protocol scaffold + noop worker that the
//!   test fixture in `tests/protocol.rs` exercises.
//! - **P9.8:** overrides the default worker via
//!   [`Coordinator::set_default_worker`] with the real agent-loop closure.
//! - **P9.9:** subscribes the TUI plan panel to [`PlanHandle::subscribe`].

pub mod admission;
pub mod collab;
pub mod coordinator;
pub mod error;
pub mod lifecycle;
pub mod report;
pub mod rpc;
pub mod spec;
pub mod team;
pub mod worker;

pub use admission::{
    default_probe, AdmissionGate, AdmissionTicket, GateCfg, MemReading, MemoryProbe, NullProbe, ScriptedProbe,
};
pub use collab::{FileRegistry, FileShiftNotice, Mailbox, Message, MsgScope};
pub use coordinator::{collab_enabled, Coordinator, WorkerHandle, WorkerId};
pub use error::SwarmError;
pub use lifecycle::Lifecycle;
pub use report::CompletionReport;
pub use rpc::PlanHandle;
pub use spec::{Budget, DecisionRecord, McpServerSpec, ReportStatus, TaskRef, Usage, WorkerSpec};
pub use team::{
    report_summary, MissionEntry, MissionEvent, MissionLog, Team, TeamError, TeamEvent, TeamRegistry,
    Teammate, TeammateStatus,
};
pub use worker::{default_noop_worker, SharedMailboxes, WorkerCollab, WorkerContext, WorkerFn};
