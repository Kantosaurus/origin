//! Session-scoped output-content-addressed store.
//!
//! The envelope hashes each non-mutating tool result and stores the bytes
//! once per session. Repeat reads of the same file (or any byte-identical
//! result) replay as a short `{tool_result_ref: "blake3:…"}` token, which
//! the agent expands back to the body before serialising into the provider's
//! `tool_result` block. Since the bytes are byte-identical across calls,
//! the provider's prompt cache hits and incremental token cost is ~0.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use blake3::Hash as Blake3Hash;
use serde_json::{json, Value};

type StoreMap = HashMap<[u8; 32], Arc<[u8]>>;

#[derive(Debug, Clone)]
pub struct ResultStore {
    inner: Arc<RwLock<StoreMap>>,
}

impl Default for ResultStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ResultStore {
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// Store `body` under its blake3 hash and return the hash.
    ///
    /// # Panics
    /// Panics if the internal `RwLock` is poisoned (only happens if another
    /// thread panicked while holding the write lock).
    #[must_use]
    pub fn put(&self, body: &[u8]) -> Blake3Hash {
        let h = blake3::hash(body);
        let mut w = self.inner.write().expect("ResultStore RwLock poisoned");
        w.entry(*h.as_bytes()).or_insert_with(|| Arc::from(body.to_vec().into_boxed_slice()));
        h
    }

    /// Retrieve stored bytes by hash, if present.
    ///
    /// # Panics
    /// Panics if the internal `RwLock` is poisoned.
    #[must_use]
    pub fn get(&self, h: &Blake3Hash) -> Option<Arc<[u8]>> {
        self.inner.read().expect("ResultStore RwLock poisoned").get(h.as_bytes()).cloned()
    }
}

/// Build the `{tool_result_ref, bytes, preview}` short-form value.
#[must_use]
pub fn ref_token(h: &Blake3Hash, bytes: usize, body_str: &str) -> Value {
    let preview: String = body_str.chars().take(80).collect();
    json!({
        "tool_result_ref": format!("blake3:{}", h.to_hex()),
        "bytes": bytes,
        "preview": preview,
    })
}
