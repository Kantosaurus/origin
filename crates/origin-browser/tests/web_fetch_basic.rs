use origin_browser::web_fetch::{fetch, FetchOptions};

#[tokio::test]
async fn fetch_extracts_main_content_as_markdown() {
    // Spin up a one-shot HTTP server returning a known HTML page.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await.unwrap();
        let body = "<html><head><title>Hi</title></head><body><article><h1>Hi</h1><p>World.</p></article></body></html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });

    let url = format!("http://{addr}/page");
    let out = fetch(&url, FetchOptions::default()).await.unwrap();
    assert!(out.markdown.contains("World."), "got: {}", out.markdown);
    assert_eq!(out.final_url, url);
}
