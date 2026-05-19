//! `origin-mem` — conversation memory: ONNX `MiniLM` embeddings + int8 quantization
//! + HNSW + temporal-decay re-rank, with bodies in CAS and edges in `SQLite`.

pub mod embedder;

// `EmbedderError` repeats the module name; we re-export it under the canonical
// name to keep the public surface stable across the rest of Phase 6, even
// though clippy's `module_name_repetitions` flags it.
#[allow(clippy::module_name_repetitions)]
pub use embedder::EmbedderError;
pub use embedder::{Embedder, EMBED_DIM};
