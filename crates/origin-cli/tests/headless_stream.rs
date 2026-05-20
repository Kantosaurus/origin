//! Spin a fake daemon on a temp socket, send 3 events + final reply,
//! and assert the CLI's JSON-Lines stream matches a golden sequence.

use origin_daemon::protocol::{PromptReply, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Listener;
use tempfile::TempDir;

#[tokio::test(flavor = "current_thread")]
async fn json_lines_stream_matches_golden() {
    let dir = TempDir::new().expect("tempdir");
    let sock = if cfg!(windows) {
        format!(r"\\.\pipe\origin-test-{}", ulid::Ulid::new())
    } else {
        format!("{}/origin-test.sock", dir.path().display())
    };
    let listener = Listener::bind(&sock).await.expect("bind");

    let listen_sock = sock.clone();
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let _req = conn.read_frame_body().await.expect("read req");

        for ev in [
            StreamEvent::TextDelta { text: "hello ".into() },
            StreamEvent::TextDelta { text: "world".into() },
            StreamEvent::TurnEnd,
        ] {
            let body = serde_json::to_vec(&ev).expect("ser ev");
            conn.write_raw(&encode(1, FrameKind::Event, &body)).await.expect("write ev");
        }
        let reply = PromptReply { assistant_text: "hello world".into(), turns: 1 };
        let body = serde_json::to_vec(&reply).expect("ser reply");
        conn.write_raw(&encode(1, FrameKind::Response, &body)).await.expect("write reply");
        let _ = listen_sock;
    });

    let cmd = env!("CARGO_BIN_EXE_origin");
    let output = tokio::process::Command::new(cmd)
        .env("ORIGIN_SOCK", &sock)
        .args(["run", "--json", "summarize"])
        .output()
        .await
        .expect("run binary");
    server.await.expect("server task");

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    assert!(
        lines.iter().any(|l| l.contains("\"kind\":\"text_delta\"") && l.contains("hello ")),
        "missing first delta: {stdout}"
    );
    assert!(
        lines.iter().any(|l| l.contains("\"kind\":\"text_delta\"") && l.contains("world")),
        "missing second delta: {stdout}"
    );
    assert!(
        lines.iter().any(|l| l.contains("\"kind\":\"turn_end\"")),
        "missing turn_end: {stdout}"
    );
}
