// SPDX-License-Identifier: Apache-2.0
use origin_provider_anthropic::streaming::parse_into_ring;
use origin_stream::{Ring, TokenKind};
use std::path::PathBuf;

#[tokio::test]
async fn fixture_emits_two_text_deltas_then_turn_end() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/sse_hello.txt");
    let body = std::fs::read(&path).expect("read fixture");

    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let parser = tokio::spawn(async move {
        parse_into_ring(body.as_slice(), &r).await.expect("parse");
        r.close();
    });

    let mut text_payloads: Vec<String> = Vec::new();
    let mut saw_turn_end = false;
    let mut saw_usage = false;
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::TextDelta => text_payloads.push(String::from_utf8_lossy(ev.payload()).into_owned()),
            TokenKind::TurnEnd => saw_turn_end = true,
            TokenKind::Usage => saw_usage = true,
            _ => {}
        }
    }
    parser.await.expect("parser");

    assert_eq!(text_payloads, vec!["Hel".to_string(), "lo!".to_string()]);
    assert!(saw_turn_end, "expected TurnEnd");
    assert!(saw_usage, "expected Usage");
}
