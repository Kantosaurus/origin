// SPDX-License-Identifier: Apache-2.0
use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::source::Source;
use std::path::PathBuf;

#[test]
fn claude_code_scan_reads_one_session_and_one_skill() {
    let root = PathBuf::from("tests/fixtures/claude-code");
    let src = ClaudeCodeSource;
    let bundle = src.scan(&root).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].messages.len(), 4);
    assert_eq!(bundle.sessions[0].messages[0].role, "user");
    assert_eq!(bundle.sessions[0].messages[0].body, "hello");

    assert_eq!(bundle.skills.len(), 1);
    assert_eq!(bundle.skills[0].name, "refactor");
    assert!(bundle.skills[0].body.contains("Refactor skill"));
}
