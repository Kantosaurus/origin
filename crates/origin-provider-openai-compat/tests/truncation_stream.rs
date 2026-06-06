// SPDX-License-Identifier: Apache-2.0
//! Verifies the streaming finalize path surfaces a length-limit truncation for
//! quirky (non-`OpenAI`) backends by tagging the terminal `TurnEnd` with
//! `TURNEND_TRUNCATED_MARKER`, while leaving the `TurnEnd` payload empty for a
//! clean stop and for every `OpenAI` backend (byte-identical wire).
//!
//! Drives `streaming::parse_into_ring` directly with an in-memory
//! `reqwest::Response` so both backend classifications can be exercised
//! deterministically with no server.

#![allow(clippy::unwrap_used, clippy::panic)]

use origin_provider_openai_compat::streaming::{parse_into_ring, TURNEND_TRUNCATED_MARKER};
use origin_shimquirks::Backend;
use origin_stream::{Ring, TokenKind};

/// Build an SSE event-stream body whose final frame carries `finish_reason`.
fn sse_with_finish(finish_reason: &str) -> String {
    let frames = [
        r#"{"choices":[{"delta":{"content":"partial answer"}}]}"#.to_string(),
        format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{finish_reason}"}}]}}"#),
    ];
    let mut body = String::new();
    for f in frames {
        body.push_str("data: ");
        body.push_str(&f);
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// Wrap raw SSE text into a synthetic `200 OK` `reqwest::Response` (no network).
fn response_from_sse(sse: String) -> reqwest::Response {
    reqwest::Response::from(http::Response::new(sse))
}

/// Collect every `TurnEnd` payload observed while draining the ring.
async fn drain_turn_end_payloads(resp: reqwest::Response, backend: Backend) -> Vec<Vec<u8>> {
    let ring = Ring::with_capacity(64 * 1024);
    let mut sub = ring.subscribe();
    let r = ring.clone();
    let prov = tokio::spawn(async move {
        parse_into_ring(resp, &r, backend).await.expect("parse");
        r.close();
    });

    let mut payloads = Vec::new();
    while let Some(ev) = sub.next().await.expect("recv") {
        if ev.kind() == TokenKind::TurnEnd {
            payloads.push(ev.payload().to_vec());
        }
    }
    prov.await.expect("task");
    payloads
}

#[tokio::test]
async fn length_finish_on_quirky_backend_tags_turn_end() {
    let payloads =
        drain_turn_end_payloads(response_from_sse(sse_with_finish("length")), Backend::Other).await;
    assert_eq!(payloads.len(), 1, "exactly one TurnEnd");
    assert_eq!(
        payloads[0], TURNEND_TRUNCATED_MARKER,
        "a length finish on a non-OpenAi backend must mark TurnEnd as truncated"
    );
}

#[tokio::test]
async fn max_tokens_finish_on_quirky_backend_tags_turn_end() {
    let payloads = drain_turn_end_payloads(
        response_from_sse(sse_with_finish("max_tokens")),
        Backend::VLlm,
    )
    .await;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0], TURNEND_TRUNCATED_MARKER);
}

#[tokio::test]
async fn length_finish_on_openai_backend_is_noop() {
    // Same length finish, but OpenAi must leave the TurnEnd payload empty so the
    // streamed wire is byte-identical to the pre-wiring behavior.
    let payloads =
        drain_turn_end_payloads(response_from_sse(sse_with_finish("length")), Backend::OpenAi).await;
    assert_eq!(payloads.len(), 1);
    assert!(
        payloads[0].is_empty(),
        "OpenAi truncation detection is a deliberate no-op: TurnEnd stays empty"
    );
}

#[tokio::test]
async fn clean_stop_finish_leaves_turn_end_empty() {
    // A natural `stop` finish — even on a quirky backend — is not a truncation,
    // so the TurnEnd payload stays empty.
    let payloads =
        drain_turn_end_payloads(response_from_sse(sse_with_finish("stop")), Backend::Other).await;
    assert_eq!(payloads.len(), 1);
    assert!(
        payloads[0].is_empty(),
        "a clean stop must not be flagged as truncation"
    );
}
