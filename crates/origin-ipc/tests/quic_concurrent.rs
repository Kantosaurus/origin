// SPDX-License-Identifier: Apache-2.0
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::quic::{QuicConnector, QuicListener};
use origin_ipc::tls::generate_self_signed;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_round_trip_concurrently() {
    let bundle = generate_self_signed("origin-test").expect("generate");
    let listener = QuicListener::bind("127.0.0.1:0".parse().expect("static literal"), bundle.clone())
        .await
        .expect("bind");
    let addr = listener.local_addr();
    let server_ca = bundle.ca_der.clone();

    // Server: accept two clients, echo whatever request body they send.
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let mut conn = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                let (_kind, body) = conn.read_frame().await.expect("read");
                conn.write_frame(FrameKind::Response, &body).await.expect("write");
            });
        }
        // Keep the listener (and its endpoint) alive long enough for the
        // per-client tasks above to finish flushing their responses.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let ca_alpha = server_ca.clone();
    let ca_beta = server_ca.clone();
    let alpha = tokio::spawn(async move {
        let mut client = QuicConnector::connect(addr, "origin-test", &ca_alpha)
            .await
            .expect("connect alpha");
        client
            .write_raw(&encode(1, FrameKind::Request, b"alpha"))
            .await
            .expect("write alpha");
        let (kind, body) = client.read_frame().await.expect("read alpha");
        assert_eq!(kind, FrameKind::Response);
        assert_eq!(&body, b"alpha");
    });
    let beta = tokio::spawn(async move {
        let mut client = QuicConnector::connect(addr, "origin-test", &ca_beta)
            .await
            .expect("connect beta");
        client
            .write_raw(&encode(2, FrameKind::Request, b"beta"))
            .await
            .expect("write beta");
        let (kind, body) = client.read_frame().await.expect("read beta");
        assert_eq!(kind, FrameKind::Response);
        assert_eq!(&body, b"beta");
    });

    let (a, b) = tokio::join!(alpha, beta);
    a.expect("alpha task");
    b.expect("beta task");
    server.await.expect("server task");
}
