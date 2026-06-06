// SPDX-License-Identifier: Apache-2.0
//! CAS-backed code-graph index (P7.3, N6.7).
//!
//! Nodes and edges live as small `SQLite` rows that point at CAS-stored
//! signature, body, and evidence blobs. Insert is content-deduplicating:
//! identical signature bytes from two files collapse to one CAS handle,
//! which we then index on so callers can fan-out from a signature to every
//! file that declares it.
//!
//! `EntityId` is a deterministic blake3 hash over `(kind, name, file_path,
//! range_start)` — the same source span in the same file always lands on
//! the same row, which lets `insert_node` upsert by primary key.

use std::time::{SystemTime, UNIX_EPOCH};

use origin_cas::{Hash, Store as CasStore, StoreError as CasError};
use origin_store::{Store as SqlStore, StoreError as StoreErr};
use rusqlite::params;
use thiserror::Error;

use crate::record::{CodeNodeRecord, Confidence, ParseConfidenceError};

/// Stable 32-byte identity for a code-graph node.
///
/// Derived as `blake3(kind || 0 || name || 0 || file_path || 0 ||
/// range_start_le_bytes)`. The trailing zero separators prevent two distinct
/// `(name, file_path)` pairs from colliding when one is a prefix of the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId(pub [u8; 32]);

impl EntityId {
    /// Borrow the raw 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One SQL row out of `code_nodes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRow {
    pub entity_id: EntityId,
    pub kind: String,
    pub name: String,
    pub file_path: String,
    pub signature_handle: [u8; 32],
    pub body_handle: [u8; 32],
}

/// One SQL row out of `code_edges`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRow {
    pub from: EntityId,
    pub to: EntityId,
    pub kind: String,
    pub confidence: Confidence,
    pub evidence_handle: [u8; 32],
}

/// Errors surfaced by [`CodeGraphIndex`].
// `IndexError` matches the Phase 7 plan's public API; the `Index` prefix
// disambiguates against other crate errors and is intentional.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("cas: {0}")]
    Cas(#[from] CasError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Store(#[from] StoreErr),
    #[error("malformed confidence column: {0}")]
    Confidence(#[from] ParseConfidenceError),
    #[error("malformed entity id: expected 32 bytes, got {0}")]
    EntityIdShape(usize),
    #[error("malformed cas handle: expected 32 bytes, got {0}")]
    HandleShape(usize),
}

/// CAS-backed code-graph index. Holds an [`origin_cas::Store`] for signature /
/// body / evidence blobs and an [`origin_store::Store`] for the `SQLite` rows.
// `CodeGraphIndex` matches the Phase 7 plan's public API.
#[allow(clippy::module_name_repetitions)]
pub struct CodeGraphIndex {
    cas: CasStore,
    sql: SqlStore,
}

impl CodeGraphIndex {
    /// Construct an index over an existing CAS + `SQLite` store pair. The
    /// `SQLite` store must have run migration V3 (which the
    /// [`origin_store::Store::open`] refinery runner does automatically).
    #[must_use]
    pub const fn new(cas: CasStore, sql: SqlStore) -> Self {
        Self { cas, sql }
    }

    /// Upsert a code node. Signature & body are CAS-put; the `SQLite` row is
    /// inserted with `ON CONFLICT DO UPDATE` keyed on `entity_id`.
    ///
    /// # Errors
    /// Propagates CAS write errors and `SQLite` errors.
    pub fn insert_node(&mut self, rec: &CodeNodeRecord) -> Result<EntityId, IndexError> {
        let sig_hash = self.cas.put(&rec.signature)?;
        let body_hash = self.cas.put(&rec.body)?;
        let entity = derive_entity_id(rec);
        let sig_bytes = *sig_hash.as_bytes();
        let body_bytes = *body_hash.as_bytes();
        let kind = rec.kind.as_str();
        let lang = rec.language.as_discriminant();
        let range_start = i64::try_from(rec.range.start).unwrap_or(i64::MAX);
        let range_end = i64::try_from(rec.range.end).unwrap_or(i64::MAX);
        let last_seen = epoch_ms();

        self.sql.with_conn(|conn| {
            conn.execute(
                "INSERT INTO code_nodes (
                    entity_id, kind, name, language, file_path,
                    range_start, range_end, signature_handle, body_handle, last_seen
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(entity_id) DO UPDATE SET
                    kind = excluded.kind,
                    name = excluded.name,
                    language = excluded.language,
                    file_path = excluded.file_path,
                    range_start = excluded.range_start,
                    range_end = excluded.range_end,
                    signature_handle = excluded.signature_handle,
                    body_handle = excluded.body_handle,
                    last_seen = excluded.last_seen",
                params![
                    &entity.0[..],
                    kind,
                    rec.name,
                    lang,
                    rec.file_path,
                    range_start,
                    range_end,
                    &sig_bytes[..],
                    &body_bytes[..],
                    last_seen,
                ],
            )?;
            Ok(())
        })?;
        Ok(entity)
    }

    /// Look up every node whose `signature_handle` equals `Hash::of(sig)`.
    ///
    /// # Errors
    /// Propagates `SQLite` errors and surfaces malformed BLOB shapes.
    pub fn nodes_by_signature(&self, sig: &[u8]) -> Result<Vec<NodeRow>, IndexError> {
        let handle = *Hash::of(sig).as_bytes();
        let rows = self.sql.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
                 FROM code_nodes WHERE signature_handle = ?1",
            )?;
            let it = stmt.query_map([&handle[..]], |row| {
                let entity: Vec<u8> = row.get(0)?;
                let kind: String = row.get(1)?;
                let name: String = row.get(2)?;
                let file_path: String = row.get(3)?;
                let sig_h: Vec<u8> = row.get(4)?;
                let body_h: Vec<u8> = row.get(5)?;
                Ok((entity, kind, name, file_path, sig_h, body_h))
            })?;
            it.collect::<rusqlite::Result<Vec<_>>>()
        })?;

        let mut out = Vec::with_capacity(rows.len());
        for (entity, kind, name, file_path, sig_h, body_h) in rows {
            out.push(NodeRow {
                entity_id: EntityId(to32(&entity)?),
                kind,
                name,
                file_path,
                signature_handle: to32(&sig_h)?,
                body_handle: to32(&body_h)?,
            });
        }
        Ok(out)
    }

    /// Insert (or replace) an edge between two nodes. Evidence bytes are
    /// CAS-stored and the resulting handle is what lands in `code_edges`.
    ///
    /// # Errors
    /// Propagates CAS and `SQLite` errors.
    pub fn insert_edge(
        &mut self,
        from: EntityId,
        to: EntityId,
        kind: &str,
        confidence: Confidence,
        evidence: &[u8],
    ) -> Result<(), IndexError> {
        let ev_hash = self.cas.put(evidence)?;
        let ev_bytes = *ev_hash.as_bytes();
        let conf = confidence.as_str();
        self.sql.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO code_edges
                 (from_id, to_id, kind, confidence, evidence_handle)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![&from.0[..], &to.0[..], kind, conf, &ev_bytes[..]],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Run a closure against the underlying `SQLite` connection.
    ///
    /// Used by the typed query DSL (`crate::query`) to issue ad-hoc reads
    /// without exposing the store handle.
    ///
    /// # Errors
    /// Propagates `SQLite` errors from the closure.
    pub fn with_store<R, F>(&self, f: F) -> rusqlite::Result<R>
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<R>,
    {
        self.sql.with_conn(f)
    }

    /// Fetch every outgoing edge from `from`.
    ///
    /// # Errors
    /// Propagates `SQLite` errors and surfaces malformed BLOB shapes /
    /// unknown confidence tags.
    pub fn edges_from(&self, from: EntityId) -> Result<Vec<EdgeRow>, IndexError> {
        let rows = self.sql.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT from_id, to_id, kind, confidence, evidence_handle
                 FROM code_edges WHERE from_id = ?1",
            )?;
            let it = stmt.query_map([&from.0[..]], |row| {
                let f: Vec<u8> = row.get(0)?;
                let t: Vec<u8> = row.get(1)?;
                let kind: String = row.get(2)?;
                let conf: String = row.get(3)?;
                let ev: Vec<u8> = row.get(4)?;
                Ok((f, t, kind, conf, ev))
            })?;
            it.collect::<rusqlite::Result<Vec<_>>>()
        })?;

        let mut out = Vec::with_capacity(rows.len());
        for (f, t, kind, conf, ev) in rows {
            out.push(EdgeRow {
                from: EntityId(to32(&f)?),
                to: EntityId(to32(&t)?),
                kind,
                confidence: Confidence::from_str(&conf)?,
                evidence_handle: to32(&ev)?,
            });
        }
        Ok(out)
    }
}

fn derive_entity_id(rec: &CodeNodeRecord) -> EntityId {
    let mut h = blake3::Hasher::new();
    h.update(rec.kind.as_str().as_bytes());
    h.update(&[0]);
    h.update(rec.name.as_bytes());
    h.update(&[0]);
    h.update(rec.file_path.as_bytes());
    h.update(&[0]);
    // `usize` → `u64` is infallible on 32/64-bit targets.
    let start = u64::try_from(rec.range.start).unwrap_or(u64::MAX);
    h.update(&start.to_le_bytes());
    EntityId(*h.finalize().as_bytes())
}

fn to32(bytes: &[u8]) -> Result<[u8; 32], IndexError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| IndexError::HandleShape(bytes.len()))
}

fn epoch_ms() -> i64 {
    // Wall-clock is allowed to go backwards across boots; clamp to 0 on
    // failure rather than panic.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
