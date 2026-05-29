// SPDX-License-Identifier: Apache-2.0
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
            StreamEvent::TextDelta {
                text: "hello ".into(),
            },
            StreamEvent::TextDelta { text: "world".into() },
            StreamEvent::TurnEnd,
        ] {
            let body = serde_json::to_vec(&ev).expect("ser ev");
            conn.write_raw(&encode(1, FrameKind::Event, &body))
                .await
                .expect("write ev");
        }
        let reply = PromptReply {
            assistant_text: "hello world".into(),
            turns: 1,
        };
        let body = serde_json::to_vec(&reply).expect("ser reply");
        conn.write_raw(&encode(1, FrameKind::Response, &body))
            .await
            .expect("write reply");
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

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"kind\":\"text_delta\"") && l.contains("hello ")),
        "missing first delta: {stdout}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\"kind\":\"text_delta\"") && l.contains("world")),
        "missing second delta: {stdout}"
    );
    assert!(
        lines.iter().any(|l| l.contains("\"kind\":\"turn_end\"")),
        "missing turn_end: {stdout}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn error_frame_surfaces_to_stderr_and_nonzero_exit() {
    // Before this fix, `origin run` (no --json) silently dropped ErrorFrame
    // bodies and returned exit 0, leaving the operator with no signal that
    // their prompt failed. The --json path *did* render the body but still
    // exited 0. Both paths must now propagate the error.
    let dir = TempDir::new().expect("tempdir");
    let sock = if cfg!(windows) {
        format!(r"\\.\pipe\origin-err-{}", ulid::Ulid::new())
    } else {
        format!("{}/origin-err.sock", dir.path().display())
    };
    let listener = Listener::bind(&sock).await.expect("bind");

    let listen_sock = sock.clone();
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let _req = conn.read_frame_body().await.expect("read req");
        let msg = b"loop error: provider: rate limit; retry after 5s";
        conn.write_raw(&encode(1, FrameKind::ErrorFrame, msg))
            .await
            .expect("write err");
        let _ = listen_sock;
    });

    let cmd = env!("CARGO_BIN_EXE_origin");
    let output = tokio::process::Command::new(cmd)
        .env("ORIGIN_SOCK", &sock)
        .args(["run", "summarize"])
        .output()
        .await
        .expect("run binary");
    server.await.expect("server task");

    assert!(
        !output.status.success(),
        "expected non-zero exit on ErrorFrame, got {}; stdout={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("loop error") && stderr.contains("rate limit"),
        "stderr missing daemon message: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_url_routes_through_quic() {
    use origin_ipc::quic::QuicListener;
    use origin_ipc::tls::generate_self_signed;

    let bundle = generate_self_signed("origin-daemon").expect("cert");
    let listener = QuicListener::bind("127.0.0.1:0".parse().expect("addr"), bundle.clone())
        .await
        .expect("bind quic");
    let addr = listener.local_addr();
    let dir = TempDir::new().expect("tempdir");
    let ca_path = dir.path().join("ca.der");
    std::fs::write(&ca_path, &bundle.ca_der).expect("write ca");

    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let _req = conn.read_frame().await.expect("read req");
        let ev = origin_daemon::protocol::StreamEvent::TextDelta {
            text: "remote-ok".into(),
        };
        let body = serde_json::to_vec(&ev).expect("ser ev");
        conn.write_frame(origin_ipc::frame::FrameKind::Event, &body)
            .await
            .expect("write ev");
        let reply = origin_daemon::protocol::PromptReply {
            assistant_text: "remote-ok".into(),
            turns: 1,
        };
        let body = serde_json::to_vec(&reply).expect("ser reply");
        conn.write_frame(origin_ipc::frame::FrameKind::Response, &body)
            .await
            .expect("write reply");
    });

    let cmd = env!("CARGO_BIN_EXE_origin");
    let url = format!("origin://{addr}#deadbeef");
    let output = tokio::process::Command::new(cmd)
        .env("ORIGIN_REMOTE_CA_DER_FILE", &ca_path)
        .args(["run", "--remote", &url, "--json", "hi"])
        .output()
        .await
        .expect("run binary");
    server.await.expect("server task");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("remote-ok"), "stdout: {stdout}");
}
