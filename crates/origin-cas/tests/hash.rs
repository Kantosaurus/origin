use origin_cas::Hash;

#[test]
fn same_bytes_same_hash() {
    let a = Hash::of(b"hello");
    let b = Hash::of(b"hello");
    assert_eq!(a, b);
}

#[test]
fn different_bytes_different_hash() {
    let a = Hash::of(b"hello");
    let b = Hash::of(b"world");
    assert_ne!(a, b);
}

#[test]
fn display_is_lowercase_hex_64_chars() {
    let h = Hash::of(b"x");
    let s = format!("{h}");
    assert_eq!(s.len(), 64);
    assert!(s
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn from_bytes_round_trip() {
    let h = Hash::of(b"y");
    let bytes = *h.as_bytes();
    let h2 = Hash::from_bytes(bytes);
    assert_eq!(h, h2);
}
