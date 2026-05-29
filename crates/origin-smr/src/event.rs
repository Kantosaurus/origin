// SPDX-License-Identifier: Apache-2.0
//! `SwarmEvent` — rkyv-archived shared-memory event record.
//!
//! Per Phase 9 plan P9.4 / N7.2 the SPSC ring stores rkyv-archived
//! `SwarmEvent` records so coordinator and worker processes can decode
//! the same bytes without an intermediate serialization layer.
//!
//! The enum is intentionally small: plan-op broadcasts and direct
//! messages carry opaque `Vec<u8>` payloads, deliberately keeping rkyv
//! validation cheap (only the framing is checked here; downstream code
//! validates the payload against its own schema).

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum SwarmEvent {
    /// CRDT plan op fanned out to every subscriber. `actor_bytes` is the
    /// origin actor id; `op_payload` is the serialized `PlanOp` body
    /// (decoded by `origin-plan`, opaque here).
    PlanOpBroadcast {
        lamport: u64,
        actor_bytes: [u8; 16],
        op_payload: Vec<u8>,
    },
    /// Point-to-point message between two swarm members.
    DirectMessage {
        from: [u8; 16],
        to: [u8; 16],
        body: Vec<u8>,
    },
    /// Liveness ping. `now_ms` is the sender's monotonic clock in ms.
    Heartbeat { sender: [u8; 16], now_ms: u64 },
    /// Worker emitted a `CompletionReport`; the report itself is in the
    /// CAS at `report_handle` (32-byte blake3).
    WorkerComplete {
        worker: [u8; 16],
        report_handle: [u8; 32],
    },
}
