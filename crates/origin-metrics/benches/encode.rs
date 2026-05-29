// SPDX-License-Identifier: Apache-2.0
//! `cargo bench -p origin-metrics --bench encode -- --quick`
//!
//! Threshold: encode 1000-series snapshot in <= 200 us.

use std::time::Instant;

use origin_metrics::Metrics;

fn main() {
    let m = Metrics::new();
    // Generate ~1000 distinct series by combining the 18 allowlisted tools
    // with the 7 allowlisted providers and 3 result tiers (= 378), then
    // inflating with a few token series so the registry sees ~1000 rows.
    for tool in origin_metrics::keys::ALLOWED_TOOLS {
        for provider in origin_metrics::keys::ALLOWED_PROVIDERS {
            for result in origin_metrics::keys::ALLOWED_RESULTS {
                m.tool_call_total(provider, tool, result).inc();
            }
        }
    }
    // Pad with token series; model labels go through the unbounded path
    // so we exercise the raw label set.
    for i in 0..600 {
        let model = Box::leak(format!("model-{i}").into_boxed_str()) as &'static str;
        m.tokens_in_total("anthropic", model).inc_by(1);
    }

    // Warm up.
    for _ in 0..10 {
        let _ = m.encode_text().expect("encode");
    }

    let iters = 1000_u32;
    let start = Instant::now();
    for _ in 0..iters {
        let _ = m.encode_text().expect("encode");
    }
    let elapsed = start.elapsed();
    let per_call = elapsed / iters;
    eprintln!("encode {per_call:?} avg ({iters} iters, total {elapsed:?})");
    assert!(
        per_call.as_micros() <= 200,
        "encode took {per_call:?} > 200us threshold"
    );
}
