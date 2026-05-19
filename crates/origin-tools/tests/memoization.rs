use origin_tools::dispatch::{Cache, NormalizedInput};

#[test]
fn second_lookup_returns_cached_handle() {
    let mut cache = Cache::new();
    let key = NormalizedInput::hash("Read", br#"{"path":"/etc/passwd"}"#);
    let h = [7u8; 32];
    assert!(cache.lookup(&key).is_none());
    cache.record(key.clone(), h, 4);
    let hit = cache.lookup(&key).expect("hit");
    assert_eq!(hit.handle, h);
    assert_eq!(hit.from_turn, 4);
}

#[test]
fn bash_normalization_is_never_inserted() {
    let cache = Cache::new();
    assert!(cache.is_skipped("Bash"));
    assert!(cache.is_skipped("Edit"));
    assert!(cache.is_skipped("Write"));
    assert!(!cache.is_skipped("Read"));
}

#[test]
fn byte_equivalent_normalization_means_whitespace_matters() {
    // Phase 3 uses byte-equivalent normalization: identical input bytes
    // produce identical keys. Tool-specific normalization is in scope for
    // Phase 10. So slight whitespace differences should produce different
    // keys at this stage.
    let a = NormalizedInput::hash("Read", br#"{"path":"/etc/passwd"}"#);
    let b = NormalizedInput::hash("Read", br#"{ "path" : "/etc/passwd" }"#);
    assert_ne!(a, b);
}
