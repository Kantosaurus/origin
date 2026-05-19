use origin_permission::bloom::BloomPreCheck;
use origin_permission::rules::Rule;

fn make_rules() -> Vec<Rule> {
    (0..30)
        .map(|i| Rule {
            tool_name: format!("Tool{i}"),
            scope: "default".into(),
            allow: true,
        })
        .collect()
}

#[test]
fn bloom_rejects_at_least_95_percent_of_unrelated_calls() {
    let rules = make_rules();
    let bloom = BloomPreCheck::build(&rules);
    let mut rejected = 0usize;
    for i in 0..1000 {
        let probe = format!("Unrelated{i}@default");
        if !bloom.maybe_contains(&probe) {
            rejected += 1;
        }
    }
    assert!(
        rejected >= 950,
        "expected >=95% of 1000 unrelated probes rejected, got {rejected}"
    );
}

#[test]
fn bloom_matches_brute_force_exactly_for_known_rules() {
    let rules = make_rules();
    let bloom = BloomPreCheck::build(&rules);
    for r in &rules {
        let key = format!("{}@{}", r.tool_name, r.scope);
        assert!(bloom.maybe_contains(&key), "bloom must contain {key}");
    }
}
