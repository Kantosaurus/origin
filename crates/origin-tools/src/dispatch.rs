// SPDX-License-Identifier: Apache-2.0
//! Shared dispatch + per-session memoization (N5.4) and memory-tool support types.
//!
//! - `Cache` / `NormalizedInput` / `CacheHit`: the agent looks up
//!   `(tool_name, normalized_input)` in `Cache` before actually running the
//!   tool. `Bash`, `Edit`, and `Write` are on the deny-list because their
//!   side effects mean a cached result could be stale. (Phase 3.)
//! - `MemoryHandle` / `SearchHit` / `MemoryToolError`: the trait the dispatch
//!   passes into memory-aware tools (`mem_search` / `mem_save` / `mem_forget`).
//!   The daemon implements `MemoryHandle` by wrapping `origin_mem` types so
//!   `origin-tools` itself does not depend on `origin-mem`. (Phase 6.)

use blake3::Hash as Blake3Hash;
use std::collections::HashMap;

/// Tool names that never memoize. Side-effect-free guarantees do not extend
/// to tools that write to the filesystem or shell out.
pub const MEMOIZATION_SKIPLIST: &[&str] = &["Bash", "Edit", "Write"];

/// 32-byte content hash of `(tool_name, raw_input_bytes)`. The hash function
/// is blake3, matching the rest of `origin` (`origin-cas::Hash`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalizedInput(Blake3Hash);

impl NormalizedInput {
    /// Compute the canonical key for `(tool_name, raw_input)`.
    ///
    /// Phase 3 uses byte-equivalent normalization: identical input bytes
    /// produce identical keys. Tool-specific normalization (path canon,
    /// regex parse-equivalent collapsing) is in scope for Phase 10.
    #[must_use]
    pub fn hash(tool_name: &str, raw_input: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(tool_name.as_bytes());
        h.update(&[0xff]); // separator
        h.update(raw_input);
        Self(h.finalize())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CacheHit {
    pub handle: [u8; 32],
    pub from_turn: u32,
}

#[derive(Debug, Default)]
pub struct Cache {
    table: HashMap<NormalizedInput, CacheHit>,
}

impl Cache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `tool_name` is on the memoization deny-list.
    #[must_use]
    pub fn is_skipped(&self, tool_name: &str) -> bool {
        MEMOIZATION_SKIPLIST.contains(&tool_name)
    }

    /// Try to fetch the cached handle for a previously-run tool call.
    #[must_use]
    pub fn lookup(&self, key: &NormalizedInput) -> Option<&CacheHit> {
        self.table.get(key)
    }

    /// Record a result. `turn` is the conversation turn number for the
    /// `(cached from turn N)` annotation the agent appends when serving
    /// a hit.
    pub fn record(&mut self, key: NormalizedInput, handle: [u8; 32], turn: u32) {
        self.table.insert(
            key,
            CacheHit {
                handle,
                from_turn: turn,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 6 — memory-tool support types
// ---------------------------------------------------------------------------

/// A semantic-search hit returned by [`MemoryHandle::search`].
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// ULID string identifying the memory entry.
    pub id: String,
    /// Short text preview of the stored body.
    pub preview: String,
    /// Cosine similarity score in [0, 1].
    pub score: f32,
    /// Age of the entry in fractional days.
    pub age_days: f32,
    /// User-supplied tags attached to the entry.
    pub tags: Vec<String>,
}

/// Errors that memory tool operations can surface.
#[derive(Debug, thiserror::Error)]
pub enum MemoryToolError {
    /// The memory subsystem is not reachable (e.g. daemon not running).
    #[error("memory subsystem unavailable")]
    Unavailable,
    /// The supplied id string is not a valid ULID.
    #[error("invalid id: {0}")]
    BadId(String),
    /// A storage-layer failure with a human-readable description.
    #[error("storage: {0}")]
    Storage(String),
}

/// Thin object-safe handle the tool dispatch passes into memory-aware tools.
///
/// The daemon will implement this by wrapping `origin_mem`'s `MemoryStore`,
/// `Embedder`, and `MemIndex`. `origin-tools` itself remains free of that dep.
pub trait MemoryHandle: Send + Sync + std::fmt::Debug {
    /// Search top-k entries by semantic similarity to `query`.
    ///
    /// When `fresh` is `true`, implementations should bias toward recently saved
    /// entries when ranking is otherwise tied.
    ///
    /// # Errors
    /// Returns [`MemoryToolError`] on subsystem failure.
    fn search(&self, query: &str, k: usize, fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError>;

    /// Persist `body` with optional `tags`; returns the new entry's ULID string.
    ///
    /// # Errors
    /// Returns [`MemoryToolError`] on subsystem failure.
    fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError>;

    /// Hard-delete the entry identified by the ULID `id`.
    ///
    /// # Errors
    /// Returns [`MemoryToolError::BadId`] if `id` is not a valid ULID or is
    /// not found, and [`MemoryToolError::Storage`] on I/O failure.
    fn forget(&self, id: &str) -> Result<(), MemoryToolError>;
}
