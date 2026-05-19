//! Walk a tree-sitter tree → `CodeNode` records.
//!
//! Edges land in P7.3; this module emits nodes only.

use thiserror::Error;
use tree_sitter::Node;

use crate::lang::{LangError, Language};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Function,
    Method,
    Struct,
    Class,
    Trait,
    Interface,
    Module,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

/// Public stub. Full `CodeNode` (with `signature_handle`, `body_handle`) lands
/// in P7.3 when records are wired through CAS. Here we surface just enough
/// (name, kind, byte range) to validate extraction.
#[derive(Debug, Clone)]
pub struct CodeNode {
    pub name: String,
    pub kind: NodeKind,
    pub range: Range,
}

/// Public stub — P7.3 expands to the full edge record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Calls,
    Mentions,
    Implements,
    Extends,
}

#[derive(Debug, Clone)]
pub struct CodeEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

/// Errors produced while walking a parsed tree.
// `ExtractError` matches the public API in the Phase 7 plan; the
// `Extract` prefix disambiguates from `LangError`.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("lang: {0}")]
    Lang(#[from] LangError),
    #[error("source not utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Extract top-level node declarations.
///
/// # Errors
/// Returns [`ExtractError::Lang`] if parsing fails and [`ExtractError::Utf8`]
/// if a name slice is not valid UTF-8.
// `extract_nodes` is the public verb for this module's primary action; the
// shared module-name prefix is intentional and matches the plan's API.
#[allow(clippy::module_name_repetitions)]
pub fn extract_nodes(lang: Language, src: &[u8]) -> Result<Vec<CodeNode>, ExtractError> {
    let tree = lang.parse(src)?;
    let mut out = Vec::new();
    walk(tree.root_node(), lang, src, &mut out)?;
    Ok(out)
}

fn walk(node: Node, lang: Language, src: &[u8], out: &mut Vec<CodeNode>) -> Result<(), ExtractError> {
    if let Some((name, kind)) = classify(node, lang, src)? {
        out.push(CodeNode {
            name,
            kind,
            range: Range {
                start: node.start_byte(),
                end: node.end_byte(),
            },
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, lang, src, out)?;
    }
    Ok(())
}

/// Classify a tree-sitter node as a `NodeKind` if it represents a top-level
/// declaration in the given language. Returns `Ok(None)` when the node is
/// uninteresting or has no recoverable name.
fn classify(node: Node, lang: Language, src: &[u8]) -> Result<Option<(String, NodeKind)>, ExtractError> {
    let Some(kind) = node_kind_for(lang, node.kind()) else {
        return Ok(None);
    };
    let Some(name_node) = node.child_by_field_name("name") else {
        return Ok(None);
    };
    let name = std::str::from_utf8(&src[name_node.start_byte()..name_node.end_byte()])?.to_owned();
    Ok(Some((name, kind)))
}

/// Map a `(Language, ts_kind)` pair to a `NodeKind`. Grouped by `NodeKind` to
/// keep clippy's `match_same_arms` happy and to make the language → kind table
/// easy to scan.
fn node_kind_for(lang: Language, ts_kind: &str) -> Option<NodeKind> {
    // Functions across languages.
    let is_function = matches!(
        (lang, ts_kind),
        (Language::Rust, "function_item")
            | (Language::TypeScript | Language::Go, "function_declaration")
            | (Language::Python, "function_definition"),
    );
    if is_function {
        return Some(NodeKind::Function);
    }
    // Methods across languages.
    let is_method = matches!(
        (lang, ts_kind),
        (Language::TypeScript, "method_definition") | (Language::Go | Language::Java, "method_declaration"),
    );
    if is_method {
        return Some(NodeKind::Method);
    }
    // Structs / record-shaped declarations.
    let is_struct = matches!(
        (lang, ts_kind),
        (Language::Rust, "struct_item") | (Language::Go, "type_declaration"),
    );
    if is_struct {
        return Some(NodeKind::Struct);
    }
    // Classes (TS / Python / Java).
    let is_class = matches!(
        (lang, ts_kind),
        (Language::TypeScript | Language::Java, "class_declaration") | (Language::Python, "class_definition"),
    );
    if is_class {
        return Some(NodeKind::Class);
    }
    // Traits (Rust only).
    if matches!((lang, ts_kind), (Language::Rust, "trait_item")) {
        return Some(NodeKind::Trait);
    }
    // Interfaces (TS / Java).
    if matches!(
        (lang, ts_kind),
        (Language::TypeScript | Language::Java, "interface_declaration"),
    ) {
        return Some(NodeKind::Interface);
    }
    // Modules (Rust only at P7.1).
    if matches!((lang, ts_kind), (Language::Rust, "mod_item")) {
        return Some(NodeKind::Module);
    }
    None
}
