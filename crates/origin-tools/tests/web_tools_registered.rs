use origin_tools::registry_iter;

#[test]
fn web_fetch_and_web_search_and_browser_registered() {
    let names: Vec<&str> = registry_iter().map(|m| m.name).collect();
    for want in ["WebFetch", "WebSearch", "Browser"] {
        assert!(
            names.contains(&want),
            "missing tool registration: {want}; got {names:?}"
        );
    }
}
