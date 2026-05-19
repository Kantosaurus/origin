#![allow(clippy::single_char_pattern)] // test asserts use exact `"$"` from the P2.11 spec

use origin_cli::status::{render_line, UsageSnapshot};
use std::time::Duration;

#[test]
fn renders_in_out_cache_cost_time() {
    let snap = UsageSnapshot {
        provider: "anthropic",
        model: "claude-opus-4-7".to_string(),
        input_tokens: 1024,
        output_tokens: 512,
        cache_read_input_tokens: 256,
        cache_creation_input_tokens: 128,
        elapsed: Duration::from_millis(2_345),
    };
    let s = render_line(&snap);
    assert!(s.contains("in 1024"), "got {s}");
    assert!(s.contains("out 512"));
    assert!(s.contains("cache_r 256"));
    assert!(s.contains("cache_w 128"));
    assert!(s.contains("$"));
    assert!(s.contains("2.345s"));
}

#[test]
fn cost_is_zero_for_unknown_model() {
    let snap = UsageSnapshot {
        provider: "anthropic",
        model: "unknown-model-xyz".to_string(),
        input_tokens: 100,
        output_tokens: 100,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        elapsed: Duration::from_millis(10),
    };
    let s = render_line(&snap);
    assert!(s.contains("$0.000"));
}
