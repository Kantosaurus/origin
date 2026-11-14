// SPDX-License-Identifier: Apache-2.0
use origin_provider_gemini::streaming::parse_chunk_for_test;

#[test]
fn gemini_usage_metadata_on_final_frame() {
    let line = br#"data: {"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":4}}"#;
    let evt = parse_chunk_for_test(line).expect("final frame");
    let u = evt.usage.expect("usage");
    assert_eq!(u.input_tokens, 9);
    assert_eq!(u.output_tokens, 4);
}

/// `promptTokenCount` includes `cachedContentTokenCount`; `Usage.input_tokens`
/// is the uncached remainder.
#[test]
fn gemini_streamed_usage_splits_out_cached_tokens() {
    let line = br#"data: {"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1000,"candidatesTokenCount":4,"cachedContentTokenCount":600}}"#;
    let u = parse_chunk_for_test(line).and_then(|f| f.usage).expect("usage");
    assert_eq!(u.input_tokens, 400);
    assert_eq!(u.cache_read_input_tokens, 600);
}
