use origin_codegraph::index::EntityId;
use origin_codegraph::query::Query;
use origin_tools::builtins::graph_explain::graph_explain_tool;

#[test]
fn explains_path_query() {
    let from = EntityId([0xab; 32]);
    let to = EntityId([0xcd; 32]);
    let q = Query::Path { from, to, max_hops: 5 };
    let out = graph_explain_tool(&q);
    assert_eq!(out, "shortest path from abababab to cdcdcdcd within 5 hops");
}

#[test]
fn explains_neighbors_query() {
    let node = EntityId([0x12; 32]);
    let q = Query::Neighbors { node, depth: 3 };
    let out = graph_explain_tool(&q);
    assert_eq!(out, "neighbors of 12121212 up to depth 3");
}

#[test]
fn explains_communities_query() {
    let out = graph_explain_tool(&Query::Communities);
    assert_eq!(out, "all detected communities");
}

#[test]
fn explains_god_nodes_query() {
    let q = Query::GodNodes { top_per_partition: 7 };
    let out = graph_explain_tool(&q);
    assert_eq!(out, "top 7 god-nodes per community");
}

#[test]
fn explains_recent_changes_query() {
    let q = Query::RecentChanges { since_ms: 1_700_000_000_000 };
    let out = graph_explain_tool(&q);
    assert_eq!(out, "nodes changed since unix-ms 1700000000000");
}
