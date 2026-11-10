// SPDX-License-Identifier: Apache-2.0
//! C10: `LspPool` keeps one server alive across diagnostics calls, never
//! reports a late publish for an earlier edit as fresh, and replaces servers
//! that hung or exited. Driven by the `fake-lsp-server` fixture (see its docs
//! for the text-triggered fault modes); the diagnostic `code` is the server pid.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use origin_lsp_client::{LspPool, Probe};

const FAKE: &str = env!("CARGO_BIN_EXE_fake-lsp-server");

async fn probe(pool: &LspPool, text: &str, timeout_ms: u64) -> Probe {
    probe_with(pool, &[], text, timeout_ms).await.unwrap()
}

/// Probe `a.rs` (contents `text`) with the fake launched with `args`.
async fn probe_with(
    pool: &LspPool,
    args: &[&str],
    text: &str,
    timeout_ms: u64,
) -> Result<Probe, origin_lsp_client::LspError> {
    pool.diagnose(
        FAKE,
        args,
        Path::new("."),
        Path::new("a.rs"),
        "rust",
        text,
        Duration::from_millis(timeout_ms),
    )
    .await
}

/// `(message, server pid)` of a fresh probe's single diagnostic.
fn fresh(p: Probe) -> (String, String) {
    let Probe::Fresh(d) = p else {
        unreachable!("expected fresh diagnostics, got {p:?}")
    };
    (d[0].message.clone(), d[0].code.clone().unwrap())
}

#[tokio::test]
async fn reuses_one_server_and_reports_each_edit() {
    let pool = LspPool::default();
    let (m1, pid1) = fresh(probe(&pool, "v1", 5000).await);
    let (m2, pid2) = fresh(probe(&pool, "v2", 5000).await);
    assert_eq!((m1.as_str(), m2.as_str()), ("v1", "v2"));
    assert_eq!(pid1, pid2, "second call spawned a new server");
}

#[tokio::test]
async fn slow_server_is_kept_and_its_late_publish_is_not_reported_as_fresh() {
    let pool = LspPool::default();
    let (_, pid) = fresh(probe(&pool, "v1", 5000).await);

    // Misses the deadline: the earlier result comes back as `previous`.
    let Probe::TimedOut { previous, .. } = probe(&pool, "SLOW:800", 300).await else {
        unreachable!("slow publish should time out")
    };
    assert_eq!(previous[0].message, "v1");

    // The late "SLOW:800" publish lands while this call waits; it answers an
    // earlier version and must not be returned for this edit.
    let (msg, pid_after) = fresh(probe(&pool, "SLOW:1200", 5000).await);
    assert_eq!(msg, "SLOW:1200", "stale publish reported as fresh");
    assert_eq!(pid_after, pid, "a slow but live server must be kept");
}

/// The server hangs after its first answer; the next call (with `next_text`)
/// must time out without blocking, and the call after must get a new server.
async fn hung_server_is_replaced(next_text: &str) {
    let pool = LspPool::default();
    let (_, pid) = fresh(probe(&pool, "HANG", 5000).await);
    let missed = tokio::time::timeout(Duration::from_secs(10), probe(&pool, next_text, 300))
        .await
        .unwrap();
    assert!(matches!(missed, Probe::TimedOut { .. }), "{missed:?}");
    let (msg, pid_after) = fresh(probe(&pool, "v3", 5000).await);
    assert_eq!(msg, "v3");
    assert_ne!(pid_after, pid, "hung server was not replaced");
}

#[tokio::test]
async fn hung_server_is_dropped_and_respawned() {
    hung_server_is_replaced("v2").await;
}

#[tokio::test]
async fn hung_server_that_stops_reading_does_not_block_the_call() {
    // Larger than any pipe buffer: the didOpen write itself blocks.
    hung_server_is_replaced(&"x".repeat(4 << 20)).await;
}

#[tokio::test]
async fn exited_server_is_respawned() {
    let pool = LspPool::default();
    let (_, pid) = fresh(probe(&pool, "EXIT", 5000).await);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (msg, pid_after) = fresh(probe(&pool, "v2", 5000).await);
    assert_eq!(msg, "v2");
    assert_ne!(pid_after, pid);
}

#[tokio::test]
async fn each_probe_closes_the_document() {
    let pool = LspPool::default();
    fresh(probe(&pool, "v1", 5000).await);
    let (log, _) = fresh(probe(&pool, "LOG", 5000).await);
    assert!(
        log.contains("textDocument/didOpen textDocument/didClose textDocument/didOpen"),
        "{log}"
    );
}

#[tokio::test]
async fn changed_files_are_reported_to_live_servers() {
    let pool = LspPool::default();
    fresh(probe(&pool, "v1", 5000).await);
    let paths: Vec<PathBuf> = (0..11).map(|i| PathBuf::from(format!("f{i}.rs"))).collect();
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    pool.notify_changed(&refs).await;
    let (log, _) = fresh(probe(&pool, "LOG", 5000).await);
    let seen: Vec<&str> = log.split_whitespace().collect();
    assert_eq!(
        seen[0], "initialize:watch",
        "watched-files capability not declared: {log}"
    );
    for i in 0..11 {
        let want = format!("changed:file://f{i}.rs");
        assert!(seen.contains(&want.as_str()), "{want} missing: {log}");
    }
}

#[tokio::test]
async fn recycle_replaces_servers_and_notify_alone_keeps_them() {
    let pool = LspPool::default();
    let (_, pid1) = fresh(probe(&pool, "v1", 5000).await);
    pool.notify_changed(&[Path::new("a.rs")]).await;
    let (_, pid2) = fresh(probe(&pool, "v2", 5000).await);
    pool.recycle().await;
    let (_, pid3) = fresh(probe(&pool, "v3", 5000).await);
    assert_eq!(pid1, pid2, "an edit-only turn must reuse the server");
    assert_ne!(pid2, pid3, "recycle must start a fresh server");
}

#[tokio::test]
async fn slow_starting_server_is_kept_through_its_startup_grace() {
    let pool = LspPool::default();
    // Reads nothing for 4 s: misses both deadlines and both pings.
    let args = ["--init-delay=4000"];
    for _ in 0..2 {
        let p = probe_with(&pool, &args, "v", 300).await.unwrap();
        assert!(matches!(p, Probe::TimedOut { starting: true, .. }), "{p:?}");
    }
    let (log, _) = fresh(probe_with(&pool, &args, "LOG", 8000).await.unwrap());
    assert_eq!(
        log.matches("textDocument/didOpen").count(),
        3,
        "the starting server was replaced: {log}"
    );
}

#[tokio::test]
async fn server_that_keeps_failing_at_startup_is_not_respawned_forever() {
    let pool = LspPool::default();
    let args = ["--exit-at-start"];
    for _ in 0..3 {
        let p = probe_with(&pool, &args, "v", 300).await;
        assert!(!matches!(p, Ok(Probe::Fresh(_))), "{p:?}");
    }
    let err = probe_with(&pool, &args, "v", 300).await.unwrap_err();
    assert!(err.to_string().contains("not restarting"), "{err}");
}

#[tokio::test]
async fn untagged_publish_after_a_timeout_is_marked_maybe_stale() {
    let pool = LspPool::default();
    let args = ["--untagged"];
    let p1 = probe_with(&pool, &args, "v1", 5000).await.unwrap();
    assert!(matches!(p1, Probe::Fresh(_)), "{p1:?}");
    let p2 = probe_with(&pool, &args, "SLOW:800", 300).await.unwrap();
    assert!(matches!(p2, Probe::TimedOut { .. }), "{p2:?}");
    // Untagged, and the previous call for this file timed out: may be late.
    let p3 = probe_with(&pool, &args, "v3", 5000).await.unwrap();
    assert!(matches!(p3, Probe::MaybeStale(_)), "{p3:?}");
    let p4 = probe_with(&pool, &args, "v4", 5000).await.unwrap();
    assert!(matches!(p4, Probe::Fresh(_)), "{p4:?}");
}

fn alive(pid: &str) -> bool {
    #[cfg(windows)]
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    #[cfg(not(windows))]
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    if cfg!(windows) {
        text.split_whitespace().any(|w| w == pid)
    } else {
        // A killed but not yet reaped child shows as a zombie (`Z`).
        !text.trim().is_empty() && !text.trim().starts_with('Z')
    }
}

#[tokio::test]
async fn dropping_the_pool_kills_its_servers() {
    let pool = LspPool::default();
    let (_, pid) = fresh(probe(&pool, "v1", 5000).await);
    assert!(alive(&pid));
    drop(pool);
    for _ in 0..50 {
        if !alive(&pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!alive(&pid), "server {pid} outlived its pool");
}
