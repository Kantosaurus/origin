#![allow(clippy::unwrap_used)]

use origin_browser::web_search::{search_with_endpoint, SearchOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn returns_parsed_results_from_tavily_shaped_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf).await.unwrap();
        let body = r#"{"results":[{"title":"T","url":"https://x","content":"snip"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        s.write_all(resp.as_bytes()).await.unwrap();
    });
    let opts = SearchOptions { api_key: "k".into(), count: 5 };
    let r = search_with_endpoint(&format!("http://{addr}/search"), "q", opts).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].title, "T");
    assert_eq!(r[0].url, "https://x");
    assert_eq!(r[0].snippet, "snip");
}

#[tokio::test]
async fn errors_clearly_when_api_key_missing() {
    std::env::remove_var("TAVILY_API_KEY");
    let err = origin_browser::web_search::search("q", 5).await.unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("tavily_api_key"));
}
