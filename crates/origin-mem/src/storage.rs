//! Persistent `MemoryStore`: `SQLite` rows + CAS body blobs.
//!
//! Memories are stored with their quantised vector inline (centroid + 384 i8
//! deltas), body bytes in CAS, and tags encoded as a 128-bit bitset over the
//! `mem_tags` dictionary table.  A singleton `mem_quantizer` row holds the
//! serialised [`Quantizer`] so it survives restarts.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bitvec::prelude::*;
use parking_lot::RwLock;
use rusqlite::{params, OptionalExtension};
use ulid::Ulid;

use crate::quantizer::{EncodedVector, Quantizer};
use origin_cas::Store as CasStore;
use origin_store::Store as SqlStore;

/// Stable public identity for a stored memory.
pub type MemoryId = Ulid;

/// Relationship kind between two memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    RelatedTo = 0,
    Supersedes = 1,
    Contradicts = 2,
}

/// Full record returned by [`MemoryStore::get`] / [`MemoryStore::iter_all`].
#[derive(Debug, Clone)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub encoded: EncodedVector,
    /// 32-byte CAS blake3 hash.
    pub body_handle: [u8; 32],
    /// At most 64 UTF-8 bytes of the body, truncated on a codepoint boundary.
    pub body_preview: String,
    /// Tag names resolved from the 128-bit bitset + `mem_tags` dictionary.
    pub tags: Vec<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub superseded_by: Option<MemoryId>,
    pub cluster_priority: f32,
}

/// Errors returned by [`MemoryStore`] operations.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sql: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
    #[error("ulid: {0}")]
    Ulid(#[from] ulid::DecodeError),
    #[error("no quantizer trained yet")]
    NoQuantizer,
    #[error("preview must be utf-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("quantizer format: {0}")]
    QuantizerFormat(String),
}

// ── MemoryStore ───────────────────────────────────────────────────────────────

/// Persistent store combining `SQLite` metadata + CAS body blobs.
pub struct MemoryStore {
    sql: Arc<SqlStore>,
    cas: Arc<CasStore>,
    /// Cached quantizer to avoid repeated SQL round-trips.
    q_cache: RwLock<Option<Quantizer>>,
}

impl MemoryStore {
    /// Create a new store backed by `sql` and `cas`.
    #[must_use]
    pub const fn new(sql: Arc<SqlStore>, cas: Arc<CasStore>) -> Self {
        Self {
            sql,
            cas,
            q_cache: RwLock::new(None),
        }
    }

    // ── Quantizer management ──────────────────────────────────────────────

    /// Persist `q` in the `mem_quantizer` singleton row and update the cache.
    ///
    /// # Errors
    /// Propagates SQL errors.
    pub fn install_quantizer(&self, q: &Quantizer) -> Result<(), StorageError> {
        let bytes = q.to_bytes();
        self.sql.with_conn(|conn| {
            conn.execute(
                "INSERT INTO mem_quantizer (id, bytes) VALUES (1, ?1) \
                 ON CONFLICT(id) DO UPDATE SET bytes = excluded.bytes",
                params![bytes],
            )?;
            Ok(())
        })?;
        // Deserialise from the bytes we just serialised to get an owned copy.
        let owned =
            Quantizer::from_bytes(&bytes).map_err(|e| StorageError::QuantizerFormat(e.to_string()))?;
        *self.q_cache.write() = Some(owned);
        Ok(())
    }

    /// Load the stored quantizer, if any.  Populates `q_cache` on success so
    /// subsequent [`Self::save`] calls skip the SQL round-trip.
    ///
    /// # Errors
    /// Propagates SQL or deserialisation errors.
    pub fn load_quantizer(&self) -> Result<Option<Quantizer>, StorageError> {
        let opt = self.deserialise_from_db_opt()?;
        if let Some(ref q) = opt {
            *self.q_cache.write() = Some(q.clone());
        }
        Ok(opt)
    }

    // ── Core CRUD ────────────────────────────────────────────────────────

    /// Store `body` bytes in CAS, quantise `vector`, and insert a `memories` row.
    ///
    /// Requires a quantizer to have been installed via [`Self::install_quantizer`].
    ///
    /// # Errors
    /// Returns [`StorageError::NoQuantizer`] if no quantizer has been installed.
    /// Propagates SQL and CAS errors.
    pub fn save(
        &self,
        body: &str,
        vector: &[f32; crate::EMBED_DIM],
        tags: &[&str],
    ) -> Result<MemoryId, StorageError> {
        // --- Quantise ---
        let encoded = {
            let guard = self.q_cache.read();
            match guard.as_ref() {
                Some(q) => q.encode(vector),
                None => return Err(StorageError::NoQuantizer),
            }
        };

        // --- CAS write ---
        let hash = self.cas.put(body.as_bytes())?;
        let body_handle = *hash.as_bytes();

        // --- Preview (≤64 UTF-8 bytes, codepoint-boundary) ---
        let preview = truncate_to_64_bytes(body);

        // --- Timestamp ---
        let now_ms = now_ms();
        let id = Ulid::new();

        // --- Tag resolution + memory INSERT (single atomic transaction) ---
        // Reinterpret i8 bytes as u8 for BLOB storage; bit pattern is preserved.
        #[allow(clippy::cast_sign_loss)]
        let deltas_blob: Vec<u8> = encoded.deltas.iter().map(|&b| b as u8).collect();
        let centroid_id = i64::from(encoded.centroid_id);
        let id_str = id.to_string();
        let superseded_by: Option<String> = None;

        self.sql.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let tags_bitset = resolve_tags(&tx, tags)?;
            tx.execute(
                "INSERT INTO memories \
                 (id, centroid_id, deltas, body_handle, body_preview, tags_bitset, \
                  created_at, last_seen_at, superseded_by, cluster_priority) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1.0)",
                params![
                    id_str,
                    centroid_id,
                    deltas_blob,
                    body_handle.as_slice(),
                    preview,
                    tags_bitset.as_slice(),
                    now_ms,
                    now_ms,
                    superseded_by,
                ],
            )?;
            tx.commit()
        })?;

        Ok(id)
    }

    /// Delete a memory row.  The CAS blob is left for the GC sweeper to reap.
    ///
    /// # Errors
    /// Propagates SQL errors.
    ///
    /// # Note
    /// Refcount decrement is deferred to the existing idle GC (see P2 / `RefTable`).
    /// TODO(P6.x): call `RefTable::decr` here once the GC is wired to `MemoryStore`.
    pub fn forget(&self, id: MemoryId) -> Result<(), StorageError> {
        let id_str = id.to_string();
        self.sql.with_conn(|conn| {
            conn.execute("DELETE FROM memories WHERE id = ?1", params![id_str])?;
            Ok(())
        })?;
        Ok(())
    }

    /// Mark `loser` as superseded by `winner` and update `last_seen_at`.
    ///
    /// # Errors
    /// Propagates SQL errors.
    pub fn mark_superseded(&self, loser: MemoryId, winner: MemoryId) -> Result<(), StorageError> {
        let loser_str = loser.to_string();
        let winner_str = winner.to_string();
        let now_ms = now_ms();
        self.sql.with_conn(|conn| {
            conn.execute(
                "UPDATE memories SET superseded_by = ?1, last_seen_at = ?2 WHERE id = ?3",
                params![winner_str, now_ms, loser_str],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Retrieve a single memory by id.  Returns `Ok(None)` if not found.
    ///
    /// # Errors
    /// Propagates SQL or ULID parse errors.
    pub fn get(&self, id: MemoryId) -> Result<Option<MemoryRecord>, StorageError> {
        let id_str = id.to_string();
        let rows = self.sql.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.centroid_id, m.deltas, m.body_handle, m.body_preview, \
                 m.tags_bitset, m.created_at, m.last_seen_at, m.superseded_by, \
                 m.cluster_priority \
                 FROM memories m WHERE m.id = ?1",
            )?;
            query_records(&mut stmt, params![id_str])
        })?;

        // Resolve tag names in a second pass (requires another conn call).
        match rows.into_iter().next() {
            None => Ok(None),
            Some(partial) => {
                let tags = self.resolve_tag_names(&partial.tags_bitset_raw)?;
                Ok(Some(partial.into_record(tags)))
            }
        }
    }

    /// Return all memories, ordered by id (deterministic for consolidator).
    ///
    /// # Errors
    /// Propagates SQL or ULID parse errors.
    pub fn iter_all(&self) -> Result<Vec<MemoryRecord>, StorageError> {
        let rows = self.sql.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.centroid_id, m.deltas, m.body_handle, m.body_preview, \
                 m.tags_bitset, m.created_at, m.last_seen_at, m.superseded_by, \
                 m.cluster_priority \
                 FROM memories m ORDER BY m.id",
            )?;
            query_records(&mut stmt, params![])
        })?;

        // Batch-resolve all tag names.
        let mut out = Vec::with_capacity(rows.len());
        for partial in rows {
            let tags = self.resolve_tag_names(&partial.tags_bitset_raw)?;
            out.push(partial.into_record(tags));
        }
        Ok(out)
    }

    /// Insert or ignore a directed edge between two memories (idempotent).
    ///
    /// # Errors
    /// Propagates SQL errors.
    pub fn add_edge(
        &self,
        from: MemoryId,
        to: MemoryId,
        kind: EdgeKind,
        weight: f32,
    ) -> Result<(), StorageError> {
        let from_str = from.to_string();
        let to_str = to.to_string();
        let kind_val = kind as i64;
        let now_ms = now_ms();
        self.sql.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO mem_edges \
                 (from_id, to_id, kind, weight, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![from_str, to_str, kind_val, weight, now_ms],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Resolve set bits in `bitset` to tag name strings.
    fn resolve_tag_names(&self, bitset: &[u8; 16]) -> Result<Vec<String>, StorageError> {
        let bits = BitArray::<[u8; 16], Lsb0>::new(*bitset);
        // Bit index is 0..127 — fits i64 on all targets.
        #[allow(clippy::cast_possible_wrap)]
        let set_indices: Vec<i64> = bits
            .iter()
            .enumerate()
            .filter_map(|(i, b)| if *b { Some(i as i64) } else { None })
            .collect();

        if set_indices.is_empty() {
            return Ok(Vec::new());
        }

        let names = self.sql.with_conn(|conn| {
            let mut names = Vec::with_capacity(set_indices.len());
            for idx in &set_indices {
                let name: Option<String> = conn
                    .query_row(
                        "SELECT name FROM mem_tags WHERE bit_idx = ?1",
                        params![idx],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(n) = name {
                    names.push(n);
                }
            }
            Ok(names)
        })?;
        Ok(names)
    }

    fn deserialise_from_db_opt(&self) -> Result<Option<Quantizer>, StorageError> {
        let bytes_opt: Option<Vec<u8>> = self.sql.with_conn(|conn| {
            conn.query_row("SELECT bytes FROM mem_quantizer WHERE id = 1", [], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .optional()
        })?;

        match bytes_opt {
            None => Ok(None),
            Some(bytes) => {
                let q = Quantizer::from_bytes(&bytes)
                    .map_err(|e| StorageError::QuantizerFormat(e.to_string()))?;
                Ok(Some(q))
            }
        }
    }
}

// ── Partial row ───────────────────────────────────────────────────────────────

/// Intermediate struct used while deserialising SQL rows (before tag resolution).
struct PartialRow {
    id: MemoryId,
    encoded: EncodedVector,
    body_handle: [u8; 32],
    body_preview: String,
    tags_bitset_raw: [u8; 16],
    created_at_ms: i64,
    last_seen_at_ms: i64,
    superseded_by: Option<MemoryId>,
    cluster_priority: f32,
}

impl PartialRow {
    fn into_record(self, tags: Vec<String>) -> MemoryRecord {
        MemoryRecord {
            id: self.id,
            encoded: self.encoded,
            body_handle: self.body_handle,
            body_preview: self.body_preview,
            tags,
            created_at_ms: self.created_at_ms,
            last_seen_at_ms: self.last_seen_at_ms,
            superseded_by: self.superseded_by,
            cluster_priority: self.cluster_priority,
        }
    }
}

// ── SQL helpers ───────────────────────────────────────────────────────────────

/// Execute a prepared `SELECT` for memory rows and map each row to `PartialRow`.
fn query_records(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<PartialRow>> {
    stmt.query_map(params, map_row)?.collect()
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PartialRow> {
    let id_str: String = row.get(0)?;
    let centroid_id: i64 = row.get(1)?;
    let deltas_blob: Vec<u8> = row.get(2)?;
    let handle_blob: Vec<u8> = row.get(3)?;
    let body_preview: String = row.get(4)?;
    let tags_blob: Vec<u8> = row.get(5)?;
    let created_at_ms: i64 = row.get(6)?;
    let last_seen_at_ms: i64 = row.get(7)?;
    let superseded_str: Option<String> = row.get(8)?;
    let cluster_priority: f64 = row.get(9)?;

    // Parse ULID.
    let id = Ulid::from_string(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    // Parse centroid id.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let centroid_id_u8 = centroid_id.clamp(0, 255) as u8;

    // Parse deltas BLOB (exactly 384 bytes → Box<[i8; 384]>).
    let mut deltas = Box::new([0_i8; crate::EMBED_DIM]);
    let len = deltas_blob.len().min(crate::EMBED_DIM);
    // Reinterpret BLOB bytes as i8; bit pattern preserved (inverse of save).
    #[allow(clippy::cast_possible_wrap)]
    for (slot, &b) in deltas.iter_mut().zip(deltas_blob[..len].iter()) {
        *slot = b as i8;
    }

    // Parse body_handle (exactly 32 bytes).
    let mut body_handle = [0u8; 32];
    let hlen = handle_blob.len().min(32);
    body_handle[..hlen].copy_from_slice(&handle_blob[..hlen]);

    // Parse tags bitset (exactly 16 bytes).
    let mut tags_bitset_raw = [0u8; 16];
    let blen = tags_blob.len().min(16);
    tags_bitset_raw[..blen].copy_from_slice(&tags_blob[..blen]);

    // Parse optional superseded_by ULID.
    let superseded_by = superseded_str
        .map(|s| {
            Ulid::from_string(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
            })
        })
        .transpose()?;

    #[allow(clippy::cast_possible_truncation)]
    let cluster_priority = cluster_priority as f32;

    Ok(PartialRow {
        id,
        encoded: EncodedVector {
            centroid_id: centroid_id_u8,
            deltas,
        },
        body_handle,
        body_preview,
        tags_bitset_raw,
        created_at_ms,
        last_seen_at_ms,
        superseded_by,
        cluster_priority,
    })
}

/// Lookup-or-insert each tag name into `mem_tags`; return a 16-byte bitset BLOB.
///
/// Uses INSERT OR IGNORE (UPSERT) so concurrent callers racing on the same tag
/// name all converge to the same `bit_idx`.  Must be called inside an active
/// transaction so the next-free-slot lookup and the INSERT are race-free.
///
/// Tags beyond bit index 127 are silently dropped with a `tracing::warn!`.
fn resolve_tags(conn: &rusqlite::Connection, tags: &[&str]) -> rusqlite::Result<Vec<u8>> {
    let mut bits = BitArray::<[u8; 16], Lsb0>::new([0u8; 16]);
    for &name in tags {
        // Pick the candidate next-free slot before attempting the INSERT.
        let max_idx: Option<i64> = conn
            .query_row("SELECT MAX(bit_idx) FROM mem_tags", [], |r| r.get(0))
            .optional()?
            .flatten();
        let next = max_idx.map_or(0, |m| m + 1);
        if next > 127 {
            tracing::warn!(tag = name, "mem_tags: bit_idx exhausted (>127), dropping tag");
            continue;
        }

        // INSERT OR IGNORE: if another caller already inserted this name (race),
        // the existing row wins and the INSERT is silently skipped.
        conn.execute(
            "INSERT OR IGNORE INTO mem_tags (bit_idx, name) VALUES (?1, ?2)",
            params![next, name],
        )?;

        // Always resolve bit_idx from the authoritative row (handles both the
        // fresh-insert case and the already-existed case).
        let bit_idx: i64 = conn.query_row(
            "SELECT bit_idx FROM mem_tags WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;

        if bit_idx > 127 {
            tracing::warn!(tag = name, "mem_tags: bit_idx {} > 127, dropping tag", bit_idx);
            continue;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        bits.set(bit_idx as usize, true);
    }
    Ok(bits.into_inner().to_vec())
}

// ── Utility ───────────────────────────────────────────────────────────────────

/// Truncate `s` to at most 64 UTF-8 bytes on a codepoint boundary.
fn truncate_to_64_bytes(s: &str) -> &str {
    if s.len() <= 64 {
        return s;
    }
    // Walk codepoints and stop before we'd exceed 64 bytes.
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        if i + ch.len_utf8() > 64 {
            break;
        }
        end = i + ch.len_utf8();
    }
    &s[..end]
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
