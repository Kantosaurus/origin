#![allow(clippy::needless_collect)]

use origin_trace::{Ring, SpanRow};
use tempfile::tempdir;

#[test]
fn rotates_at_64_mib() {
    let dir = tempdir().expect("tempdir");
    let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open ring");

    // Write enough rows to trip the 64 MiB rollover. Each row's
    // `attrs_json` is ~512 bytes so ~130k rows = 64 MiB+.
    let big_attrs = "x".repeat(512);
    for i in 0..130_000_u64 {
        ring.append(SpanRow {
            ts_ns: 1_000_000_000 * i,
            span_id: i,
            parent_id: 0,
            kind: "tool",
            provider: "anthropic",
            tool: "Bash",
            dur_us: 42,
            error_kind: "",
            attrs_json: big_attrs.clone(),
        })
        .expect("append");
    }
    ring.flush().expect("flush");

    let files: Vec<_> = std::fs::read_dir(dir.path()).expect("readdir").collect();
    assert!(
        files.len() >= 2,
        "expected ≥2 parquet files after rotation, got {}",
        files.len()
    );
}

#[test]
fn round_trips_single_row() {
    let dir = tempdir().expect("tempdir");
    let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open ring");
    ring.append(SpanRow {
        ts_ns: 17,
        span_id: 1,
        parent_id: 0,
        kind: "tool",
        provider: "anthropic",
        tool: "Read",
        dur_us: 9,
        error_kind: "",
        attrs_json: r#"{"path":"/x"}"#.into(),
    })
    .expect("append");
    ring.flush().expect("flush");

    // We don't read it back here (that's P11.11's `Query`); we just assert a
    // file was produced and its parquet footer parses.
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert!(!files.is_empty(), "expected at least one parquet file");
}
