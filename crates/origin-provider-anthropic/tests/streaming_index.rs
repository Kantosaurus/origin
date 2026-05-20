use origin_provider_anthropic::streaming::parse_chunk_for_test;

#[test]
fn parses_index_on_content_block_start() {
    let line = br#"data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#;
    let evt = parse_chunk_for_test(line).expect("frame");
    assert_eq!(evt.index, Some(2));
}

#[test]
fn parses_index_on_tool_use_delta() {
    let line = br#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}"#;
    let evt = parse_chunk_for_test(line).expect("frame");
    assert_eq!(evt.index, Some(1));
}
