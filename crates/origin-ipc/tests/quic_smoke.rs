// SPDX-License-Identifier: Apache-2.0
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::quic::{QuicConnector, QuicListener};
use origin_ipc::tls::generate_self_signed;

#[tokio::test(flavor = "current_thread")]
async fn quic_round_trips_one_frame() {
    let bundle = generate_self_signed("origin-test").expect("generate");
    let listener = QuicListener::bind("127.0.0.1:0".parse().expect("static literal"), bundle.clone())
        .await
        .expect("bind");
    let addr = listener.local_addr();
    let server_ca = bundle.ca_der.clone();

    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let (_kind, body) = conn.read_frame().await.expect("read");
        conn.write_frame(FrameKind::Response, &body).await.expect("write");
    });

    let mut client = QuicConnector::connect(addr, "origin-test", &server_ca)
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
