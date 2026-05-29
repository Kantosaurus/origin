// SPDX-License-Identifier: Apache-2.0
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::{Connector, Listener};

fn unique_socket_path() -> String {
    let id = ulid::Ulid::new();
    #[cfg(unix)]
    {
        format!("{}/origin-test-{id}.sock", std::env::temp_dir().display())
    }
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\origin-test-{id}")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn echo_one_frame() {
    let path = unique_socket_path();
    let listener = Listener::bind(&path).await.expect("bind");
    let path_clone = path.clone();
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let body = conn.read_frame_body().await.expect("server read");
        conn.write_frame(FrameKind::Response, &body)
            .await
            .expect("server write");
        drop(path_clone);
    });

    let mut client = Connector::connect(&path).await.expect("connect");
    let req = encode(7, FrameKind::Request, b"ping");
    client.write_raw(&req).await.expect("client write");
    let resp = client.read_frame_body().await.expect("client read");
    assert_eq!(resp, b"ping");

    server.await.expect("server task");
}
