// SPDX-License-Identifier: Apache-2.0
//! Remote QUIC transport wiring (R7).
//!
//! The daemon ships a full QUIC + mutual-TLS remote transport in
//! [`origin_ipc::quic`], but historically nothing in production ever stood up
//! the *server* side, so `origin run --remote …` / `origin pair redeem` dialed a
//! port nothing was listening on. This module binds that listener — but **only**
//! when explicitly configured via `ORIGIN_QUIC_BIND` (default unset ⇒ no remote
//! listener, no new network surface, byte-identical boot) — and enforces the
//! bearer tokens the pairing flow already mints.
//!
//! ## Trust model
//! - **Server auth + encryption**: the client pins the daemon's self-signed cert
//!   fingerprint out-of-band (the `#<fp>` fragment of the `origin://…` URL the
//!   daemon prints at bind time). The QUIC/TLS 1.3 handshake encrypts the
//!   channel and proves the daemon's identity.
//! - **Client authorization (deny-by-default)**: a paired client presents its
//!   bearer as the first frame on the stream. The accept loop reads it and calls
//!   [`BearerStore::authorized`] *before* serving any `ClientMessage`. A
//!   missing/invalid/revoked token closes the connection.
//!
//! ## Dispatch
//! Rather than duplicate the ~700-line per-connection dispatcher, an authorized
//! QUIC connection is **bridged** to the already-bound local socket: the daemon
//! opens a fresh local client connection to `ORIGIN_SOCK` and pumps frames in
//! both directions. This reuses the entire existing request handler unchanged,
//! so a remote turn runs through the exact same agent path as a local one.

use std::net::SocketAddr;
use std::sync::Arc;

use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::quic::{QuicConnection, QuicListener};
use origin_ipc::tls::{generate_self_signed, sha256_fingerprint_hex, CertBundle};
use origin_ipc::transport::Connector;
use origin_runtime::{spawn_in, TaskClass};
use tracing::{error, info, warn};

use crate::auth::BearerStore;

/// Bind a bearer-gated remote QUIC listener and spawn its accept loop.
///
/// Bound ONLY when `ORIGIN_QUIC_BIND` is set to a parseable socket address. When
/// the var is unset (the default) this is a complete no-op: no socket is opened
/// and the daemon's network surface is byte-identical to before this wiring
/// landed.
///
/// `bearer_store` is the SAME store the pairing `PairRedeem` path populates, so a
/// freshly paired device authorizes immediately. `sock_path` is the local IPC
/// socket the daemon already bound; authorized remote connections are bridged to
/// it so they reuse the existing dispatcher.
pub fn maybe_spawn(bearer_store: Arc<BearerStore>, sock_path: String) {
    let Some(bind_str) = std::env::var("ORIGIN_QUIC_BIND").ok().filter(|s| !s.is_empty()) else {
        // Default: no remote listener. No new network surface.
        return;
    };
    let addr: SocketAddr = match bind_str.parse() {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, value = %bind_str, "ORIGIN_QUIC_BIND is not a valid socket address; remote listener NOT started");
            return;
        }
    };

    // Server identity: a self-signed cert minted at boot. The client pins its
    // SHA-256 fingerprint via the `origin://…#<fp>` URL, so we print the bind
    // URL the operator hands out.
    let bundle = match resolve_server_bundle() {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "remote QUIC: could not build server certificate; remote listener NOT started");
            return;
        }
    };
    let fp_hex = sha256_fingerprint_hex(&bundle.cert_der);

    spawn_in(TaskClass::Realtime, async move {
        let listener = match QuicListener::bind_bearer_gated(addr, bundle).await {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, %addr, "remote QUIC: bind failed; remote access unavailable");
                return;
            }
        };
        let local_addr = listener.local_addr();
        info!(
            url = %format!("origin://{local_addr}#{fp_hex}"),
            "remote QUIC listener bound (bearer-gated); pair a device and connect with --remote/--bearer"
        );
        accept_loop(listener, bearer_store, sock_path).await;
    });
}

/// Resolve the server certificate bundle presented to remote clients. Today this
/// mints a fresh self-signed Ed25519 identity per boot (the fingerprint changes
/// across restarts, so a client re-pairs / re-reads the printed URL). Persisting
/// the identity across restarts is a follow-up.
fn resolve_server_bundle() -> anyhow::Result<CertBundle> {
    generate_self_signed("origin-daemon").map_err(|e| anyhow::anyhow!("server identity: {e}"))
}

/// Accept loop for the bearer-gated remote listener. Each accepted connection is
/// authorized against `bearer_store` before being bridged to the local socket.
async fn accept_loop(listener: QuicListener, bearer_store: Arc<BearerStore>, sock_path: String) {
    loop {
        let conn = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                // A single failed handshake (e.g. an attacker probing the port)
                // must not tear down the listener; log and keep accepting.
                warn!(error = %e, "remote QUIC: accept failed; continuing");
                continue;
            }
        };
        let store = Arc::clone(&bearer_store);
        let sock = sock_path.clone();
        spawn_in(TaskClass::Critical, async move {
            serve_one(conn, store, sock).await;
        });
    }
}

/// Authorize one remote connection and, if authorized, bridge it to the local
/// socket. The bearer gate is the enforcement point: a missing/invalid/revoked
/// token drops the connection WITHOUT serving any request (deny-by-default).
async fn serve_one(mut conn: QuicConnection, bearer_store: Arc<BearerStore>, sock_path: String) {
    let bearer = match conn.read_bearer().await {
        Ok(b) => b,
        Err(e) => {
            // No bearer frame ⇒ unauthenticated peer. Deny.
            warn!(error = %e, "remote QUIC: connection presented no bearer; rejected");
            return;
        }
    };
    if !bearer_store.authorized(&bearer) {
        // Deny-by-default: unknown or revoked token. Drop without serving.
        warn!("remote QUIC: invalid/expired bearer; connection rejected");
        return;
    }
    info!("remote QUIC: bearer authorized; bridging connection to local dispatcher");

    // Bridge to the local socket so the authorized remote connection reuses the
    // existing per-connection dispatcher unchanged.
    let local = match Connector::connect(&sock_path).await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "remote QUIC: could not open local bridge connection");
            return;
        }
    };
    if let Err(e) = bridge_frames(conn, local).await {
        // A closed connection is the normal end-of-session case; log at debug
        // level via a warn-with-context only on unexpected errors.
        tracing::debug!(error = %e, "remote QUIC: bridge ended");
    }
}

/// Pump frames between the remote QUIC connection and a local socket connection
/// until either side closes. Frames are decoded and re-encoded (`request_id`
/// normalized to 0, which both the dispatcher and the CLI ignore), preserving
/// frame kind + body across the bridge.
///
/// Each transport is a single owner of its (bidirectional) stream, so we cannot
/// split read/write halves. Instead each connection lives in its own task and a
/// pair of channels carries frames between them: the QUIC task forwards
/// client→daemon frames and writes daemon→client frames, and vice-versa for the
/// local task. When either transport closes, its task drops its sender, which
/// unblocks and tears down the other.
async fn bridge_frames(
    mut quic: QuicConnection,
    mut local: origin_ipc::transport::Connection,
) -> anyhow::Result<()> {
    // client→daemon and daemon→client frame channels.
    let (to_daemon_tx, mut to_daemon_rx) = tokio::sync::mpsc::channel::<(FrameKind, Vec<u8>)>(64);
    let (to_client_tx, mut to_client_rx) = tokio::sync::mpsc::channel::<(FrameKind, Vec<u8>)>(64);

    // Local half: pump frames to/from the daemon dispatcher.
    let local_task = spawn_in(TaskClass::Critical, async move {
        loop {
            tokio::select! {
                msg = to_daemon_rx.recv() => {
                    let Some((kind, body)) = msg else { break };
                    if local.write_raw(&encode(0, kind, &body)).await.is_err() {
                        break;
                    }
                }
                read = local.read_frame() => {
                    match read {
                        Ok((kind, body)) => {
                            if to_client_tx.send((kind, body)).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });

    // QUIC half: pump frames to/from the remote client.
    loop {
        tokio::select! {
            msg = to_client_rx.recv() => {
                let Some((kind, body)) = msg else { break };
                if quic.write_frame(kind, &body).await.is_err() {
                    break;
                }
            }
            read = quic.read_frame() => {
                match read {
                    Ok((kind, body)) => {
                        if to_daemon_tx.send((kind, body)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
    drop(to_daemon_tx);
    local_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// With `ORIGIN_QUIC_BIND` unset, `maybe_spawn` must be a complete no-op:
    /// no listener task, no panic. (Default-safe / no new network surface.)
    #[test]
    fn maybe_spawn_is_noop_without_env() {
        // SAFETY: single-threaded test; we restore the prior value.
        let prev = std::env::var("ORIGIN_QUIC_BIND").ok();
        std::env::remove_var("ORIGIN_QUIC_BIND");
        let store = Arc::new(BearerStore::new());
        // Must not panic and must return immediately (no runtime needed because
        // the no-env path never calls `spawn_in`).
        maybe_spawn(store, "ignored-sock".to_string());
        if let Some(v) = prev {
            std::env::set_var("ORIGIN_QUIC_BIND", v);
        }
    }
}
