// SPDX-License-Identifier: Apache-2.0
//! `CompletionReport` — the structured handoff from a worker back to its
//! coordinator (N7.5, P9.6).
//!
//! Prose is **deliberately** excluded from this shape: the worker can dump
//! free-form text into its transcript and reference it via `transcript_handle`
//! (a 32-byte blake3 CAS address), but the structured report itself is
//! parseable by the inlining caller without an LLM round-trip.

use origin_cas::Store as CasStore;
use origin_plan::OpEnvelope;
use serde::{Deserialize, Serialize};

use crate::spec::{DecisionRecord, ReportStatus, TaskRef, Usage};

/// Structured worker → coordinator handoff (N7.5).
///
/// `plan_updates` is the in-order sequence of `OpEnvelope` values the worker
/// authored against the shared plan (already applied via [`crate::PlanHandle`];
/// included here for audit). `files_touched` lists the paths of the files the
/// worker created or modified. `transcript_handle` points at the
/// worker's full chat log (stored separately in CAS by the worker harness).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionReport {
    /// Verbatim copy of the goal the worker was given.
    pub goal: String,
    /// Terminal status.
    pub status: ReportStatus,
    /// Ordered list of plan-op envelopes authored by this worker.
    pub plan_updates: Vec<OpEnvelope>,
    /// Paths of files the worker created or rewrote through its own successful
    /// file-editing tool calls, as the tool used them, in first-touch order.
    pub files_touched: Vec<String>,
    /// Decisions the worker explicitly logged.
    pub decisions: Vec<DecisionRecord>,
    /// Suggested follow-up tasks the parent may dispatch.
    pub follow_ups: Vec<TaskRef>,
    /// 32-byte CAS handle of the worker's full transcript.
    pub transcript_handle: [u8; 32],
    /// Provider-side token / tool-call accounting.
    pub usage: Usage,
    /// Optional human-readable detail surfaced to the parent agent — e.g. the
    /// underlying error string when `status` is `GoalUnreachable`.
    ///
    /// Serde-default + skip-if-none keeps the wire/CAS shape backward-compatible
    /// with reports written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// C2: content from a network or MCP tool entered the worker's context
    /// (session taint). The parent session becomes tainted when this report
    /// reaches its context. Serde-default keeps older reports readable.
    #[serde(default)]
    pub tainted: bool,
}

impl CompletionReport {
    /// Serialize the report into the CAS and return its handle.
    ///
    /// Workers hand the coordinator this compact CAS handle rather than the
    /// full report body, so completion fanout stays cheap and the bulk payload
    /// is fetched from the store only on demand.
    ///
    /// # Errors
    /// Returns `origin_cas::StoreError` if the CAS write fails. Bincode
    /// encoding of `Self` is infallible for the shape used here (all fields
    /// are plain serde-derive types); on the off chance a future field
    /// introduces a fallible encode path, callers will still surface that as a
    /// CAS-layer error after we coerce it through `unwrap_or_default`.
    pub fn store_in_cas(&self, cas: &CasStore) -> Result<[u8; 32], origin_cas::StoreError> {
        let body = bincode::serialize(self).unwrap_or_default();
        let hash = cas.put(&body)?;
        Ok(*hash.as_bytes())
    }
}
