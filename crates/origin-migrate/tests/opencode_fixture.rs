// SPDX-License-Identifier: Apache-2.0
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::source::Source;
use std::path::PathBuf;

#[test]
fn opencode_scan_reads_one_session_two_messages() {
    let root = PathBuf::from("tests/fixtures/opencode");
    let src = OpencodeSource;
    let bundle = src.scan(&root).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].title.as_deref(), Some("First session"));
    assert_eq!(bundle.sessions[0].messages.len(), 2);
    assert_eq!(bundle.sessions[0].messages[0].body, "ping");
    assert_eq!(bundle.sessions[0].messages[1].body, "pong");
}
