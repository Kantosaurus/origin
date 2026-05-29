// SPDX-License-Identifier: Apache-2.0
use origin_codegraph::community::{communities, GraphInput, PageRankOpts};
use origin_codegraph::extract::EdgeKind;
use origin_codegraph::record::Confidence;

#[test]
fn two_cliques_form_two_communities() {
    let nodes: Vec<u64> = (1..=6).collect();
    let mut edges = Vec::new();
    for &a in &[1u64, 2, 3] {
        for &b in &[1u64, 2, 3] {
            if a != b {
                edges.push((a, b, EdgeKind::Calls, Confidence::Extracted));
            }
        }
    }
    for &a in &[4u64, 5, 6] {
        for &b in &[4u64, 5, 6] {
            if a != b {
                edges.push((a, b, EdgeKind::Calls, Confidence::Extracted));
            }
        }
    }
    edges.push((3, 4, EdgeKind::Mentions, Confidence::Inferred));

    let result = communities(GraphInput { nodes, edges }, PageRankOpts::default());
    assert_eq!(result.partitions.len(), 2, "two communities");
    assert!(
        result.modularity > 0.3,
        "modularity {} should be > 0.3",
        result.modularity
    );

    let gods = result.god_nodes_top_per_partition(1);
    assert_eq!(gods.len(), 2, "one god per partition");
}

#[test]
fn singleton_graph() {
    let result = communities(
        GraphInput {
            nodes: vec![42u64],
            edges: vec![],
        },
        PageRankOpts::default(),
    );
    assert_eq!(result.partitions.len(), 1);
    assert_eq!(result.partitions[0].members, vec![42]);
}
