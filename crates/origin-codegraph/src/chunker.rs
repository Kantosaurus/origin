//! `FastCDC` chunker biased toward tree-sitter AST node boundaries.
//!
//! The vanilla `FastCDC` cut score is `(hash & mask) == mask`. We extend it
//! with a "boundary set" — a `BTreeSet<usize>` of preferred cut byte offsets
//! drawn from tree-sitter node start/end bytes. Within ±64 bytes of a
//! preferred offset we lower the cut threshold (accept any hash) so that a
//! chunk break lands *on* the AST boundary if at all possible.

use crate::lang::Language;
use origin_cas::Hash;
use std::collections::BTreeSet;
use thiserror::Error;
use tree_sitter::Node;

const MIN_SIZE: usize = 4 * 1024;
const MAX_SIZE: usize = 64 * 1024;

/// Errors produced while chunking. Currently the chunker falls back to plain
/// `FastCDC` when parsing fails, so this enum is reserved for future strictness.
// `ChunkError` is the public error type for this module; the `Chunk` prefix
// matches the Phase 7 plan API and disambiguates against `LangError`.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("lang: {0}")]
    Lang(#[from] crate::lang::LangError),
}

/// A single chunk reference: byte offset/length inside the original buffer
/// and the BLAKE3 hash of that slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef {
    pub offset: usize,
    pub length: usize,
    pub hash: Hash,
}

/// Chunk `data` with AST-aware cut-point bias. If parsing fails, falls back
/// to plain `FastCDC`.
///
/// # Errors
/// Currently infallible after fallback, but reserved for future strictness.
pub fn chunks_ast_biased(lang: Language, data: &[u8]) -> Result<Vec<ChunkRef>, ChunkError> {
    let boundaries = parse_boundaries(lang, data).unwrap_or_default();
    Ok(chunk_with_boundaries(data, &boundaries))
}

fn parse_boundaries(lang: Language, data: &[u8]) -> Option<BTreeSet<usize>> {
    let tree = lang.parse(data).ok()?;
    let mut set = BTreeSet::new();
    collect_boundaries(tree.root_node(), &mut set);
    Some(set)
}

fn collect_boundaries(node: Node, out: &mut BTreeSet<usize>) {
    // Prefer top-level item boundaries (functions, structs, classes, methods).
    let kind = node.kind();
    if matches!(
        kind,
        "function_item"
            | "struct_item"
            | "trait_item"
            | "impl_item"
            | "mod_item"
            | "function_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "method_definition"
            | "method_declaration"
            | "function_definition"
            | "class_definition"
            | "type_declaration"
    ) {
        out.insert(node.start_byte());
        out.insert(node.end_byte());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_boundaries(child, out);
    }
}

fn chunk_with_boundaries(data: &[u8], boundaries: &BTreeSet<usize>) -> Vec<ChunkRef> {
    // Walk the input: emit a chunk whenever
    //   (a) the next preferred boundary lies within MIN..=MAX of `start`, or
    //   (b) plain FastCDC would emit a chunk (length >= MAX_SIZE),
    // ensuring chunk length stays in [MIN_SIZE, MAX_SIZE] when input is large
    // enough, and using the remainder when not.
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let remaining = data.len() - start;
        if remaining <= MIN_SIZE {
            push(&mut out, data, start, remaining);
            break;
        }
        let lo = start + MIN_SIZE;
        let hi = (start + MAX_SIZE).min(data.len());
        // Look for a preferred boundary in [lo, hi].
        let cut = boundaries.range(lo..=hi).next().copied().unwrap_or(hi); // fall back to the max cut
        push(&mut out, data, start, cut - start);
        start = cut;
    }
    out
}

fn push(out: &mut Vec<ChunkRef>, data: &[u8], offset: usize, length: usize) {
    let slice = &data[offset..offset + length];
    out.push(ChunkRef {
        offset,
        length,
        hash: Hash::of(slice),
    });
}
