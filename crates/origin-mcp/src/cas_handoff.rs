//! CAS hand-off for large MCP tool results.
//!
//! Results above the threshold are stored as a single CAS entry; the proxy then
//! returns a sentinel JSON envelope `{"cas":{"handle":"…hex…","byte_len":N}}` to the model.

use origin_cas::{Hash, Store};
use serde_json::{json, Value};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug)]
pub enum HandoffOutcome {
    Inline(Value),
    Cas { handle: Hash, byte_len: usize },
}

#[derive(Debug, Error)]
pub enum HandoffError {
    #[error("serialize: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
}

/// Serialize `value`; if the byte length exceeds `threshold`, put it into
/// `store` and return a [`HandoffOutcome::Cas`]; otherwise return
/// [`HandoffOutcome::Inline`].
///
/// # Errors
/// Returns [`HandoffError`] on serialization or CAS write failure.
#[allow(clippy::module_name_repetitions)]
pub fn cas_handoff_if_large(
    store: &Arc<Store>,
    value: Value,
    threshold: usize,
) -> Result<HandoffOutcome, HandoffError> {
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() <= threshold {
        return Ok(HandoffOutcome::Inline(value));
    }
    let handle = store.put(&bytes)?;
    Ok(HandoffOutcome::Cas {
        handle,
        byte_len: bytes.len(),
    })
}

/// Encode a [`HandoffOutcome::Cas`] as a JSON envelope the model can recognize.
///
/// `Hash` implements `Display` as lowercase hex (see `origin-cas/src/hash.rs`),
/// so `to_string()` yields the 64-char hex digest.
#[must_use]
pub fn cas_envelope(handle: Hash, byte_len: usize) -> Value {
    json!({"cas": {"handle": handle.to_string(), "byte_len": byte_len}})
}
