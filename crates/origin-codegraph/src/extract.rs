// SPDX-License-Identifier: Apache-2.0
//! Walk a tree-sitter tree → `CodeNode` records.
//!
//! Edges land in P7.3; this module emits nodes only.

use origin_cas::{Store as CasStore, StoreError as CasError};
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

/// Extracted code-graph node.
///
/// `range` is the whole declaration span; `signature_range` covers the header
/// (start → opening `{` for blocked declarations, or the whole node when there
/// is no body block); `body_range` is the inside of the block when present,
/// otherwise `None`.
///
/// `signature_handle` / `body_handle` are zeroed when produced via
/// [`extract_nodes`], and populated with CAS hashes when produced via
/// [`extract_nodes_with_cas`].
#[derive(Debug, Clone)]
pub struct CodeNode {
    pub name: String,
    pub kind: NodeKind,
    pub range: Range,
    pub signature_range: Range,
    pub body_range: Option<Range>,
    pub signature_handle: [u8; 32],
    pub body_handle: [u8; 32],
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
    #[error("cas: {0}")]
    Cas(#[from] CasError),
}

/// Extract top-level node declarations.
///
/// The returned [`CodeNode`]s have `signature_handle` and `body_handle` set to
/// the zero hash; use [`extract_nodes_with_cas`] to populate them via CAS.
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

/// Same as [`extract_nodes`] but CAS-writes signature/body byte slices.
///
/// The resulting handles populate every emitted [`CodeNode`]. Nodes with no
/// body block (e.g. unit structs) get the zero handle for `body_handle`.
///
/// # Errors
/// Propagates [`ExtractError::Lang`], [`ExtractError::Utf8`], and
/// [`ExtractError::Cas`] when the CAS store rejects a write.
#[allow(clippy::module_name_repetitions)]
pub fn extract_nodes_with_cas(
    lang: Language,
    src: &[u8],
    cas: &CasStore,
) -> Result<Vec<CodeNode>, ExtractError> {
    let mut nodes = extract_nodes(lang, src)?;
    for n in &mut nodes {
        let sig_end = n.signature_range.end.min(src.len());
        let sig_start = n.signature_range.start.min(sig_end);
        let sig_bytes = &src[sig_start..sig_end];
        n.signature_handle = *cas.put(sig_bytes)?.as_bytes();
        if let Some(b) = n.body_range {
            let end = b.end.min(src.len());
            let start = b.start.min(end);
            n.body_handle = *cas.put(&src[start..end])?.as_bytes();
        }
    }
    Ok(nodes)
}

fn walk(node: Node, lang: Language, src: &[u8], out: &mut Vec<CodeNode>) -> Result<(), ExtractError> {
    if let Some((name, kind)) = classify(node, lang, src)? {
        let whole = Range {
            start: node.start_byte(),
            end: node.end_byte(),
        };
        let (signature_range, body_range) = sig_and_body_ranges(node, whole);
        out.push(CodeNode {
            name,
            kind,
            range: whole,
            signature_range,
            body_range,
            signature_handle: [0u8; 32],
            body_handle: [0u8; 32],
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, lang, src, out)?;
    }
    Ok(())
}

/// Compute `(signature_range, body_range)` for a classified declaration node.
///
/// The body is the child reached via tree-sitter's `body` field (function
/// blocks, class bodies, etc). The signature is everything from the node's
/// start up to the body's start, less any trailing whitespace. When there is
/// no body field the signature is the whole declaration and `body_range` is
/// `None` (true for e.g. Rust unit / tuple structs and `mod foo;`).
fn sig_and_body_ranges(node: Node, whole: Range) -> (Range, Option<Range>) {
    node.child_by_field_name("body").map_or((whole, None), |body| {
        let body_outer_start = body.start_byte();
        let body_outer_end = body.end_byte();
        // For blocked bodies (`{ ... }`, indented Python suites) tree-sitter
        // includes the delimiters in the body node's span. Trim the leading
        // `{` so `body_handle` content matches the human notion of "body".
        // Python suites lead with `:` + newline; we can't easily strip those
        // without language-specific logic, so we just keep the outer span.
        let body_inner_start = body_outer_start.saturating_add(1).min(body_outer_end);
        let body_inner_end = body_outer_end.saturating_sub(1).max(body_inner_start);
        let signature_end = body_outer_start.max(whole.start);
        (
            Range {
                start: whole.start,
                end: signature_end,
            },
            Some(Range {
                start: body_inner_start,
                end: body_inner_end,
            }),
        )
    })
}

/// Classify a tree-sitter node as a `NodeKind` if it represents a top-level
/// declaration in the given language. Returns `Ok(None)` when the node is
/// uninteresting or has no recoverable name.
fn classify(node: Node, lang: Language, src: &[u8]) -> Result<Option<(String, NodeKind)>, ExtractError> {
    let Some(kind) = node_kind_for(lang, node.kind()) else {
        return Ok(None);
    };
    // Most declarations expose a `name` field. C and C++ `function_definition`
    // do not — the name lives at the end of a `declarator` chain — so fall back
    // to a declarator walk for those.
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => match c_cpp_declarator_name(node, lang) {
            Some(n) => n,
            None => return Ok(None),
        },
    };
    let name = std::str::from_utf8(&src[name_node.start_byte()..name_node.end_byte()])?.to_owned();
    Ok(Some((name, kind)))
}

/// Recover a C/C++ function name by walking the `declarator` field chain
/// (`function_declarator` → `pointer_declarator` → … → `identifier`). Returns
/// `None` for any other language, or when no name-bearing node is reached.
fn c_cpp_declarator_name(node: Node, lang: Language) -> Option<Node> {
    if !matches!(lang, Language::C | Language::Cpp) {
        return None;
    }
    let mut cur = node.child_by_field_name("declarator")?;
    loop {
        if matches!(
            cur.kind(),
            "identifier"
                | "field_identifier"
                | "qualified_identifier"
                | "destructor_name"
                | "operator_name"
        ) {
            return Some(cur);
        }
        match cur.child_by_field_name("declarator") {
            // Always descends to a child, so the walk terminates.
            Some(next) => cur = next,
            None => return cur.child_by_field_name("name"),
        }
    }
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
            | (
                Language::Python | Language::C | Language::Cpp | Language::Bash,
                "function_definition"
            ),
    );
    if is_function {
        return Some(NodeKind::Function);
    }
    // Methods across languages.
    let is_method = matches!(
        (lang, ts_kind),
        (Language::TypeScript, "method_definition")
            | (
                Language::Go | Language::Java | Language::CSharp,
                "method_declaration"
            )
            | (Language::Ruby, "method" | "singleton_method"),
    );
    if is_method {
        return Some(NodeKind::Method);
    }
    // Structs / record-shaped declarations.
    let is_struct = matches!(
        (lang, ts_kind),
        (Language::Rust, "struct_item")
            | (Language::Go, "type_declaration")
            | (Language::C | Language::Cpp, "struct_specifier")
            | (Language::C, "enum_specifier" | "type_definition")
            | (Language::CSharp, "struct_declaration"),
    );
    if is_struct {
        return Some(NodeKind::Struct);
    }
    // Classes.
    let is_class = matches!(
        (lang, ts_kind),
        (
            Language::TypeScript | Language::Java | Language::CSharp,
            "class_declaration"
        ) | (Language::Python, "class_definition")
            | (Language::Cpp, "class_specifier")
            | (Language::CSharp, "record_declaration" | "enum_declaration")
            | (Language::Ruby, "class"),
    );
    if is_class {
        return Some(NodeKind::Class);
    }
    // Traits (Rust only).
    if matches!((lang, ts_kind), (Language::Rust, "trait_item")) {
        return Some(NodeKind::Trait);
    }
    // Interfaces (TS / Java / C#).
    if matches!(
        (lang, ts_kind),
        (
            Language::TypeScript | Language::Java | Language::CSharp,
            "interface_declaration"
        ),
    ) {
        return Some(NodeKind::Interface);
    }
    // Modules / namespaces.
    if matches!(
        (lang, ts_kind),
        (Language::Rust, "mod_item")
            | (Language::Cpp, "namespace_definition")
            | (Language::CSharp, "namespace_declaration")
            | (Language::Ruby, "module"),
    ) {
        return Some(NodeKind::Module);
    }
    None
}
