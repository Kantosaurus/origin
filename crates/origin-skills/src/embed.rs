//! Embed skill bodies into `origin_mem::MemIndex` with kind = `Skill`.
//!
//! N9.4 — bodies are content-addressed (skill body lives in CAS via `body_hash`);
//! the index entry's public id is the lower 64 bits of the ULID we mint per skill.

use crate::loader::Skill;
use origin_mem::{IndexError, MemIndex, EMBED_DIM};
use thiserror::Error;

/// Embedder façade. In production, holds an `origin_mem::Embedder`; in tests,
/// holds a deterministic stub.
pub struct SkillEmbedder {
    inner: Inner,
}

enum Inner {
    Stub,
}

#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Error)]
pub enum SkillEmbedError {
    #[error("index: {0}")]
    Index(#[from] IndexError),
}

impl SkillEmbedder {
    /// Deterministic stub for unit tests: maps text bytes onto a normalised
    /// vector by hashing into `EMBED_DIM` floats. Production callers will use
    /// `SkillEmbedder::with_embedder` once `origin_mem::Embedder` is wired in.
    #[must_use]
    pub const fn stub_for_tests() -> Self {
        Self { inner: Inner::Stub }
    }

    /// Returns a normalised embedding for `text` (test-only deterministic impl).
    #[must_use]
    pub fn embed_for_tests(&self, text: &str) -> [f32; EMBED_DIM] {
        let mut v = [0f32; EMBED_DIM];
        let h = blake3::hash(text.as_bytes());
        let bytes = h.as_bytes();
        for (i, slot) in v.iter_mut().enumerate() {
            let b = bytes[i % bytes.len()];
            *slot = (f32::from(b) / 255.0).mul_add(2.0, -1.0);
        }
        // L2-normalise.
        let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for slot in &mut v {
            *slot /= mag;
        }
        v
    }

    /// Embed `skill.body` and insert into `index`. Returns the public u64 id used.
    ///
    /// The id is the lower 64 bits of the blake3 body hash — deterministic
    /// across hosts so re-importing the same skill body is idempotent.
    ///
    /// # Errors
    /// Forwards [`IndexError`] on insertion failure.
    ///
    /// # Panics
    /// Never panics in practice — the `expect` on the slice conversion is
    /// infallible because `blake3::Hash` is always 32 bytes and we take `[..8]`.
    pub fn upsert(&mut self, index: &mut MemIndex, skill: &Skill) -> Result<u64, SkillEmbedError> {
        // body_hash is a fixed 32 bytes; the [..8] slice always converts.
        let bytes: [u8; 8] = skill.body_hash.0[..8]
            .try_into()
            .expect("blake3 hash is 32 bytes; first 8 always present");
        let id = u64::from_le_bytes(bytes);
        let vec = match self.inner {
            Inner::Stub => self.embed_for_tests(&skill.body),
        };
        index.insert(id, &vec)?;
        Ok(id)
    }
}
