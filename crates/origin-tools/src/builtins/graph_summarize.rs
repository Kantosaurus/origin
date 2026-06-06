// SPDX-License-Identifier: Apache-2.0
//! `graph_summarize` — neighborhood summary for a target code-graph node.
//!
//! Given a target node (passed as a lowercase 64-char hex entity id), this
//! returns the target plus its immediate (depth-1) neighbourhood as a populated
//! [`QueryResult::Nodes`] bag. The first row is always the target itself; the
//! remaining rows are its direct callees/refs (out-edges). The caller can derive
//! callees/refs counts and top-neighbour names directly from the node list.
//!
//! An unknown / unresolvable target (no matching `code_nodes` row) yields
//! [`QueryResult::Empty`] — there is nothing to summarize.

use origin_codegraph::index::{CodeGraphIndex, EntityId};
use origin_codegraph::query::{QueryError, QueryResult};

/// Maximum number of immediate neighbours surfaced in a summary. Keeps the
/// rendered result tight even for a high-fan-out "god node".
const MAX_NEIGHBORS: usize = 32;

/// Summarize the immediate neighbourhood of `target` against `idx`.
///
/// `target` is a lowercase hex entity id (the `node` arg the agent passes). The
/// result is `Nodes([target, neighbor_1, …])`: the target node first, followed
/// by up to [`MAX_NEIGHBORS`] of its direct out-edge targets (callees/refs),
/// de-duplicated and in stable id order. Returns [`QueryResult::Empty`] when the
/// target hex is malformed or names no node in the graph.
///
/// # Errors
/// Propagates [`QueryError`] from the underlying `SQLite` / index reads.
#[allow(clippy::module_name_repetitions)] // `graph_summarize_tool` follows `recall_tool` precedent
pub fn graph_summarize_tool(idx: &CodeGraphIndex, target: &str) -> Result<QueryResult, QueryError> {
    let Some(node) = parse_entity_id(target) else {
        return Ok(QueryResult::Empty);
    };
    summarize(idx, node)
}

/// Parse a lowercase 64-char hex string into an [`EntityId`]. Returns `None`
/// for any input that is not exactly 32 bytes of hex (e.g. a community id, a
/// truncated handle, or empty input).
fn parse_entity_id(target: &str) -> Option<EntityId> {
    let mut buf = [0u8; 32];
    hex::decode_to_slice(target, &mut buf).ok()?;
    Some(EntityId(buf))
}

/// Build the neighbourhood summary: target row first, then its direct
/// out-edge targets. The target must resolve to a real node or we return Empty.
fn summarize(idx: &CodeGraphIndex, target: EntityId) -> Result<QueryResult, QueryError> {
    let Some(target_row) = fetch_node(idx, target)? else {
        return Ok(QueryResult::Empty);
    };
    let mut out = vec![target_row];
    let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    seen.insert(target.0);
    for edge in idx.edges_from(target)? {
        if out.len() > MAX_NEIGHBORS {
            break;
        }
        if !seen.insert(edge.to.0) {
            continue;
        }
        if let Some(row) = fetch_node(idx, edge.to)? {
            out.push(row);
        }
    }
    Ok(QueryResult::Nodes(out))
}

/// Fetch a single node row by id, mirroring the query module's read shape.
fn fetch_node(
    idx: &CodeGraphIndex,
    id: EntityId,
) -> Result<Option<origin_codegraph::index::NodeRow>, QueryError> {
    let raw = idx.with_store(|conn| {
        let mut stmt = conn.prepare(
            "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
             FROM code_nodes WHERE entity_id = ?1 LIMIT 1",
        )?;
        let mut it = stmt.query_map([&id.0[..]], |row| {
            let entity: Vec<u8> = row.get(0)?;
            let kind: String = row.get(1)?;
            let name: String = row.get(2)?;
            let file_path: String = row.get(3)?;
            let sig_h: Vec<u8> = row.get(4)?;
            let body_h: Vec<u8> = row.get(5)?;
            Ok((entity, kind, name, file_path, sig_h, body_h))
        })?;
        it.next().map_or(Ok(None), |r| r.map(Some))
    })?;
    let Some((entity, kind, name, file_path, sig_h, body_h)) = raw else {
        return Ok(None);
    };
    Ok(Some(origin_codegraph::index::NodeRow {
        entity_id: EntityId(to32(&entity)?),
        kind,
        name,
        file_path,
        signature_handle: to32(&sig_h)?,
        body_handle: to32(&body_h)?,
    }))
}

/// Convert a BLOB slice into a fixed 32-byte handle, surfacing a malformed
/// shape as an index error rather than panicking.
fn to32(bytes: &[u8]) -> Result<[u8; 32], QueryError> {
    <[u8; 32]>::try_from(bytes)
        .map_err(|_| QueryError::Index(origin_codegraph::index::IndexError::HandleShape(bytes.len())))
}

crate::origin_tool! {
    name: "graph_summarize",
    description: "Summarize a community ({ community_id }) or a node neighborhood ({ node }). Returns CAS-handled bullets.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "community_id": {"type": "integer"},
            "node": {"type": "string", "description": "Lowercase hex entity id (64 chars)."}
        }
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
