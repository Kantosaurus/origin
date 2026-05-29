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
