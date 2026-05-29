// SPDX-License-Identifier: Apache-2.0
//! Refcount table for CAS shards.
//!
//! The actual SQLite schema lives in `origin-store` migrations (V2). This
//! module is a thin typed wrapper: callers pass a `&Connection`, we use
//! parameterised SQL. GC is `dead_hashes` → caller deletes pack entries +
//! removes rows.

use crate::Hash;
use rusqlite::{params, Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Errors returned by [`RefTable`] operations.
#[derive(Debug, Error)]
pub enum RefError {
    /// Underlying sqlite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Attempted to decrement a hash that has no positive refcount.
    #[error("decr below zero for {0}")]
    BelowZero(Hash),
}

/// Typed wrapper over the `cas_refs` table.
#[derive(Debug, Default, Clone, Copy)]
pub struct RefTable;

impl RefTable {
    /// Construct a new handle. The handle itself is zero-sized; all state
    /// lives in the SQLite connection passed to each call.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Increment refcount; inserts a row at 1 if absent.
    ///
    /// # Errors
    /// Propagates sqlite errors.
    pub fn incr(&self, conn: &Connection, h: Hash) -> Result<(), RefError> {
        let now = now_ms();
        conn.execute(
            "INSERT INTO cas_refs (hash, refcount, tier, last_access) \
             VALUES (?1, 1, 0, ?2) \
             ON CONFLICT(hash) DO UPDATE SET refcount = refcount + 1, last_access = ?2",
            params![h.as_bytes().as_slice(), now],
        )?;
        Ok(())
    }

    /// Decrement refcount. Errors if the row is absent or already at zero.
    ///
    /// # Errors
    /// Returns `BelowZero` if no positive count exists; otherwise sqlite errors.
    pub fn decr(&self, conn: &Connection, h: Hash) -> Result<(), RefError> {
        let cur = self.get(conn, h)?;
        match cur {
            None | Some(0) => Err(RefError::BelowZero(h)),
            Some(_) => {
                conn.execute(
                    "UPDATE cas_refs SET refcount = refcount - 1, last_access = ?2 WHERE hash = ?1",
                    params![h.as_bytes().as_slice(), now_ms()],
                )?;
                Ok(())
            }
        }
    }

    /// Read the current count for `h`, or `None` if no row exists.
    ///
    /// # Errors
    /// Propagates sqlite errors.
    pub fn get(&self, conn: &Connection, h: Hash) -> Result<Option<i64>, RefError> {
        let c = conn
            .query_row(
                "SELECT refcount FROM cas_refs WHERE hash = ?1",
                params![h.as_bytes().as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        Ok(c)
    }

    /// Enumerate all hashes with refcount = 0 (GC candidates).
    ///
    /// # Errors
    /// Propagates sqlite errors via the iterator collection.
    pub fn dead_hashes(&self, conn: &Connection) -> Result<impl Iterator<Item = Hash>, RefError> {
        let mut stmt = conn.prepare("SELECT hash FROM cas_refs WHERE refcount = 0")?;
        let rows: Vec<Hash> = stmt
            .query_map([], |r| {
                let bytes: Vec<u8> = r.get(0)?;
                let mut arr = [0u8; 32];
                if bytes.len() == 32 {
                    arr.copy_from_slice(&bytes);
                }
                Ok(Hash::from_bytes(arr))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
