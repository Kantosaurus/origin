use origin_provider_openai::streaming::parse_chunk_for_test;

#[test]
fn openai_tool_call_index_preserved() {
    let line = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_a","function":{"name":"r","arguments":"{}"}}]}}]}"#;
    let evt = parse_chunk_for_test(line).expect("frame");
    assert_eq!(evt.index, Some(1));
}

#[test]
fn openai_usage_when_include_usage_set() {
    let line = br#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#;
    let evt = parse_chunk_for_test(line).expect("usage frame");
    assert_eq!(evt.usage.expect("usage").input_tokens, 7);
}
