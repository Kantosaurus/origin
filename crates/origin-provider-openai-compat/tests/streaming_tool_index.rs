//! Verifies that the openai-compat SSE → ring adapter propagates each
//! tool_call's `index` into the ring payload, matching the wire shape the
//! Anthropic provider emits (see `origin-provider-anthropic/src/streaming.rs`)
//! and which the daemon consumer expects (see `origin-daemon/src/agent.rs`
//! around the `ToolUseStart` / `ToolUseDelta` decode sites).
//!
//! Wire shape:
//! - `ToolUseStart`  : 4-byte LE index + id bytes + b'\0' + name bytes
//! - `ToolUseDelta`  : 4-byte LE index + partial JSON bytes
//!
//! Without an index prefix, the daemon cannot demux fragments of parallel
//! tool calls back to the right tool.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig, StaticBearer};
use origin_stream::{Ring, TokenKind};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// SSE frame for two interleaved tool_calls (indices 0 and 1). The first
/// fragment per index carries `id` + `function.name`; later fragments carry
/// only `function.arguments`.
fn build_sse_body() -> String {
    // Frames are JSON-as-string with embedded data: prefixes and blank-line
    // separators. We split this into separate `data: …\n\n` chunks.
    let frames = [
        // index 0 starts
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_A","function":{"name":"alpha","arguments":""}}]}}]}"#,
        // index 1 starts
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_B","function":{"name":"beta","arguments":""}}]}}]}"#,
        // index 0 args fragment
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":1}"}}]}}]}"#,
        // index 1 args fragment
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"b\":2}"}}]}}]}"#,
        // finish
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let mut body = String::new();
    for f in frames {
        body.push_str("data: ");
        body.push_str(f);
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn decode_tool_use_start(p: &[u8]) -> Option<(u32, String, String)> {
    if p.len() < 4 {
        return None;
    }
    let idx = u32::from_le_bytes(p[..4].try_into().ok()?);
    let rest = &p[4..];
    let nul = rest.iter().position(|b| *b == 0)?;
    let id = std::str::from_utf8(&rest[..nul]).ok()?.to_string();
    let name = std::str::from_utf8(&rest[nul + 1..]).ok()?.to_string();
    Some((idx, id, name))
}

fn decode_tool_use_delta(p: &[u8]) -> Option<(u32, String)> {
    if p.len() < 4 {
        return None;
    }
    let idx = u32::from_le_bytes(p[..4].try_into().ok()?);
    let args = std::str::from_utf8(&p[4..]).ok()?.to_string();
    Some((idx, args))
}

#[tokio::test]
async fn parallel_tool_calls_carry_index_prefix() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(build_sse_body().into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let cfg = OpenAiCompatConfig {
        name: "test",
        base_url: server.uri(),
        chat_path: "/v1/chat/completions".to_string(),
        auth: StaticBearer::new("sk-test"),
        extra_headers: vec![],
    };
    let provider = OpenAiCompat::new(cfg);

    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let prov_handle = tokio::spawn(async move {
        provider
            .chat_stream(
                ChatRequest {
                    system: String::new(),
                    messages: vec![Message::new(Role::User).with_block(Block::text("hi"))],
                    model: "gpt-test".into(),
                    tools: vec![],
                },
                &r,
            )
            .await
            .expect("stream");
    });

    let mut starts: Vec<(u32, String, String)> = Vec::new();
    let mut deltas: Vec<(u32, String)> = Vec::new();
    while let Some(ev) = sub.next().await.expect("recv") {
        match ev.kind() {
            TokenKind::ToolUseStart => {
                let (idx, id, name) = decode_tool_use_start(ev.payload())
                    .expect("ToolUseStart must carry LE index + id\\0name");
                starts.push((idx, id, name));
            }
            TokenKind::ToolUseDelta => {
                let (idx, args) =
                    decode_tool_use_delta(ev.payload()).expect("ToolUseDelta must carry LE index prefix");
                deltas.push((idx, args));
            }
            TokenKind::TurnEnd => {}
            _ => {}
        }
    }
    prov_handle.await.expect("prov task");

    assert_eq!(
        starts,
        vec![
            (0, "call_A".to_string(), "alpha".to_string()),
            (1, "call_B".to_string(), "beta".to_string()),
        ],
        "ToolUseStart events must preserve tc.index from the wire",
    );
    assert_eq!(
        deltas,
        vec![
            // index 0 emits an empty-args delta on start, then the real args
            (0, String::new()),
            (1, String::new()),
            (0, "{\"a\":1}".to_string()),
            (1, "{\"b\":2}".to_string()),
        ],
        "ToolUseDelta events must carry LE-prefixed index matching their tc.index",
    );
}
