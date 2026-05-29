// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::module_name_repetitions)]
//! `SQLite` + CAS persistence facade for the plan op-log (P9.3, N7.7).
//!
//! [`PlanStore`] is the bridge between the in-memory CRDT
//! ([`crate::plan::Plan`] / [`crate::fold::fold`]) and the on-disk substrate
//! provided by `origin-store` (the V4 `plan_ops` + `plan_snapshots` tables)
//! and `origin-cas` (the snapshot bodies).
//!
//! Compaction contract (N7.7):
//! 1. `append_op` durably records each [`crate::ops::OpEnvelope`] keyed by
//!    `(lamport, actor)`; replay-safe via `INSERT OR IGNORE`.
//! 2. `write_snapshot` stores the body in the CAS *before* the SQL txn opens,
//!    then atomically inserts the `plan_snapshots` row and deletes every
//!    `plan_ops` row with `lamport < snapshot.fully_acked_below`. A crash
//!    after CAS but before the txn commits leaves an orphan CAS body — safe,
//!    because the GC half of the operation hasn't fired.
//! 3. `load_log` walks `plan_ops` ordered by `(lamport, actor)` — the apply
//!    order the fold expects.
//! 4. `load_latest_snapshot` materialises the latest `plan_snapshots` row
//!    into a hydrated [`crate::plan::Plan`] by going straight to the CAS,
//!    bypassing the fold entirely.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use origin_cas::{Hash, Store as CasStore};
use origin_store::Store as SqlStore;
use rusqlite::params;

use crate::ops::OpEnvelope;
use crate::plan::{Plan, SnapshotError};
use crate::snapshot::Snapshot;

/// Errors returned by [`PlanStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum PlanStoreError {
    /// Underlying `SQLite` error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// CAS-layer error from `origin-cas`.
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
    /// Snapshot body decode failure.
    #[error("snapshot decode: {0}")]
    Decode(String),
    /// Bincode encode/decode failure on an op envelope.
    #[error("bincode: {0}")]
    Bincode(String),
    /// Latest snapshot row references a CAS handle that is missing.
    #[error("snapshot body {0} missing from CAS")]
    MissingSnapshotBody(String),
}

impl From<SnapshotError> for PlanStoreError {
    fn from(e: SnapshotError) -> Self {
        Self::Decode(e.to_string())
    }
}

impl From<bincode::Error> for PlanStoreError {
    fn from(e: bincode::Error) -> Self {
        Self::Bincode(e.to_string())
    }
}

/// Plan-layer persistence facade.
///
/// Owns shared handles to the SQL and CAS stores; both are `Arc`-backed so
/// the higher-level swarm coordinator (P9.6) can hand them out to workers
/// without re-opening.
pub struct PlanStore {
    sql: Arc<SqlStore>,
    cas: Arc<CasStore>,
}

impl PlanStore {
    /// Open a new facade over the given backing stores.
    ///
    /// # Errors
    /// Currently infallible, but returns `Result` so future schema-version
    /// checks can be added without an API break.
    pub const fn open(sql: Arc<SqlStore>, cas: Arc<CasStore>) -> Result<Self, PlanStoreError> {
        Ok(Self { sql, cas })
    }

    /// Append `op` to the durable op log. Idempotent: an envelope already
    /// present (same `(lamport, actor)`) is silently ignored.
    ///
    /// # Errors
    /// Returns `PlanStoreError::Bincode` if `op` cannot be serialised, or
    /// `PlanStoreError::Sqlite` on a database error.
    pub fn append_op(&self, op: &OpEnvelope) -> Result<(), PlanStoreError> {
        let body = bincode::serialize(op)?;
        let lamport_signed = i64::try_from(op.lamport.value()).unwrap_or(i64::MAX);
        let actor_bytes = op.actor.value().to_le_bytes();
        let kind = op.op.kind_tag().to_owned();
        self.sql.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO plan_ops (lamport, actor, op_kind, body) VALUES (?1, ?2, ?3, ?4)",
                params![lamport_signed, actor_bytes.as_slice(), kind, body.as_slice()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Load every op surviving the latest snapshot's GC pass, in canonical
    /// `(lamport, actor)` order.
    ///
    /// When no snapshot exists, returns every op. When a snapshot exists, the
    /// GC half of `write_snapshot` has already removed ops below
    /// `fully_acked_below`; we simply `ORDER BY (lamport, actor)` and stream.
    ///
    /// # Errors
    /// Returns `PlanStoreError::Sqlite` on a database error or
    /// `PlanStoreError::Bincode` if a row cannot be decoded.
    pub fn load_log(&self) -> Result<Vec<OpEnvelope>, PlanStoreError> {
        let rows: Vec<Vec<u8>> = self.sql.with_conn(|c| {
            let mut stmt = c.prepare("SELECT body FROM plan_ops ORDER BY lamport, actor")?;
            let iter = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
            let mut out = Vec::new();
            for r in iter {
                out.push(r?);
            }
            Ok(out)
        })?;
        let mut envs = Vec::with_capacity(rows.len());
        for body in rows {
            let env: OpEnvelope = bincode::deserialize(&body)?;
            envs.push(env);
        }
        Ok(envs)
    }

    /// Persist a snapshot: store `body` in the CAS, write a `plan_snapshots`
    /// row, and GC every `plan_ops` row with `lamport < fully_acked_below`.
    ///
    /// The CAS write happens *outside* the SQL transaction so we never hold
    /// the `SQLite` write lock across a potentially long blob append. If the
    /// process crashes between the CAS write and the txn commit the snapshot
    /// body is orphaned in the CAS but `plan_ops` is intact — re-running the
    /// snapshot is safe (CAS dedupes by hash and the txn commits as a unit).
    ///
    /// # Errors
    /// Returns `PlanStoreError::Cas` if the CAS write fails or
    /// `PlanStoreError::Sqlite` on any database error.
    pub fn write_snapshot(&self, snapshot: &Snapshot, body: &[u8]) -> Result<(), PlanStoreError> {
        // Step 1 (outside txn): make sure the body lives in the CAS.
        let _ = self.cas.put(body)?;

        // Step 2 (inside txn): atomically install the snapshot row + GC.
        let seq_signed = i64::try_from(snapshot.seq).unwrap_or(i64::MAX);
        let acked_signed = i64::try_from(snapshot.fully_acked_below).unwrap_or(i64::MAX);
        let now_ms_signed: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0);
        self.sql.with_conn(|c| {
            // Manual txn — `with_conn` exposes a raw `&Connection` so we use
            // `execute_batch` with savepoints? No — `execute` calls are
            // already wrapped in implicit txns under WAL. Use `BEGIN` /
            // `COMMIT` to make the two writes atomic.
            c.execute("BEGIN IMMEDIATE", [])?;
            let r = (|| -> rusqlite::Result<()> {
                c.execute(
                    "INSERT OR REPLACE INTO plan_snapshots \
                       (seq, state_handle, fully_acked_below, created_at_unix_ms) \
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        seq_signed,
                        snapshot.state_handle.as_slice(),
                        acked_signed,
                        now_ms_signed,
                    ],
                )?;
                c.execute("DELETE FROM plan_ops WHERE lamport < ?1", params![acked_signed])?;
                Ok(())
            })();
            match r {
                Ok(()) => {
                    c.execute("COMMIT", [])?;
                    Ok(())
                }
                Err(e) => {
                    // Best-effort rollback; surface the original error.
                    let _ = c.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })?;
        Ok(())
    }

    /// Load the most recent snapshot + the hydrated [`Plan`] it points to.
    ///
    /// Returns `Ok(None)` when no snapshot has been written. Otherwise
    /// dereferences `state_handle` in the CAS and decodes the body via
    /// [`Plan::deserialize_snapshot`].
    ///
    /// # Errors
    /// Returns `PlanStoreError::Sqlite` on a database error,
    /// `PlanStoreError::Cas` on a CAS-read failure,
    /// `PlanStoreError::MissingSnapshotBody` if the referenced handle has no
    /// CAS entry, or `PlanStoreError::Decode` if the body fails to decode.
    pub fn load_latest_snapshot(&self) -> Result<Option<(Snapshot, Plan)>, PlanStoreError> {
        let row: Option<(i64, Vec<u8>, i64)> = self.sql.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, state_handle, fully_acked_below \
                 FROM plan_snapshots ORDER BY seq DESC LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            if let Some(r) = rows.next()? {
                Ok(Some((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                )))
            } else {
                Ok(None)
            }
        })?;
        let Some((seq, handle_bytes, acked)) = row else {
            return Ok(None);
        };
        let handle_arr: [u8; 32] = handle_bytes
            .as_slice()
            .try_into()
            .map_err(|_| PlanStoreError::Decode("snapshot state_handle is not 32 bytes".to_owned()))?;
        let hash = Hash::from_bytes(handle_arr);
        let body = self
            .cas
            .get(hash)?
            .ok_or_else(|| PlanStoreError::MissingSnapshotBody(hash.to_string()))?;
        let plan = Plan::deserialize_snapshot(&body)?;
        // `seq` and `acked` came in as i64 (SQLite native); convert back.
        // Negative values would indicate corruption — fall back to 0 rather
        // than panicking so the error surfaces at a higher layer if needed.
        let snap = Snapshot {
            seq: u64::try_from(seq).unwrap_or(0),
            state_handle: handle_arr,
            fully_acked_below: u64::try_from(acked).unwrap_or(0),
        };
        Ok(Some((snap, plan)))
    }
}
