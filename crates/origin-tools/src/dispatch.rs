//! Shared dispatch + per-session memoization (N5.4).
//!
//! The agent looks up `(tool_name, normalized_input)` in `Cache` before
//! actually running the tool. `Bash`, `Edit`, and `Write` are on the deny-list
//! because their side effects mean a cached result could be stale.

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
