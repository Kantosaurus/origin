//! Louvain (modularity-greedy) clustering with a Leiden-style refinement
//! pass that splits disconnected communities, followed by flow-weighted
//! `PageRank` per cluster (P7.5, N6.9).
//!
//! Edges are folded to an undirected weighted graph using:
//! `Calls=3.0`, `Implements|Extends=2.0`, `Mentions=1.0`, multiplied by
//! confidence (`Extracted=1.0`, `Inferred=0.5`, `Ambiguous=0.25`).
//!
//! The algorithm is implemented over plain `HashMap` adjacency lists; no
//! `petgraph` types are referenced (the dep is declared per the Phase 7
//! plan for future graph-algorithm work).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::extract::EdgeKind;
use crate::record::Confidence;

/// Inputs to the community-detection pipeline.
#[derive(Debug, Clone)]
pub struct GraphInput {
    pub nodes: Vec<u64>,
    pub edges: Vec<(u64, u64, EdgeKind, Confidence)>,
}

/// `PageRank` knobs.
#[derive(Debug, Clone, Copy)]
pub struct PageRankOpts {
    pub damping: f64,
    pub iterations: usize,
}

impl Default for PageRankOpts {
    fn default() -> Self {
        Self {
            damping: 0.85,
            iterations: 50,
        }
    }
}

/// A single community.
#[derive(Debug, Clone)]
pub struct Partition {
    pub members: Vec<u64>,
    pub pagerank: HashMap<u64, f64>,
}

/// Result of running [`communities`].
#[derive(Debug, Clone)]
// Public API name explicitly chosen to be self-describing outside the module.
#[allow(clippy::module_name_repetitions)]
pub struct CommunityResult {
    pub partitions: Vec<Partition>,
    pub modularity: f64,
}

impl CommunityResult {
    /// Returns the top-`n` nodes by `PageRank` in each partition.
    #[must_use]
    pub fn god_nodes_top_per_partition(&self, n: usize) -> Vec<Vec<u64>> {
        self.partitions
            .iter()
            .map(|p| {
                let mut ranked: Vec<(u64, f64)> = p.pagerank.iter().map(|(k, v)| (*k, *v)).collect();
                ranked.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0))
                });
                ranked.into_iter().take(n).map(|(k, _)| k).collect()
            })
            .collect()
    }
}

/// Edge-weight folding per the Phase 7 plan.
///
/// Both `EdgeKind` and `Confidence` are `Copy` enums, so a `match`
/// is allowed in a `const fn` on Rust 1.83.
#[must_use]
pub const fn edge_weight(kind: EdgeKind, confidence: Confidence) -> f64 {
    let base = match kind {
        EdgeKind::Calls => 3.0,
        EdgeKind::Implements | EdgeKind::Extends => 2.0,
        EdgeKind::Mentions => 1.0,
    };
    let mult = match confidence {
        Confidence::Extracted => 1.0,
        Confidence::Inferred => 0.5,
        Confidence::Ambiguous => 0.25,
    };
    base * mult
}

type Adj = HashMap<u64, HashMap<u64, f64>>;

/// Build the undirected weighted adjacency. Parallel edges sum.
fn build_adjacency(input: &GraphInput) -> Adj {
    let mut adj: Adj = HashMap::with_capacity(input.nodes.len());
    for &n in &input.nodes {
        adj.entry(n).or_default();
    }
    for &(u, v, kind, conf) in &input.edges {
        if u == v {
            continue;
        }
        let w = edge_weight(kind, conf);
        if w == 0.0 {
            continue;
        }
        *adj.entry(u).or_default().entry(v).or_insert(0.0) += w;
        *adj.entry(v).or_default().entry(u).or_insert(0.0) += w;
    }
    adj
}

/// Strength (sum of incident weights) per node.
fn node_strengths(adj: &Adj) -> HashMap<u64, f64> {
    adj.iter().map(|(n, nbrs)| (*n, nbrs.values().sum())).collect()
}

/// Run one full Louvain pass; returns `community_of[node]`.
fn louvain(adj: &Adj) -> HashMap<u64, u64> {
    // Each node starts in its own community (id = node id).
    let mut comm: HashMap<u64, u64> = adj.keys().map(|&n| (n, n)).collect();
    let mut nodes: Vec<u64> = adj.keys().copied().collect();
    nodes.sort_unstable();

    let strength = node_strengths(adj);
    let total_w: f64 = strength.values().sum::<f64>() * 0.5;
    if total_w == 0.0 {
        return comm;
    }
    let two_m = total_w * 2.0;

    // Community total strength (Σ_tot in Louvain notation).
    let mut sigma_tot: HashMap<u64, f64> = HashMap::new();
    for (&n, &c) in &comm {
        *sigma_tot.entry(c).or_insert(0.0) += strength.get(&n).copied().unwrap_or(0.0);
    }

    let mut moved = true;
    let mut guard = 0usize;
    while moved && guard < 50 {
        moved = false;
        guard += 1;
        for &node in &nodes {
            let k_i = strength.get(&node).copied().unwrap_or(0.0);
            let current = comm.get(&node).copied().unwrap_or(node);

            // Sum of weights from node to each neighboring community.
            let mut to_comm: HashMap<u64, f64> = HashMap::new();
            if let Some(nbrs) = adj.get(&node) {
                for (&nbr, &w) in nbrs {
                    if nbr == node {
                        continue;
                    }
                    let c = comm.get(&nbr).copied().unwrap_or(nbr);
                    *to_comm.entry(c).or_insert(0.0) += w;
                }
            }
            // Remove node from its current community for fair comparison.
            if let Some(s) = sigma_tot.get_mut(&current) {
                *s -= k_i;
            }

            // Pick the community maximizing ΔQ.
            let mut best_comm = current;
            let mut best_gain = 0.0_f64;
            let mut candidates: Vec<u64> = to_comm.keys().copied().collect();
            candidates.sort_unstable();
            for cand in candidates {
                let k_i_in = to_comm.get(&cand).copied().unwrap_or(0.0);
                let sigma = sigma_tot.get(&cand).copied().unwrap_or(0.0);
                // ΔQ ∝ k_i_in - sigma_tot * k_i / m  (constants dropped).
                let gain = k_i_in - sigma * k_i / two_m;
                if gain > best_gain + 1e-12 {
                    best_gain = gain;
                    best_comm = cand;
                }
            }

            // Re-insert (possibly into a new community).
            *sigma_tot.entry(best_comm).or_insert(0.0) += k_i;
            if best_comm == current {
                // No move this round.
            } else {
                comm.insert(node, best_comm);
                moved = true;
            }
        }
    }

    comm
}

/// Refinement: split any community whose induced subgraph is disconnected
/// (Leiden's distinguishing property).
fn refine_disconnected(adj: &Adj, comm: &HashMap<u64, u64>) -> HashMap<u64, u64> {
    let mut buckets: HashMap<u64, Vec<u64>> = HashMap::new();
    for (&n, &c) in comm {
        buckets.entry(c).or_default().push(n);
    }
    let mut refined: HashMap<u64, u64> = HashMap::with_capacity(comm.len());
    let mut next_id: u64 = comm.values().copied().max().unwrap_or(0).saturating_add(1);

    let mut bucket_ids: Vec<u64> = buckets.keys().copied().collect();
    bucket_ids.sort_unstable();
    for cid in bucket_ids {
        let members = buckets.remove(&cid).unwrap_or_default();
        let member_set: HashSet<u64> = members.iter().copied().collect();
        let mut visited: HashSet<u64> = HashSet::new();
        let mut comp_idx: u64 = 0;

        let mut roots: Vec<u64> = members.clone();
        roots.sort_unstable();
        for root in roots {
            if visited.contains(&root) {
                continue;
            }
            let label = if comp_idx == 0 {
                cid
            } else {
                let id = next_id;
                next_id = next_id.saturating_add(1);
                id
            };
            comp_idx += 1;

            // BFS over the induced subgraph of `member_set`.
            let mut queue: VecDeque<u64> = VecDeque::new();
            queue.push_back(root);
            visited.insert(root);
            refined.insert(root, label);
            while let Some(x) = queue.pop_front() {
                if let Some(nbrs) = adj.get(&x) {
                    let mut sorted: Vec<u64> = nbrs.keys().copied().collect();
                    sorted.sort_unstable();
                    for y in sorted {
                        if !member_set.contains(&y) || visited.contains(&y) {
                            continue;
                        }
                        visited.insert(y);
                        refined.insert(y, label);
                        queue.push_back(y);
                    }
                }
            }
        }
    }
    refined
}

/// Modularity of a partition over the full weighted graph.
#[allow(clippy::cast_precision_loss)] // small cluster sizes, no precision concern
fn modularity(adj: &Adj, comm: &HashMap<u64, u64>) -> f64 {
    let strength = node_strengths(adj);
    let m = strength.values().sum::<f64>() * 0.5;
    if m == 0.0 {
        return 0.0;
    }
    let two_m = m * 2.0;
    let mut q = 0.0_f64;
    for (&u, nbrs) in adj {
        let cu = comm.get(&u).copied().unwrap_or(u);
        let ku = strength.get(&u).copied().unwrap_or(0.0);
        for (&v, &a_uv) in nbrs {
            let cv = comm.get(&v).copied().unwrap_or(v);
            if cu != cv {
                continue;
            }
            let kv = strength.get(&v).copied().unwrap_or(0.0);
            q += a_uv - (ku * kv) / two_m;
        }
        // Diagonal (i == j) term of the modularity sum. A node is always in its
        // own community, and the inner loop above only visits u's neighbours —
        // never the (u, u) pair — so the null-model contribution `-k_u^2/2m`
        // (with A_uu = 0, since `build_adjacency` skips self-loops) would be
        // dropped, systematically overestimating Q. Add it explicitly.
        let _ = cu;
        q -= (ku * ku) / two_m;
    }
    q / two_m
}

/// Weighted `PageRank` inside an induced subgraph (cluster).
#[allow(clippy::cast_precision_loss)] // small cluster sizes, no precision concern
fn pagerank_cluster(members: &[u64], adj: &Adj, opts: PageRankOpts) -> HashMap<u64, f64> {
    let n = members.len();
    if n == 0 {
        return HashMap::new();
    }
    let member_set: HashSet<u64> = members.iter().copied().collect();
    let base = 1.0 / n as f64;
    let mut pr: HashMap<u64, f64> = members.iter().map(|&m| (m, base)).collect();

    // Pre-compute restricted out-weight sums.
    let mut out_sum: HashMap<u64, f64> = HashMap::with_capacity(n);
    let mut restricted_adj: HashMap<u64, Vec<(u64, f64)>> = HashMap::with_capacity(n);
    for &u in members {
        let mut s = 0.0_f64;
        let mut row: Vec<(u64, f64)> = Vec::new();
        if let Some(nbrs) = adj.get(&u) {
            for (&v, &w) in nbrs {
                if member_set.contains(&v) {
                    row.push((v, w));
                    s += w;
                }
            }
        }
        out_sum.insert(u, s);
        restricted_adj.insert(u, row);
    }

    let d = opts.damping;
    let teleport = (1.0 - d) / n as f64;
    for _ in 0..opts.iterations {
        let mut next: HashMap<u64, f64> = members.iter().map(|&m| (m, teleport)).collect();
        // Dangling mass: nodes with zero out-sum distribute uniformly.
        let mut dangling_mass = 0.0_f64;
        for &u in members {
            let pu = pr.get(&u).copied().unwrap_or(0.0);
            let s = out_sum.get(&u).copied().unwrap_or(0.0);
            if s == 0.0 {
                dangling_mass += pu;
                continue;
            }
            if let Some(row) = restricted_adj.get(&u) {
                for &(v, w) in row {
                    if let Some(slot) = next.get_mut(&v) {
                        *slot += d * pu * w / s;
                    }
                }
            }
        }
        let dangle_share = d * dangling_mass / n as f64;
        if dangle_share != 0.0 {
            for v in members {
                if let Some(slot) = next.get_mut(v) {
                    *slot += dangle_share;
                }
            }
        }
        pr = next;
    }
    pr
}

/// Run Louvain + refinement + per-cluster `PageRank`.
#[must_use]
// Public entry takes ownership to match the documented Phase 7 API even though
// internals borrow; consumers typically build `GraphInput` once and hand it off.
#[allow(clippy::needless_pass_by_value)]
pub fn communities(input: GraphInput, opts: PageRankOpts) -> CommunityResult {
    let adj = build_adjacency(&input);

    // Empty graph guard.
    if adj.is_empty() {
        return CommunityResult {
            partitions: Vec::new(),
            modularity: 0.0,
        };
    }

    let comm = louvain(&adj);
    let refined = refine_disconnected(&adj, &comm);
    let modu = modularity(&adj, &refined);

    // Group into partitions.
    let mut groups: HashMap<u64, Vec<u64>> = HashMap::new();
    for (&n, &c) in &refined {
        groups.entry(c).or_default().push(n);
    }
    let mut partitions: Vec<Partition> = groups
        .into_values()
        .map(|mut members| {
            members.sort_unstable();
            let pagerank = pagerank_cluster(&members, &adj, opts);
            Partition { members, pagerank }
        })
        .collect();
    partitions.sort_by(|a, b| {
        let lhs = a.members.first().copied().unwrap_or(u64::MAX);
        let rhs = b.members.first().copied().unwrap_or(u64::MAX);
        lhs.cmp(&rhs)
    });

    CommunityResult {
        partitions,
        modularity: modu,
    }
}
