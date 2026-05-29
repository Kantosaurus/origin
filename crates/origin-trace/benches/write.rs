// SPDX-License-Identifier: Apache-2.0
//! `cargo bench -p origin-trace --bench write -- --quick`
//!
//! Threshold: > 100k spans / sec on a single thread, single core.

use std::time::Instant;

use origin_trace::{Ring, SpanRow};
use tempfile::tempdir;

fn main() {
    let dir = tempdir().expect("tempdir");
    let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open");
    let n: u64 = 200_000;
    let start = Instant::now();
    for i in 0..n {
        ring.append(SpanRow {
            ts_ns: i,
            span_id: i,
            parent_id: 0,
            kind: "tool",
            provider: "anthropic",
            tool: "Bash",
            dur_us: 1,
            error_kind: "",
            attrs_json: r#"{"k":"v"}"#.into(),
        })
        .expect("append");
    }
    ring.flush().expect("flush");
    let elapsed = start.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let rate = (n as f64) / elapsed.as_secs_f64();
    eprintln!("write rate: {rate:.0} rows/s in {elapsed:?}");
    assert!(rate > 100_000.0, "write rate {rate} below 100k rows/s threshold");
}
