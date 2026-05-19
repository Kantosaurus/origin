//! `origin-codegraph` — native code knowledge graph (Phase 7).
//!
//! Modules land per-task across P7.1–P7.8; this lib.rs collects them.

pub mod extract;
pub mod lang;

pub use extract::{CodeEdge, CodeNode, EdgeKind, NodeKind};
pub use lang::{LangError, Language, Parser};
