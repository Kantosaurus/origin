// SPDX-License-Identifier: Apache-2.0
//! Regression: a daemon that was killed/panicked leaves its Unix socket file
//! on disk. `interprocess`'s Drop-time unlink never ran, so the path exists
//! but no listener owns it. A fresh `Listener::bind` to the same path must
//! reclaim the stale file instead of failing with `AddrInUse`.
// These transport types and the path helper are exercised only by the
// `#[cfg(unix)]` regression tests below (the stale-socket-file failure mode is
// specific to Unix-domain sockets; Windows named pipes have no on-disk inode to
// leave behind). Gating them to `unix` keeps `clippy --all-targets -D warnings`
// clean on Windows, where the tests themselves are compiled out.
#[cfg(unix)]
use origin_ipc::transport::{Connector, Listener};

#[cfg(unix)]
fn unique_socket_path() -> String {
    let id = ulid::Ulid::new();
    format!("{}/origin-stale-{id}.sock", std::env::temp_dir().display())
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn rebind_reclaims_stale_socket_file() {
    let path = unique_socket_path();

    // Simulate a crashed daemon: std's `UnixListener` does NOT unlink its
    // socket file on drop, so closing the fd leaves a stale socket inode on
    // disk with no listener behind it — exactly the post-crash state.
    {
        let stale = std::os::unix::net::UnixListener::bind(&path).expect("seed stale socket");
        drop(stale);
    }
    assert!(std::path::Path::new(&path).exists(), "stale file should remain");

    // A fresh bind to the same path must succeed by reclaiming the stale file.
    let _relisten = Listener::bind(&path).await.expect("rebind over stale socket");

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn rebind_refuses_when_live_listener_owns_path() {
    let path = unique_socket_path();

    // A *live* listener owns the path: a rebind must NOT clobber it.
    let live = Listener::bind(&path).await.expect("live bind");
    // Keep accepting so the socket is genuinely connectable.
    let accept_path = path.clone();
    let server = tokio::spawn(async move {
        let _ = live.accept().await;
        drop(accept_path);
    });

    // Confirm the path is live before asserting bind refuses it.
    let _client = Connector::connect(&path).await.expect("connect to live");

    let rebind = Listener::bind(&path).await;
    assert!(rebind.is_err(), "rebind must refuse a live socket");
    assert_eq!(
        rebind.err().map(|e| e.kind()),
        Some(std::io::ErrorKind::AddrInUse),
        "live socket should still report AddrInUse",
    );

    server.abort();
    let _ = std::fs::remove_file(&path);
}
