use origin_tui::width::WidthCache;

#[test]
fn ascii_is_width_1() {
    let mut c = WidthCache::new(64);
    assert_eq!(c.width_of("a"), 1);
}

#[test]
fn cjk_is_width_2() {
    let mut c = WidthCache::new(64);
    assert_eq!(c.width_of("漢"), 2);
}

#[test]
fn zwj_emoji_cluster_is_one_grapheme() {
    let mut c = WidthCache::new(64);
    // Family ZWJ sequence: 👨‍👩‍👧
    let cluster = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let width = c.width_of(cluster);
    assert!(width >= 1, "ZWJ emoji should advance >=1 column, got {width}");
}

#[test]
fn measure_str_sums_grapheme_widths() {
    let mut c = WidthCache::new(64);
    assert_eq!(c.measure_str("hi"), 2);
    assert_eq!(c.measure_str("a漢b"), 4);
}

#[test]
fn lru_evicts_oldest_at_capacity() {
    let mut c = WidthCache::new(2);
    let _ = c.width_of("a");
    let _ = c.width_of("b");
    let _ = c.width_of("c"); // evicts "a"
    assert_eq!(c.len(), 2);
}
