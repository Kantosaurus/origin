// SPDX-License-Identifier: Apache-2.0
//! A7: compaction that works inside one long prompt.
//!
//! Summary-backed compaction can only fold turns that already have a Haiku
//! summary, and those are generated after the request returns — so during one
//! long autonomous prompt nothing is foldable. These tests pin the
//! summary-free path: old tool-result bodies are elided once the context
//! passes the high-water mark, and a provider context-length error triggers
//! exactly one aggressive elide-and-retry.
#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

const OVERFLOW: &str = "status 400: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long: 210000 tokens > 200000 maximum\"}}";

/// Replays `script` in order and records every request it receives.
struct Scripted {
    script: Mutex<Vec<Result<ChatResponse, ProviderError>>>,
    seen: Mutex<Vec<Vec<Message>>>,
}

impl Scripted {
    const fn new(script: Vec<Result<ChatResponse, ProviderError>>) -> Self {
        Self {
            script: Mutex::new(script),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn seen(&self) -> Vec<Vec<Message>> {
        self.seen.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Provider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen.lock().expect("lock").push(req.messages);
        let mut q = self.script.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("script exhausted".into()));
        }
        q.remove(0)
    }
}

fn text(t: &str) -> ChatResponse {
    ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text(t)),
        usage: Usage::default(),
    }
}

/// An assistant turn calling `Read` on a one-byte file, reporting `input_tokens`
/// as the measured prompt size of the request that produced it.
fn read_call(dir: &tempfile::TempDir, id: &str, input_tokens: u32) -> ChatResponse {
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "x").expect("write");
    ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::ToolUse {
            id: id.into(),
            name: "Read".into(),
            input_json: serde_json::to_vec(&serde_json::json!({ "file_path": file })).expect("json"),
            cache_marker: None,
        }),
        usage: Usage {
            input_tokens,
            ..Usage::default()
        },
    }
}

/// A session holding `turns` completed tool turns, each result `body_len` bytes.
fn seeded(turns: usize, body_len: usize) -> Session {
    let mut s = Session::new_with_id("sess-a7".into(), "test-model".into());
    s.messages
        .push(Message::new(Role::User).with_block(Block::text("the original task")));
    for k in 0..turns {
        s.messages
            .push(Message::new(Role::Assistant).with_block(Block::ToolUse {
                id: format!("T{k}"),
                name: "Read".into(),
                input_json: b"{}".to_vec(),
                cache_marker: None,
            }));
        s.messages
            .push(Message::new(Role::Tool).with_block(Block::ToolResult {
                tool_use_id: format!("T{k}"),
                handle: None,
                inline: Some(vec![b'x'; body_len]),
                cache_marker: None,
                is_error: false,
            }));
    }
    s
}

/// Inline body of the tool result answering `id`, wherever it sits; a body
/// still carrying its full payload is shown as `<N bytes>` to keep failures short.
fn result_body(msgs: &[Message], id: &str) -> String {
    let body = msgs
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult {
                tool_use_id, inline, ..
            } if tool_use_id == id => inline.clone(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool_result for {id}"));
    if body.iter().all(|&c| c == b'x') {
        format!("<{} bytes>", body.len())
    } else {
        String::from_utf8_lossy(&body).into_owned()
    }
}

/// Every `tool_result` answers a `tool_use` in the message just before it.
fn pairing_valid(msgs: &[Message]) -> bool {
    msgs.iter().enumerate().all(|(i, m)| {
        m.blocks.iter().all(|b| match b {
            Block::ToolResult { tool_use_id, .. } => {
                i > 0
                    && msgs[i - 1]
                        .blocks
                        .iter()
                        .any(|p| matches!(p, Block::ToolUse { id, .. } if id == tool_use_id))
            }
            _ => true,
        })
    })
}

#[tokio::test]
async fn context_length_error_triggers_exactly_one_elide_and_retry() {
    // Small bodies: the high-water mark is never crossed, so only the
    // context-length handler can elide anything.
    let mut session = seeded(6, 1_000);
    let provider = Scripted::new(vec![Err(ProviderError::Api(OVERFLOW.into())), Ok(text("done"))]);
    let opts = LoopOptions::default().without_streaming();

    run_loop(&mut session, "continue", &provider, &AlwaysAllow, &opts)
        .await
        .expect("the retry after eliding must succeed");

    let seen = provider.seen();
    assert_eq!(seen.len(), 2, "exactly one retry after the context-length error");
    assert_eq!(
        result_body(&seen[0], "T0"),
        "<1000 bytes>",
        "first attempt is untrimmed"
    );
    // Aggressive window: the last 3 assistant turns are T5, T4, T3 (the new
    // prompt has no assistant reply yet), so T0..T2 are elided and T3.. kept.
    for k in 0..3 {
        assert_eq!(
            result_body(&seen[1], &format!("T{k}")),
            "[tool result elided: Read 1000 bytes]"
        );
    }
    for k in 3..6 {
        assert_eq!(result_body(&seen[1], &format!("T{k}")), "<1000 bytes>");
    }
    assert!(pairing_valid(&seen[1]));
    // The session itself carries the elision so later turns stay small.
    assert_eq!(
        result_body(&session.messages, "T0"),
        "[tool result elided: Read 1000 bytes]"
    );
}

#[tokio::test]
async fn streaming_context_length_error_triggers_exactly_one_elide_and_retry() {
    // Same as above on the default streaming path: the error comes back out of
    // `chat_stream` (the default impl forwards `chat`'s error) via
    // `run_streaming_turn`, and must reach the same elide-and-retry arm.
    let mut session = seeded(6, 1_000);
    let provider = Scripted::new(vec![Err(ProviderError::Api(OVERFLOW.into())), Ok(text("done"))]);
    let opts = LoopOptions::default();

    run_loop(&mut session, "continue", &provider, &AlwaysAllow, &opts)
        .await
        .expect("the streamed retry after eliding must succeed");

    let seen = provider.seen();
    assert_eq!(seen.len(), 2, "exactly one retry after the context-length error");
    assert_eq!(result_body(&seen[0], "T0"), "<1000 bytes>");
    for k in 0..3 {
        assert_eq!(
            result_body(&seen[1], &format!("T{k}")),
            "[tool result elided: Read 1000 bytes]"
        );
    }
    for k in 3..6 {
        assert_eq!(result_body(&seen[1], &format!("T{k}")), "<1000 bytes>");
    }
    assert!(pairing_valid(&seen[1]));
}

#[tokio::test]
async fn sustained_pressure_does_not_elide_one_result_per_turn() {
    // The measured prompt (150 K of a 200 K window: system prompt + schemas
    // that elision can never shrink) keeps the 70 % gate firing every turn,
    // while each aged-out 4 KB result frees almost nothing. Eliding the single
    // newly-aged result each turn would break the prompt cache every turn and
    // never get back under the mark — each firing must be a batch that reaches
    // low water or a no-op.
    let dir = tempfile::tempdir().expect("tmp");
    let mut session = seeded(11, 4_000);
    let provider = Scripted::new(vec![
        Ok(read_call(&dir, "live1", 150_000)),
        Ok(read_call(&dir, "live2", 150_000)),
        Ok(text("done")),
    ]);
    let opts = LoopOptions::default().without_streaming();

    run_loop(&mut session, "continue", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    let seen = provider.seen();
    assert_eq!(seen.len(), 3);
    for (turn, req) in seen.iter().enumerate().skip(1) {
        for k in 0..11 {
            assert_eq!(
                result_body(req, &format!("T{k}")),
                "<4000 bytes>",
                "request {turn}: T{k} must not be trickle-elided"
            );
        }
    }
}

#[tokio::test]
async fn context_length_error_with_nothing_to_elide_is_not_retried() {
    // Only 2 assistant turns: the 3-turn window reaches nothing, so a retry
    // would resend the identical request — return the error instead.
    let mut session = seeded(2, 1_000);
    let provider = Scripted::new(vec![Err(ProviderError::Api(OVERFLOW.into())), Ok(text("done"))]);
    let opts = LoopOptions::default().without_streaming();

    run_loop(&mut session, "continue", &provider, &AlwaysAllow, &opts)
        .await
        .expect_err("nothing to elide ⇒ the overflow surfaces");
    assert_eq!(provider.seen().len(), 1, "no pointless identical retry");
}

#[tokio::test]
async fn a_second_context_length_error_is_returned() {
    let mut session = seeded(6, 1_000);
    let provider = Scripted::new(vec![
        Err(ProviderError::Api(OVERFLOW.into())),
        Err(ProviderError::Api(OVERFLOW.into())),
        Ok(text("never reached")),
    ]);
    let opts = LoopOptions::default().without_streaming();

    let err = run_loop(&mut session, "continue", &provider, &AlwaysAllow, &opts)
        .await
        .expect_err("a second overflow must surface");
    assert!(err.to_string().contains("prompt is too long"), "{err}");
    assert_eq!(provider.seen().len(), 2, "one retry, never a second");
}

#[tokio::test]
async fn over_high_water_elides_old_results_before_the_next_request() {
    // 12 turns × 60 KB ≈ 720 KB, past 70 % of "test-model"'s 200 K-token
    // fallback window (560 KB) — with no summaries available to fold.
    let dir = tempfile::tempdir().expect("tmp");
    let mut session = seeded(12, 60_000);
    let provider = Scripted::new(vec![Ok(read_call(&dir, "live", 0)), Ok(text("done"))]);
    let opts = LoopOptions::default().without_streaming();

    run_loop(&mut session, "continue", &provider, &AlwaysAllow, &opts)
        .await
        .expect("loop ok");

    let seen = provider.seen();
    assert_eq!(seen.len(), 2);
    let second = &seen[1];
    // Assistant turns are T0..T11 + "live". Keeping 10 (eliding T0..T2) would
    // leave ~540 KB, above the 400 KB low-water mark, so the window steps down
    // to 7: T0..T5 are elided in one batch and T6.. stay verbatim (~360 KB).
    for k in 0..6 {
        assert_eq!(
            result_body(second, &format!("T{k}")),
            "[tool result elided: Read 60000 bytes]"
        );
    }
    for k in 6..12 {
        assert_eq!(
            result_body(second, &format!("T{k}")),
            "<60000 bytes>",
            "T{k} kept"
        );
    }
    assert!(pairing_valid(second));
    assert!(
        matches!(second[0].blocks.first(), Some(Block::Text { text, .. }) if text == "the original task"),
        "turn 0 is never touched"
    );
}
