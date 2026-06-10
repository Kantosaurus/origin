// SPDX-License-Identifier: Apache-2.0
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::quic::{QuicConnector, QuicListener};
use origin_ipc::tls::{generate_self_signed, sha256_fingerprint};

#[tokio::test(flavor = "current_thread")]
async fn quic_round_trips_one_frame() {
    let server_bundle = generate_self_signed("origin-test").expect("generate server");
    let client_bundle = generate_self_signed("origin-client").expect("generate client");
    let server_fp = sha256_fingerprint(&server_bundle.cert_der);
    let client_fp = sha256_fingerprint(&client_bundle.cert_der);

    let listener = QuicListener::bind(
        "127.0.0.1:0".parse().expect("static literal"),
        server_bundle,
        vec![client_fp],
    )
    .await
    .expect("bind");
    let addr = listener.local_addr();

    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let (_kind, body) = conn.read_frame().await.expect("read");
        conn.write_frame(FrameKind::Response, &body).await.expect("write");
    });

    let mut client = QuicConnector::connect(addr, "origin-test", server_fp, &client_bundle)
        .await
        .expect("connect");
    client
        .write_raw(&encode(7, FrameKind::Request, b"ping"))
        .await
        .expect("write_raw");
    let (kind, body) = client.read_frame().await.expect("read");
    assert_eq!(kind, FrameKind::Response);
    assert_eq!(&body, b"ping");
    server.await.expect("server task");
}

/// Zero-trust: a client whose certificate fingerprint is not on the server's
/// pinned allow-list must be rejected at the TLS handshake, never reaching the
/// frame layer.
#[tokio::test(flavor = "current_thread")]
async fn quic_rejects_unpinned_client() {
    let server_bundle = generate_self_signed("origin-test").expect("generate server");
    let trusted_client = generate_self_signed("origin-client").expect("generate trusted");
    let attacker = generate_self_signed("attacker").expect("generate attacker");
    let server_fp = sha256_fingerprint(&server_bundle.cert_der);
    let trusted_fp = sha256_fingerprint(&trusted_client.cert_der);

    // Server pins ONLY the trusted client.
    let listener = QuicListener::bind(
        "127.0.0.1:0".parse().expect("static literal"),
        server_bundle,
        vec![trusted_fp],
    )
    .await
    .expect("bind");
    let addr = listener.local_addr();
    let server = tokio::spawn(async move {
        // The attacker's handshake must fail, so accept() never yields a usable
        // connection. A trusted client would be echoed here, but the attacker
        // never gets that far.
        if let Ok(mut conn) = listener.accept().await {
            if let Ok((_k, body)) = conn.read_frame().await {
                let _ = conn.write_frame(FrameKind::Response, &body).await;
            }
        }
    });

    // The attacker presents a valid-but-unpinned client cert and pins the real
    // server. Mutual TLS may let the client-side handshake complete optimistically,
    // but the server rejects the client cert, so no request/response round-trip
    // can complete: either connect fails or the first frame exchange errors.
    let outcome = async {
        let mut client = QuicConnector::connect(addr, "origin-test", server_fp, &attacker).await?;
        client
            .write_raw(&encode(1, FrameKind::Request, b"intrude"))
            .await?;
        let _ = client.read_frame().await?;
        Ok::<(), origin_ipc::quic::QuicError>(())
    }
    .await;
    assert!(
        outcome.is_err(),
        "server must reject a client whose fingerprint is not pinned"
    );
    server.abort();
}

/// Zero-trust: a client that dials with the wrong pinned server fingerprint
/// (e.g. an attacker-substituted daemon) must refuse to connect.
#[tokio::test(flavor = "current_thread")]
async fn quic_client_rejects_wrong_server_fingerprint() {
    let server_bundle = generate_self_signed("origin-test").expect("generate server");
    let client_bundle = generate_self_signed("origin-client").expect("generate client");
    let client_fp = sha256_fingerprint(&client_bundle.cert_der);
    // A fingerprint that does NOT belong to the real server.
    let wrong_fp = sha256_fingerprint(b"not the server certificate");

    let listener = QuicListener::bind(
        "127.0.0.1:0".parse().expect("static literal"),
        server_bundle,
        vec![client_fp],
    )
    .await
    .expect("bind");
    let addr = listener.local_addr();
    let server = tokio::spawn(async move {
        let _ = listener.accept().await;
    });

    let result = QuicConnector::connect(addr, "origin-test", wrong_fp, &client_bundle).await;
    assert!(
        result.is_err(),
        "client must reject a server whose certificate does not match the pin"
    );
    server.abort();
}
