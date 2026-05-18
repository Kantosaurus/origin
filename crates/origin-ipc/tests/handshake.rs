use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::{Connector, Listener};

fn unique_socket_path() -> String {
    let id = ulid::Ulid::new();
    #[cfg(unix)]
    {
        format!("{}/origin-handshake-{id}.sock", std::env::temp_dir().display())
    }
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\origin-handshake-{id}")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_responds_to_ping() {
    let path = unique_socket_path();
    let listener = Listener::bind(&path).await.expect("bind");
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let body = conn.read_frame_body().await.expect("read");
        assert_eq!(body, b"ping");
        conn.write_frame(FrameKind::Response, b"pong")
            .await
            .expect("write");
    });

    let mut client = Connector::connect(&path).await.expect("connect");
    let req = encode(1, FrameKind::Request, b"ping");
    client.write_raw(&req).await.expect("client write");
    let body = client.read_frame_body().await.expect("client read");
    assert_eq!(body, b"pong");

    server.await.expect("server task");
}
