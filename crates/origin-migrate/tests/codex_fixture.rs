// SPDX-License-Identifier: Apache-2.0
use origin_migrate::codex::CodexSource;
use origin_migrate::source::Source;
use std::path::PathBuf;

#[test]
fn codex_scan_reads_one_session_with_flattened_segments() {
    let root = PathBuf::from("tests/fixtures/codex");
    let src = CodexSource;
    assert_eq!(src.name(), "codex");

    let bundle = src.scan(&root).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    let session = &bundle.sessions[0];

    // The header line carries the timestamp; the 3 message/tool records become
    // ImportedMessages (the header itself is not a message).
    assert_eq!(session.created_at_unix_ms, 1_700_000_000_000);
    assert_eq!(session.messages.len(), 3);

    assert_eq!(session.messages[0].role, "user");
    assert_eq!(session.messages[0].body, "refactor the parser");

    // The assistant content is an array of text segments flattened into one body.
    assert_eq!(session.messages[1].role, "assistant");
    assert_eq!(session.messages[1].body, "splitting the lexer out");

    // function_call_output maps onto the "tool" role.
    assert_eq!(session.messages[2].role, "tool");
    assert!(session.messages[2].body.contains("src/lexer.rs"));

    // source_id is the path relative to root.
    assert!(
        session.source_id.contains("rollout-test.jsonl"),
        "source_id = {}",
        session.source_id
    );
}
