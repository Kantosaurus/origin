use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use origin_mcp::{HttpTransport, McpClient};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn mock_server(port_tx: oneshot::Sender<u16>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    port_tx.send(port).expect("port_tx");

    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(|req: Request<hyper::body::Incoming>| async move {
                let path = req.uri().path().to_string();
                let body_bytes = if path == "/rpc" {
                    br#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"http_ping","description":"d","input_schema":{}}]}}"#.to_vec()
                } else {
                    b"{}".to_vec()
                };
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(body_bytes))))
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_tools_over_http() {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(mock_server(tx));
    let port = rx.await.expect("port");

    let url = format!("http://127.0.0.1:{port}/rpc");
    let transport: Arc<dyn origin_mcp::Transport> = Arc::new(HttpTransport::new(url, None));
    let client = McpClient::new(transport);
    client.initialize().await.expect("initialize");
    let tools = client.list_tools().await.expect("list");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "http_ping");
}
