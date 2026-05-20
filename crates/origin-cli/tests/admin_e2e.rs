//! P13.4.4 — end-to-end smoke for the `origin sessions ls` admin path
//! against a stand-in daemon. The fake daemon binds the same
//! local-socket transport the real daemon uses, accepts the
//! `ClientMessage::ListSessions` frame, and replies with a fixed
//! `StreamEvent::SessionsListed` event. The CLI binary is exec'd with
//! `ORIGIN_SOCK` pointed at the stand-in so the full encode → decode →
//! render path is covered.

use origin_daemon::protocol::{ClientMessage, SessionSummaryWire, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Listener;

#[tokio::test(flavor = "current_thread")]
async fn sessions_ls_prints_summaries() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let sock = if cfg!(windows) {
        format!(r"\\.\pipe\origin-admin-{}", ulid::Ulid::new())
    } else {
        format!("{}/admin.sock", dir.path().display())
    };
    let listener = Listener::bind(&sock).await.expect("bind");
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let req = conn.read_frame_body().await.expect("read req");
        let cm: ClientMessage = serde_json::from_slice(&req).expect("de");
        assert!(matches!(cm, ClientMessage::ListSessions));
        let ev = StreamEvent::SessionsListed {
            summaries: vec![
                SessionSummaryWire {
                    id: "s1".into(),
                    created_at: 1,
                    title: Some("alpha".into()),
                    model: "m1".into(),
                    message_count: 4,
                },
                SessionSummaryWire {
                    id: "s2".into(),
                    created_at: 2,
                    title: None,
                    model: "m2".into(),
                    message_count: 9,
                },
            ],
        };
        let body = serde_json::to_vec(&ev).expect("ser");
        conn.write_raw(&encode(1, FrameKind::Event, &body))
            .await
            .expect("write ev");
    });

    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_origin"))
        .env("ORIGIN_SOCK", &sock)
        .args(["sessions", "ls"])
        .output()
        .await
        .expect("run");
    server.await.expect("server");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("s1"), "stdout: {stdout}");
    assert!(stdout.contains("s2"), "stdout: {stdout}");
    assert!(stdout.contains("alpha"), "stdout: {stdout}");
}
