use origin_migrate::jcode::JcodeSource;
use origin_migrate::source::Source;
use rusqlite::Connection;
use tempfile::tempdir;

fn seed_db(path: &std::path::Path) {
    let c = Connection::open(path).expect("open");
    c.execute_batch(
        "
        CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, created_at INTEGER);
        CREATE TABLE messages (id INTEGER PRIMARY KEY, session_id TEXT, role TEXT, body TEXT, ts INTEGER);
        INSERT INTO sessions (id,title,created_at) VALUES ('s1','first',1700000000000);
        INSERT INTO messages (session_id,role,body,ts) VALUES
          ('s1','user','hi',1700000000001),
          ('s1','assistant','hello',1700000000002);
        ",
    )
    .expect("seed");
}

#[test]
fn jcode_scan_reads_one_session_two_messages() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("sessions.sqlite");
    seed_db(&db);

    let src = JcodeSource;
    let bundle = src.scan(dir.path()).expect("scan ok");

    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].title.as_deref(), Some("first"));
    assert_eq!(bundle.sessions[0].messages.len(), 2);
    assert_eq!(bundle.sessions[0].messages[0].role, "user");
    assert_eq!(bundle.sessions[0].messages[0].body, "hi");
}
