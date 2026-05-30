// SPDX-License-Identifier: Apache-2.0
//! Verifies that two `functionCall` parts with the same `name` in one Gemini
//! response receive distinct synthesized IDs. Prior to the fix, both calls
//! would surface as `call_<name>`, collapsing them in the daemon's per-id
//! `parsers`/`tool_input_bufs` maps.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_gemini::Gemini;
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn build_sse_body() -> String {
    // Two function_call parts with the same `name` in a single candidate.
    // Gemini's streaming format places parts inside `candidates[0].content.parts`.
    let frames = [
        r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"bash","args":{"cmd":"ls"}}}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"bash","args":{"cmd":"pwd"}}}]}}]}"#,
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":""}]},"finishReason":"STOP"}]}"#,
    ];
    let mut body = String::new();
    for f in frames {
        body.push_str("data: ");
        body.push_str(f);
        body.push_str("\n\n");
    }
    body
}

fn decode_tool_use_start_id(p: &[u8]) -> Option<String> {
    // Gemini's ToolUseStart payload: id + b'\0' + name (no LE prefix — the
    // gemini adapter does not synthesise indices because Gemini does not
    // surface parallel tool-call indices in the wire format).
    let nul = p.iter().position(|b| *b == 0)?;
    std::str::from_utf8(&p[..nul]).ok().map(str::to_string)
}

#[tokio::test]
async fn duplicate_function_name_yields_distinct_ids() {
    let server = MockServer::start().await;
    let body = build_sse_body();

    Mock::given(method("POST"))
        .and(path_regex(r"/v1beta/models/.*:streamGenerateContent"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let prov = Gemini::with_base_url("k-test", &server.uri());
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();

    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        prov.chat_stream(
            ChatRequest {
                system: String::new(),
                messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                model: "gemini-2.0-flash".into(),
                tools: vec![],
                effort: None,
                attachments: Vec::new(),
            },
            &r,
        )
        .await
        .expect("stream");
    });

    let mut ids: Vec<String> = Vec::new();
    while let Some(ev) = sub.next().await.expect("recv") {
        if matches!(ev.kind(), TokenKind::ToolUseStart) {
            let id = decode_tool_use_start_id(ev.payload()).expect("decode id");
            ids.push(id);
        }
    }
    prov_handle.await.expect("prov task");

    assert_eq!(ids.len(), 2, "expected two ToolUseStart events");
    assert_ne!(
        ids[0], ids[1],
        "duplicate function_call names must receive distinct ids (got {ids:?})",
    );
}
