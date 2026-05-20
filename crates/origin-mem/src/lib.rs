//! `origin-mem` — conversation memory: ONNX `MiniLM` embeddings + int8 quantization
//! + HNSW + temporal-decay re-rank, with bodies in CAS and edges in `SQLite`.

pub mod consolidator;
pub mod embedder;
pub mod index;
pub mod injector;
pub mod proposer;
pub mod quantizer;
pub mod storage;

/// Seconds in one day (24 × 60 × 60). Used by age-based scoring and by
/// admin code that surfaces day-aged thresholds.
pub const SECS_PER_DAY: u64 = 86_400;

/// Milliseconds in one day, in `f32` so callers avoid an extra cast in
/// the typical `(now_ms - then_ms) / MS_PER_DAY` recency calculation.
pub const MS_PER_DAY: f32 = 86_400_000.0;

// `EmbedderError` repeats the module name; we re-export it under the canonical
// name to keep the public surface stable across the rest of Phase 6, even
// though clippy's `module_name_repetitions` flags it.
pub use consolidator::{ConsolidationError, ConsolidationReport, Consolidator};
#[allow(clippy::module_name_repetitions)]
pub use embedder::EmbedderError;
pub use embedder::{Embedder, EMBED_DIM};
pub use index::{Candidate, IndexError, MemIndex, MetaRow, SearchOpts};
pub use injector::{memory_id_to_u64, InjectedContext, Injector, InjectorError};
pub use proposer::{MemoryProposal, Proposer};
pub use quantizer::{EncodedVector, Quantizer, QuantizerError, NUM_CENTROIDS};
pub use storage::{EdgeKind, MemoryId, MemoryRecord, MemoryStore, StorageError};
