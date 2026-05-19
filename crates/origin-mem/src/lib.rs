//! `origin-mem` — conversation memory: ONNX `MiniLM` embeddings + int8 quantization
//! + HNSW + temporal-decay re-rank, with bodies in CAS and edges in `SQLite`.

pub mod embedder;
pub mod index;
pub mod injector;
pub mod proposer;
pub mod quantizer;
pub mod storage;

// `EmbedderError` repeats the module name; we re-export it under the canonical
// name to keep the public surface stable across the rest of Phase 6, even
// though clippy's `module_name_repetitions` flags it.
#[allow(clippy::module_name_repetitions)]
pub use embedder::EmbedderError;
pub use embedder::{Embedder, EMBED_DIM};
pub use index::{Candidate, IndexError, MemIndex, MetaRow, SearchOpts};
pub use injector::{InjectedContext, Injector, InjectorError};
pub use proposer::{MemoryProposal, Proposer};
pub use quantizer::{EncodedVector, Quantizer, QuantizerError, NUM_CENTROIDS};
pub use storage::{EdgeKind, MemoryId, MemoryRecord, MemoryStore, StorageError};
