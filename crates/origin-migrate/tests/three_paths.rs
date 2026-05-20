//! GA criterion 5: three migration paths (claude-code / jcode / opencode)
//! each yield a non-empty `MigrateBundle` on the bundled fixtures.

use origin_migrate::claude_code::ClaudeCodeSource;
use origin_migrate::jcode::JcodeSource;
use origin_migrate::opencode::OpencodeSource;
use origin_migrate::source::Source;
use rusqlite::Connection;
use tempfile::tempdir;

#[test]
fn three_sources_each_produce_a_session() {
    // claude-code from on-disk fixture
    let cc = ClaudeCodeSource
        .scan(std::path::Path::new("tests/fixtures/claude-code"))
        .expect("cc scan");
    assert!(!cc.sessions.is_empty(), "claude-code scan empty");

    // jcode seeded at runtime
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("sessions.sqlite");
    let c = Connection::open(&db).expect("open sqlite");
    c.execute_batch(
        "CREATE TABLE sessions(id TEXT PRIMARY KEY,title TEXT,created_at INTEGER);
         CREATE TABLE messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,body TEXT,ts INTEGER);
         INSERT INTO sessions VALUES('a','t',1);
         INSERT INTO messages(session_id,role,body,ts) VALUES('a','user','x',2);",
    )
    .expect("seed");
    drop(c); // release the file handle before JcodeSource opens it
    let jc = JcodeSource.scan(dir.path()).expect("jc scan");
    assert!(!jc.sessions.is_empty(), "jcode scan empty");

    // opencode from on-disk fixture
    let oc = OpencodeSource
        .scan(std::path::Path::new("tests/fixtures/opencode"))
        .expect("oc scan");
    assert!(!oc.sessions.is_empty(), "opencode scan empty");
}
