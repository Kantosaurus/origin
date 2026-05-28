use origin_tools::builtins::tool_search::{tool_search, ToolSearchArgs};

#[test]
fn returns_deferred_tool_schema_by_exact_name() {
    let out = tool_search(&ToolSearchArgs { query: "select:Recall".into(), max_results: None }).unwrap();
    let arr = out.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "Recall");
    assert!(arr[0].get("input_schema").is_some());
}

#[test]
fn returns_multiple_by_select_list() {
    let out = tool_search(&ToolSearchArgs { query: "select:Recall,ask".into(), max_results: None }).unwrap();
    assert_eq!(out.as_array().unwrap().len(), 2);
}

#[test]
fn keyword_search_ranks_by_relevance() {
    let out = tool_search(&ToolSearchArgs { query: "graph".into(), max_results: Some(3) }).unwrap();
    let arr = out.as_array().unwrap();
    assert!(!arr.is_empty());
    for v in arr {
        assert!(
            v["name"].as_str().unwrap().to_lowercase().contains("graph")
                || v["description"].as_str().unwrap().to_lowercase().contains("graph")
        );
    }
}

#[test]
fn cannot_fetch_hot_tool_via_search() {
    let out = tool_search(&ToolSearchArgs { query: "select:Read".into(), max_results: None }).unwrap();
    assert!(out.as_array().unwrap().is_empty());
}
