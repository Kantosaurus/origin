//! Typed query DSL — no NL, no in-tool LLM hop (P7.6 N6.10).
//!
//! Callers compose a [`Query`] enum value and hand it to [`dispatch`]; the
//! dispatcher walks the [`CodeGraphIndex`] using its existing SQL/edge
//! primitives.
//!
//! `Communities` runs Label Propagation (LPA) directly over the edge table.
//! LPA is O(E) per sweep and converges in ~5 sweeps on typical call graphs,
//! which keeps the read path lean compared to a Louvain/Leiden offline build
//! (Louvain lives in [`crate::community`] for the heavier-weight rebuild
//! pipeline). The whole edge list is hashed with blake3 into an
//! `edge_snapshot_hash`; future revisions of this module can persist that
//! hash alongside the partition assignment in `code_communities` to skip the
//! recompute when nothing changed. `GodNodes` ranks each community's members
//! by in-degree (top-`top_per_partition`).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

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
    /// Community partition of the whole graph via Label Propagation.
    Communities,
    /// Top-`top_per_partition` god-nodes per community, ranked by in-degree.
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
        Query::Communities => communities(idx),
        Query::GodNodes { top_per_partition } => god_nodes(idx, top_per_partition),
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

/// Maximum LPA sweeps before forced termination. LPA usually converges in
/// ~5 passes on real graphs; the cap protects against pathological oscillation.
const LPA_MAX_SWEEPS: usize = 32;

/// Edge-set view used by LPA: undirected, deduped, sorted.
struct EdgeSet {
    /// Stable list of `(min(from,to), max(from,to))` pairs, sorted, deduped.
    /// Sorting makes `edge_snapshot_hash` deterministic across runs.
    sorted: Vec<([u8; 32], [u8; 32])>,
}

impl EdgeSet {
    fn snapshot_hash(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        for (a, b) in &self.sorted {
            h.update(a);
            h.update(b);
        }
        *h.finalize().as_bytes()
    }
}

fn read_edges(idx: &CodeGraphIndex) -> Result<EdgeSet, QueryError> {
    let raw = idx.with_store(|conn| {
        let mut stmt = conn.prepare("SELECT from_id, to_id FROM code_edges")?;
        let it = stmt.query_map([], |row| {
            let f: Vec<u8> = row.get(0)?;
            let t: Vec<u8> = row.get(1)?;
            Ok((f, t))
        })?;
        it.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut pairs: Vec<([u8; 32], [u8; 32])> = Vec::with_capacity(raw.len());
    for (f, t) in raw {
        let a = to32(&f)?;
        let b = to32(&t)?;
        if a == b {
            continue;
        }
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        pairs.push((lo, hi));
    }
    pairs.sort_unstable();
    pairs.dedup();
    Ok(EdgeSet { sorted: pairs })
}

/// Build the undirected adjacency list keyed by `[u8; 32]` entity id.
fn build_adj(edges: &EdgeSet) -> HashMap<[u8; 32], Vec<[u8; 32]>> {
    let mut adj: HashMap<[u8; 32], Vec<[u8; 32]>> = HashMap::new();
    for (a, b) in &edges.sorted {
        adj.entry(*a).or_default().push(*b);
        adj.entry(*b).or_default().push(*a);
    }
    adj
}

/// Synchronous label propagation. Each node's label is the most frequent
/// label among its neighbours; ties broken by lexicographically smallest
/// label so the result is deterministic.
fn label_propagate(adj: &HashMap<[u8; 32], Vec<[u8; 32]>>) -> HashMap<[u8; 32], [u8; 32]> {
    let mut label: HashMap<[u8; 32], [u8; 32]> = adj.keys().map(|k| (*k, *k)).collect();
    // Process nodes in a fixed (sorted) order so the result is independent of
    // HashMap iteration order.
    let mut order: Vec<[u8; 32]> = adj.keys().copied().collect();
    order.sort_unstable();

    for _ in 0..LPA_MAX_SWEEPS {
        let mut next = label.clone();
        let mut changed = false;
        for node in &order {
            let Some(nbrs) = adj.get(node) else { continue };
            if nbrs.is_empty() {
                continue;
            }
            let mut counts: BTreeMap<[u8; 32], usize> = BTreeMap::new();
            for n in nbrs {
                let l = label.get(n).copied().unwrap_or(*n);
                *counts.entry(l).or_insert(0) += 1;
            }
            // Pick label with highest count; ties broken by smallest label
            // (BTreeMap iterates in sorted key order, so the first max wins).
            let mut best: Option<([u8; 32], usize)> = None;
            for (lbl, c) in &counts {
                match best {
                    None => best = Some((*lbl, *c)),
                    Some((_, bc)) if *c > bc => best = Some((*lbl, *c)),
                    _ => {}
                }
            }
            if let Some((lbl, _)) = best {
                if next.get(node).copied() != Some(lbl) {
                    next.insert(*node, lbl);
                    changed = true;
                }
            }
        }
        label = next;
        if !changed {
            break;
        }
    }
    label
}

/// Group nodes by their final LPA label, then materialise each community into
/// a `Vec<NodeRow>` by fetching from `code_nodes`.
///
/// Communities with only one node are kept (a singleton lone function is a
/// legitimate partition). Communities are sorted by the smallest entity id
/// they contain to give deterministic output ordering.
fn partition_to_rows(
    idx: &CodeGraphIndex,
    labels: HashMap<[u8; 32], [u8; 32]>,
) -> Result<Vec<Vec<NodeRow>>, QueryError> {
    let mut buckets: BTreeMap<[u8; 32], Vec<[u8; 32]>> = BTreeMap::new();
    for (n, l) in labels {
        buckets.entry(l).or_default().push(n);
    }
    let mut out: Vec<Vec<NodeRow>> = Vec::with_capacity(buckets.len());
    let mut keyed: Vec<([u8; 32], Vec<[u8; 32]>)> = buckets
        .into_values()
        .map(|mut members| {
            members.sort_unstable();
            let key = members.first().copied().unwrap_or([0xff; 32]);
            (key, members)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, members) in keyed {
        let mut rows = Vec::with_capacity(members.len());
        for m in members {
            if let Some(r) = fetch_node(idx, EntityId(m))? {
                rows.push(r);
            }
        }
        if !rows.is_empty() {
            out.push(rows);
        }
    }
    Ok(out)
}

fn communities(idx: &CodeGraphIndex) -> Result<QueryResult, QueryError> {
    let edges = read_edges(idx)?;
    if edges.sorted.is_empty() {
        return Ok(QueryResult::Partitions(Vec::new()));
    }
    // `_snapshot_hash` is computed here so future revisions can cache the
    // partition assignment in `code_communities` keyed on this value. We
    // bind it explicitly (rather than dropping it) to document the lazy-
    // recompute hook for the next iteration.
    let _snapshot_hash = edges.snapshot_hash();
    let adj = build_adj(&edges);
    let labels = label_propagate(&adj);
    let parts = partition_to_rows(idx, labels)?;
    Ok(QueryResult::Partitions(parts))
}

fn god_nodes(idx: &CodeGraphIndex, top_per_partition: usize) -> Result<QueryResult, QueryError> {
    if top_per_partition == 0 {
        return Ok(QueryResult::Partitions(Vec::new()));
    }
    let in_deg = inbound_degrees(idx)?;
    let QueryResult::Partitions(parts) = communities(idx)? else {
        return Ok(QueryResult::Partitions(Vec::new()));
    };
    let mut out: Vec<Vec<NodeRow>> = Vec::with_capacity(parts.len());
    for mut members in parts {
        members.sort_by(|a, b| {
            let ad = in_deg.get(&a.entity_id.0).copied().unwrap_or(0);
            let bd = in_deg.get(&b.entity_id.0).copied().unwrap_or(0);
            bd.cmp(&ad).then_with(|| a.entity_id.0.cmp(&b.entity_id.0))
        });
        members.truncate(top_per_partition);
        if !members.is_empty() {
            out.push(members);
        }
    }
    Ok(QueryResult::Partitions(out))
}

fn inbound_degrees(idx: &CodeGraphIndex) -> Result<HashMap<[u8; 32], usize>, QueryError> {
    let raw = idx.with_store(|conn| {
        let mut stmt = conn.prepare("SELECT to_id FROM code_edges")?;
        let it = stmt.query_map([], |row| {
            let t: Vec<u8> = row.get(0)?;
            Ok(t)
        })?;
        it.collect::<rusqlite::Result<Vec<_>>>()
    })?;
    let mut deg: HashMap<[u8; 32], usize> = HashMap::new();
    for t in raw {
        let id = to32(&t)?;
        *deg.entry(id).or_insert(0) += 1;
    }
    Ok(deg)
}
