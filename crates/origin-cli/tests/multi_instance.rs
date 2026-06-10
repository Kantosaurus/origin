// SPDX-License-Identifier: Apache-2.0
//! Multi-instance isolation: `origin` launched from two different project
//! directories must talk to two different daemons (per-project IPC paths
//! derived from the cwd), so n concurrent sessions never share or kill each
//! other's daemon. Regression guard for the "running `origin` in a second
//! VS Code window kills the first window's daemon" defect.
//!
//! Strategy: stand up two fake daemons bound to the *derived* per-instance
//! paths of two temp dirs (no `ORIGIN_SOCK` override), then run
//! `origin run --json` with the cwd set to each dir and assert each CLI
//! reached its own daemon (distinguished by reply text).

use origin_daemon::protocol::{PromptReply, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::instance::InstanceId;
use origin_ipc::transport::Listener;
use tempfile::TempDir;

/// One fake daemon turn: accept a connection, read the request, stream a
/// single text delta + turn end, then send the final reply.
async fn serve_one(listener: Listener, reply_text: &'static str) {
    let mut conn = listener.accept().await.expect("accept");
    let _req = conn.read_frame_body().await.expect("read req");
    let ev = StreamEvent::TextDelta {
        text: reply_text.into(),
    };
    let body = serde_json::to_vec(&ev).expect("ser ev");
    conn.write_raw(&encode(1, FrameKind::Event, &body))
        .await
        .expect("write ev");
    let body = serde_json::to_vec(&StreamEvent::TurnEnd).expect("ser end");
    conn.write_raw(&encode(1, FrameKind::Event, &body))
        .await
        .expect("write end");
    let reply = PromptReply {
        assistant_text: reply_text.into(),
        turns: 1,
    };
    let body = serde_json::to_vec(&reply).expect("ser reply");
    conn.write_raw(&encode(1, FrameKind::Response, &body))
        .await
        .expect("write reply");
}

#[tokio::test(flavor = "current_thread")]
async fn two_project_dirs_reach_two_distinct_daemons() {
    let dir_a = TempDir::new().expect("tempdir a");
    let dir_b = TempDir::new().expect("tempdir b");

    // The per-instance paths the CLI will derive from each cwd.
    let id_a = InstanceId::for_dir(dir_a.path());
    let id_b = InstanceId::for_dir(dir_b.path());
    assert_ne!(
        id_a.ipc_path(),
        id_b.ipc_path(),
        "distinct project dirs must derive distinct daemon paths"
    );

    // Two fake daemons listening simultaneously — the whole point: they
    // coexist, neither bind kicks the other off.
    let listener_a = Listener::bind(&id_a.ipc_path()).await.expect("bind a");
    let listener_b = Listener::bind(&id_b.ipc_path()).await.expect("bind b");
    let server_a = tokio::spawn(serve_one(listener_a, "from-daemon-A"));
    let server_b = tokio::spawn(serve_one(listener_b, "from-daemon-B"));

    let cmd = env!("CARGO_BIN_EXE_origin");
    // No ORIGIN_SOCK: the CLI must derive the path from its cwd.
    let out_a = tokio::process::Command::new(cmd)
        .current_dir(dir_a.path())
        .env_remove("ORIGIN_SOCK")
        .args(["run", "--json", "hello"])
        .output()
        .await
        .expect("run in dir a");
    let out_b = tokio::process::Command::new(cmd)
        .current_dir(dir_b.path())
        .env_remove("ORIGIN_SOCK")
        .args(["run", "--json", "hello"])
        .output()
        .await
        .expect("run in dir b");
    server_a.await.expect("server a");
    server_b.await.expect("server b");

    let stdout_a = String::from_utf8_lossy(&out_a.stdout);
    let stdout_b = String::from_utf8_lossy(&out_b.stdout);
    assert!(
        out_a.status.success(),
        "run in dir A failed; stderr: {}",
        String::from_utf8_lossy(&out_a.stderr)
    );
    assert!(
        out_b.status.success(),
        "run in dir B failed; stderr: {}",
        String::from_utf8_lossy(&out_b.stderr)
    );
    // Each CLI reached ITS OWN daemon — not a shared/global one.
    assert!(
        stdout_a.contains("from-daemon-A"),
        "dir-A run must hit daemon A, got: {stdout_a}"
    );
    assert!(
        stdout_b.contains("from-daemon-B"),
        "dir-B run must hit daemon B, got: {stdout_b}"
    );
}

/// `ORIGIN_SOCK` still overrides the per-instance derivation — the escape
/// hatch for a deliberately shared daemon (and for every existing harness
/// test that pins a temp socket).
#[tokio::test(flavor = "current_thread")]
async fn origin_sock_override_bypasses_instance_path() {
    let dir = TempDir::new().expect("tempdir");
    let sock = if cfg!(windows) {
        format!(r"\\.\pipe\origin-override-{}", ulid::Ulid::new())
    } else {
        format!("{}/origin-override.sock", dir.path().display())
    };
    let listener = Listener::bind(&sock).await.expect("bind");
    let server = tokio::spawn(serve_one(listener, "from-shared-daemon"));

    let cmd = env!("CARGO_BIN_EXE_origin");
    let out = tokio::process::Command::new(cmd)
        .current_dir(dir.path())
        .env("ORIGIN_SOCK", &sock)
        .args(["run", "--json", "hello"])
        .output()
        .await
        .expect("run with override");
    server.await.expect("server");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "override run failed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("from-shared-daemon"),
        "ORIGIN_SOCK override must win over the derived path, got: {stdout}"
    );
}
