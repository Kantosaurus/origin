// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_browser::web_fetch::{fetch, FetchError, FetchOptions};

/// One-shot local HTTP server returning a known HTML page; yields its address.
async fn serve_once() -> std::net::SocketAddr {
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
            body.len(),
            body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn fetch_extracts_main_content_as_markdown() {
    let addr = serve_once().await;
    let url = format!("http://{addr}/page");
    // Loopback is refused unless the dev escape hatch is on.
    let opts = FetchOptions {
        allow_private: true,
        ..FetchOptions::default()
    };
    let out = fetch(&url, opts).await.unwrap();
    assert!(out.markdown.contains("World."), "got: {}", out.markdown);
    assert_eq!(out.final_url, url);
}

/// A9b: a live local server is refused when private targets are not allowed,
/// both by literal IP and by a hostname that resolves to loopback.
#[tokio::test]
async fn local_server_is_refused_without_escape_hatch() {
    let addr = serve_once().await;
    let opts = FetchOptions {
        allow_private: false,
        ..FetchOptions::default()
    };

    let literal = fetch(&format!("http://{addr}/page"), opts.clone()).await;
    assert!(
        matches!(literal, Err(FetchError::Blocked(_))),
        "literal loopback must be refused: {literal:?}"
    );

    // `localhost` is a hostname: refused after DNS resolution.
    let named = fetch(&format!("http://localhost:{}/page", addr.port()), opts).await;
    let err = named.expect_err("hostname resolving to loopback must be refused");
    let mut chain = String::new();
    let mut src: Option<&dyn std::error::Error> = Some(&err);
    while let Some(e) = src {
        chain.push_str(&e.to_string());
        src = e.source();
    }
    assert!(
        chain.contains("non-public address"),
        "refusal reason missing: {chain}"
    );
}
