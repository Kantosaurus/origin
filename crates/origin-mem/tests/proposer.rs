use origin_mem::proposer::Proposer;

#[test]
fn extracts_remember_directive() {
    let p = Proposer::new();
    let mut next = 1_u32;
    let out = p.scan("remember: I'm a senior Rust engineer", "Sure, noted.", &mut next);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].body, "I'm a senior Rust engineer");
    assert!(out[0].suggested_tags.contains(&"user-statement".to_string()));
    assert_eq!(next, 2);
}

#[test]
fn extracts_preference_phrase() {
    let p = Proposer::new();
    let mut next = 1_u32;
    let out = p.scan("i prefer fewer comments in generated code", "ok.", &mut next);
    assert!(out.iter().any(|m| m.body.contains("i prefer fewer comments")));
}

#[test]
fn no_match_returns_empty() {
    let p = Proposer::new();
    let mut next = 1_u32;
    assert!(p.scan("hello", "hi", &mut next).is_empty());
    assert_eq!(next, 1);
}
