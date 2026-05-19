//! `origin-codegraph` — native code knowledge graph (Phase 7).
//!
//! Modules land per-task across P7.1–P7.8; this lib.rs collects them.

pub mod chunker;
pub mod community;
pub mod extract;
pub mod index;
pub mod lang;
pub mod query;
pub mod record;
pub mod sidecar;

pub use extract::{CodeEdge, CodeNode, EdgeKind, NodeKind};
pub use index::{CodeGraphIndex, EdgeRow, EntityId, IndexError, NodeRow};
pub use lang::{LangError, Language, Parser};
pub use record::{CodeNodeRecord, Confidence};
