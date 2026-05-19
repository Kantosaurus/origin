//! Typed query DSL — no NL, no in-tool LLM hop (P7.6 N6.10).
//!
//! Callers compose a [`Query`] enum value and hand it to [`dispatch`]; the
//! dispatcher walks the [`CodeGraphIndex`] using its existing SQL/edge
//! primitives. Variants that depend on community-detection output
//! (`Communities`, `GodNodes`) intentionally stub to
//! [`QueryResult::Empty`] for Phase 7 — the reads side of community storage
//! lands in a follow-up task.

use std::collections::{HashMap, HashSet, VecDeque};

use rusqlite::params;
use thiserror::Error;

use crate::index::{CodeGraphIndex, EntityId, IndexError, NodeRow};

/// Typed query against a [`CodeGraphIndex`]. Each variant maps to one
/// dispatch arm in [`dispatch`]; there is no NL or LLM round-trip.
#[derive(Debug, Clone)]
pub enum Query {
    /// Shortest path of nodes (`from`, …, `to`) with at most `max_hops`
    /// edges between them.
    Path {
        from: EntityId,
        to: EntityId,
        max_hops: usize,
    },
    /// Breadth-first reachable set from `node` up to `depth` hops away.
    Neighbors { node: EntityId, depth: usize },
    /// All communities (stub in Phase 7 — returns [`QueryResult::Empty`]).
    Communities,
    /// Top-`top_per_partition` god-nodes per community (stub in Phase 7 —
    /// returns [`QueryResult::Empty`]).
    GodNodes { top_per_partition: usize },
    /// Nodes whose `last_seen` is at or after `since_ms` (unix epoch ms),
    /// newest first.
    RecentChanges { since_ms: i64 },
}

/// Result of dispatching a [`Query`].
// `QueryResult` is the plan-mandated public name; the `Query` prefix
// disambiguates against `rusqlite`'s own `Result`/`Rows` types at call sites.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub enum QueryResult {
    /// Unordered set of nodes (e.g. neighbor set, recent-changes list).
    Nodes(Vec<NodeRow>),
    /// Ordered chain of nodes from `from` to `to` (inclusive).
    Path(Vec<NodeRow>),
    /// One node bag per community partition.
    Partitions(Vec<Vec<NodeRow>>),
    /// No result (path not found, stub variant, etc.).
    Empty,
}

impl QueryResult {
    /// Whether the result carries no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Nodes(v) | Self::Path(v) => v.is_empty(),
            Self::Partitions(parts) => parts.iter().all(Vec::is_empty),
        }
    }
}

/// Errors surfaced by [`dispatch`].
// `QueryError` matches the Phase 7 plan's public API and parallels
// `IndexError`, `LangError` in this crate.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum QueryError {
    #[error("index: {0}")]
    Index(#[from] IndexError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Dispatch a [`Query`] against `idx`.
///
/// # Errors
/// Propagates index errors (CAS / `SQLite` / decode) and ad-hoc `SQLite`
/// errors from query-only SQL.
// `Query` owns heap data (`EntityId` is `Copy`, but the enum is `Clone`-not-
// `Copy` to leave room for future variants holding `String`/`Vec`). Taking it
// by value matches the Phase 7 plan signature and avoids forcing callers to
// keep the value alive past dispatch.
#[allow(clippy::needless_pass_by_value)]
pub fn dispatch(idx: &CodeGraphIndex, q: Query) -> Result<QueryResult, QueryError> {
    match q {
        Query::Neighbors { node, depth } => neighbors(idx, node, depth),
        Query::Path { from, to, max_hops } => path(idx, from, to, max_hops),
        Query::RecentChanges { since_ms } => recent_changes(idx, since_ms),
        Query::Communities | Query::GodNodes { .. } => Ok(QueryResult::Empty),
    }
}

fn neighbors(idx: &CodeGraphIndex, start: EntityId, depth: usize) -> Result<QueryResult, QueryError> {
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    let mut queue: VecDeque<(EntityId, usize)> = VecDeque::new();
    let mut out: Vec<NodeRow> = Vec::new();
    seen.insert(start.0);
    queue.push_back((start, 0));
    while let Some((n, hops)) = queue.pop_front() {
        if hops >= depth {
            continue;
        }
        for e in idx.edges_from(n)? {
            if seen.insert(e.to.0) {
                if let Some(row) = fetch_node(idx, e.to)? {
                    out.push(row);
                }
                queue.push_back((e.to, hops + 1));
            }
        }
    }
    Ok(QueryResult::Nodes(out))
}

fn path(
    idx: &CodeGraphIndex,
    from: EntityId,
    to: EntityId,
    max_hops: usize,
) -> Result<QueryResult, QueryError> {
    if from.0 == to.0 {
        return Ok(fetch_node(idx, from)?.map_or(QueryResult::Empty, |row| QueryResult::Path(vec![row])));
    }
    let mut parents: HashMap<[u8; 32], [u8; 32]> = HashMap::new();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    let mut queue: VecDeque<(EntityId, usize)> = VecDeque::new();
    seen.insert(from.0);
    queue.push_back((from, 0));
    let mut found = false;
    while let Some((n, hops)) = queue.pop_front() {
        if n.0 == to.0 {
            found = true;
            break;
        }
        if hops >= max_hops {
            continue;
        }
        for e in idx.edges_from(n)? {
            if e.to.0 == from.0 {
                continue;
            }
            if seen.insert(e.to.0) {
                parents.insert(e.to.0, n.0);
                if e.to.0 == to.0 {
                    found = true;
                    break;
                }
                queue.push_back((e.to, hops + 1));
            }
        }
        if found {
            break;
        }
    }
    if !found {
        return Ok(QueryResult::Empty);
    }
    let mut chain: Vec<[u8; 32]> = vec![to.0];
    let mut cur = to.0;
    while cur != from.0 {
        let Some(&p) = parents.get(&cur) else {
            return Ok(QueryResult::Empty);
        };
        chain.push(p);
        cur = p;
    }
    chain.reverse();
    let mut rows: Vec<NodeRow> = Vec::with_capacity(chain.len());
    for id in chain {
        if let Some(row) = fetch_node(idx, EntityId(id))? {
            rows.push(row);
        }
    }
    Ok(QueryResult::Path(rows))
}

fn recent_changes(idx: &CodeGraphIndex, since_ms: i64) -> Result<QueryResult, QueryError> {
    let raw = idx.with_store(|conn| {
        let mut stmt = conn.prepare(
            "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
             FROM code_nodes
             WHERE last_seen >= ?1
             ORDER BY last_seen DESC",
        )?;
        let it = stmt.query_map(params![since_ms], |row| {
            let entity: Vec<u8> = row.get(0)?;
            let kind: String = row.get(1)?;
            let name: String = row.get(2)?;
            let file_path: String = row.get(3)?;
            let sig_h: Vec<u8> = row.get(4)?;
            let body_h: Vec<u8> = row.get(5)?;
            Ok((entity, kind, name, file_path, sig_h, body_h))
        })?;
        it.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut out = Vec::with_capacity(raw.len());
    for (entity, kind, name, file_path, sig_h, body_h) in raw {
        out.push(NodeRow {
            entity_id: EntityId(to32(&entity)?),
            kind,
            name,
            file_path,
            signature_handle: to32(&sig_h)?,
            body_handle: to32(&body_h)?,
        });
    }
    Ok(QueryResult::Nodes(out))
}

fn fetch_node(idx: &CodeGraphIndex, id: EntityId) -> Result<Option<NodeRow>, QueryError> {
    let raw = idx.with_store(|conn| {
        let mut stmt = conn.prepare(
            "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
             FROM code_nodes WHERE entity_id = ?1 LIMIT 1",
        )?;
        let mut it = stmt.query_map(params![&id.0[..]], |row| {
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
    Ok(Some(NodeRow {
        entity_id: EntityId(to32(&entity)?),
        kind,
        name,
        file_path,
        signature_handle: to32(&sig_h)?,
        body_handle: to32(&body_h)?,
    }))
}

fn to32(bytes: &[u8]) -> Result<[u8; 32], QueryError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| QueryError::Index(IndexError::HandleShape(bytes.len())))
}
