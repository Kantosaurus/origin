// SPDX-License-Identifier: Apache-2.0
//! P11.11 — parquet pushdown reader for the trace ring.

use origin_trace::query::QueryArgs;
use origin_trace::{Ring, SpanRow};
use tempfile::tempdir;

#[test]
fn query_filters_by_kind_and_error_kind() {
    let dir = tempdir().expect("tempdir");
    {
        let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open");
        ring.append(SpanRow {
            ts_ns: 1,
            span_id: 1,
            parent_id: 0,
            kind: "tool",
            provider: "anthropic",
            tool: "Bash",
            dur_us: 9,
            error_kind: "Sandbox",
            attrs_json: "{}".into(),
        })
        .expect("append");
        ring.append(SpanRow {
            ts_ns: 2,
            span_id: 2,
            parent_id: 0,
            kind: "tool",
            provider: "anthropic",
            tool: "Read",
            dur_us: 7,
            error_kind: "",
            attrs_json: "{}".into(),
        })
        .expect("append");
        ring.append(SpanRow {
            ts_ns: 3,
            span_id: 3,
            parent_id: 0,
            kind: "provider",
            provider: "anthropic",
            tool: "",
            dur_us: 18,
            error_kind: "Sandbox",
            attrs_json: "{}".into(),
        })
        .expect("append");
        ring.flush().expect("flush");
    }

    let rows = origin_trace::query::run(&QueryArgs {
        dir: dir.path().to_path_buf(),
        kind: Some("tool".into()),
        error_kind: Some("Sandbox".into()),
        limit: 100,
    })
    .expect("query");
    assert_eq!(rows.len(), 1, "expected exactly one row matching the predicate");
    assert_eq!(rows[0].span_id, 1);
    assert_eq!(rows[0].tool, "Bash");
}

#[test]
fn query_with_no_filters_returns_every_row_up_to_limit() {
    let dir = tempdir().expect("tempdir");
    {
        let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open");
        for i in 0..5_u64 {
            ring.append(SpanRow {
                ts_ns: i,
                span_id: i,
                parent_id: 0,
                kind: "turn",
                provider: "anthropic",
                tool: "",
                dur_us: 1,
                error_kind: "",
                attrs_json: "{}".into(),
            })
            .expect("append");
        }
        ring.flush().expect("flush");
    }
    let rows = origin_trace::query::run(&QueryArgs {
        dir: dir.path().to_path_buf(),
        kind: None,
        error_kind: None,
        limit: 3,
    })
    .expect("query");
    assert_eq!(rows.len(), 3, "limit must be respected");
}

#[test]
fn query_returns_empty_on_empty_dir() {
    let dir = tempdir().expect("tempdir");
    let rows = origin_trace::query::run(&QueryArgs {
        dir: dir.path().to_path_buf(),
        kind: None,
        error_kind: None,
        limit: 10,
    })
    .expect("query");
    assert!(rows.is_empty());
}
