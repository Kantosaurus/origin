// SPDX-License-Identifier: Apache-2.0
//! B1/B2: prompt-cache telemetry and the cache-stable request prefix.
//!
//! Anthropic's prompt cache only hits when the request prefix — tool schemas,
//! then the system prompt — is byte-identical to an earlier request. These
//! tests drive real `run_loop` prompts on one session, encode each captured
//! `ChatRequest` exactly as the Anthropic provider would, and compare bytes.
//! Every later prompt / tool-schema change must keep them green.

#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::StreamEvent;
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

/// Records every request and answers with plain text (so each prompt is one
/// request) carrying a fixed usage.
struct RecordingProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

const USAGE: Usage = Usage {
    input_tokens: 100,
    output_tokens: 7,
    cache_read_input_tokens: 50,
    cache_creation_input_tokens: 20,
};

#[async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: USAGE,
        })
    }
}

const fn recording() -> RecordingProvider {
    RecordingProvider {
        requests: Mutex::new(Vec::new()),
    }
}

/// A small Rust workspace for the repo map to rank: `leaf.rs` is referenced by
/// nothing, so naming it in a prompt re-ranks the (pre-B2) map.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (file, body) in [
        (
            "a.rs",
            "pub fn alpha() { crate::b::beta(); crate::c::gamma(); }\n",
        ),
        ("b.rs", "pub fn beta() { crate::c::gamma(); }\n"),
        ("c.rs", "pub fn gamma() {}\n"),
        (
            "d.rs",
            "pub fn delta() { crate::b::beta(); crate::c::gamma(); }\n",
        ),
        ("leaf.rs", "pub fn lonely_leaf() {}\npub fn other_leaf() {}\n"),
    ] {
        std::fs::write(src.join(file), body).unwrap();
    }
    dir
}

fn session_in(dir: &tempfile::TempDir) -> Session {
    let mut s = Session::new("recording", "claude-test");
    s.roots = vec![dir.path().to_path_buf()];
    s
}

fn opts() -> LoopOptions {
    LoopOptions {
        max_turns: 4,
        ..LoopOptions::default().without_streaming()
    }
}

/// The cacheable prefix of an Anthropic request: `tools` then `system`, as the
/// exact JSON bytes the encoder would send.
fn anthropic_prefix(req: &ChatRequest) -> (String, String) {
    let body = origin_provider_anthropic::encode_request_for_test(req);
    (body["tools"].to_string(), body["system"].to_string())
}

#[tokio::test]
async fn tools_and_system_bytes_identical_across_two_prompts() {
    let dir = workspace();
    let provider = recording();
    let mut session = session_in(&dir);
    let opts = opts();
    run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .unwrap();
    run_loop(
        &mut session,
        "now explain src/leaf.rs please",
        &provider,
        &AlwaysAllow,
        &opts,
    )
    .await
    .unwrap();

    let reqs = provider.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2, "one request per prompt");
    assert!(
        reqs[0].system.contains("<repo-map>"),
        "fixture must exercise the repo map:\n{}",
        reqs[0].system
    );
    // B6: the process's PATH probe rides in the stable prefix — whatever it
    // found on this machine (or nothing, under `ORIGIN_TOOL_PROBE=0`).
    let tools = origin_daemon::subsystems::tools_line().await;
    if tools.is_empty() {
        assert!(!reqs[0].system.contains("Tools on PATH"), "{}", reqs[0].system);
    } else {
        assert!(
            reqs[0].system.contains(&format!("\n{tools}\n")),
            "the tools line must ride in the system prompt:\n{}",
            reqs[0].system
        );
    }
    assert!(reqs[1].messages.len() > reqs[0].messages.len());
    let (tools0, system0) = anthropic_prefix(&reqs[0]);
    let (tools1, system1) = anthropic_prefix(&reqs[1]);
    assert!(tools0.len() > 2, "tools must be advertised");
    assert_same_bytes("tools", &tools0, &tools1);
    assert_same_bytes("system", &system0, &system1);
}

#[tokio::test]
async fn edit_between_prompts_leaves_system_bytes_and_marks_the_first_request() {
    // This asserts the DEFAULT (marked system) wire; ignore an
    // `ORIGIN_SYSTEM_CACHE=0` inherited from the tester's shell. Nothing else
    // in this binary sets the variable.
    std::env::remove_var("ORIGIN_SYSTEM_CACHE");
    let dir = workspace();
    let provider = recording();
    let mut session = session_in(&dir);
    let opts = opts();
    run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .unwrap();
    // An edit that re-ranks the map: a new, heavily-referenced file.
    std::fs::write(
        dir.path().join("src").join("hub.rs"),
        "pub fn hub_fn() { crate::leaf::lonely_leaf(); crate::leaf::other_leaf(); }\n",
    )
    .unwrap();
    run_loop(
        &mut session,
        "what does src/hub.rs do?",
        &provider,
        &AlwaysAllow,
        &opts,
    )
    .await
    .unwrap();

    let reqs = provider.requests.lock().unwrap().clone();
    let (_, system0) = anthropic_prefix(&reqs[0]);
    let (_, system1) = anthropic_prefix(&reqs[1]);
    assert_same_bytes("system", &system0, &system1);
    assert!(
        !reqs[1].system.contains("hub.rs"),
        "the session map is not re-scanned"
    );

    // The prompt-focused map entry rides in that prompt's user message, and
    // stays there (persisted) so later requests keep the same history bytes.
    let prompt_msg = session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::User))
        .find(|m| {
            m.blocks
                .iter()
                .any(|b| matches!(b, Block::Text { text, .. } if text.contains("src/hub.rs do")))
        })
        .expect("second prompt in the session");
    assert!(
        prompt_msg
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Text { text, .. } if text.contains("<repo-map-focus>") && text.contains("- src/hub.rs: hub_fn"))),
        "focused map entry must ride in the prompt's user message: {prompt_msg:?}"
    );

    // The very first request of the session already carries a cache marker
    // (on the system block), and no request exceeds Anthropic's 4 breakpoints.
    for req in &reqs {
        let body = origin_provider_anthropic::encode_request_for_test(req);
        let system = body["system"].as_array().expect("system is a block array");
        assert!(system.last().unwrap().get("cache_control").is_some());
        let markers = body.to_string().matches("\"cache_control\"").count();
        assert!((1..=4).contains(&markers), "{markers} cache_control markers");
    }
}

/// Byte equality with a readable failure: the first differing offset and the
/// text around it, not two multi-kilobyte dumps.
fn assert_same_bytes(what: &str, a: &str, b: &str) {
    if a == b {
        return;
    }
    let at = a
        .bytes()
        .zip(b.bytes())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()));
    let window = |s: &str| {
        String::from_utf8_lossy(&s.as_bytes()[at.saturating_sub(40)..(at + 120).min(s.len())]).into_owned()
    };
    panic!(
        "{what} bytes differ between prompts of one session at byte {at}:\n first: {}\nsecond: {}",
        window(a),
        window(b)
    );
}

#[tokio::test]
async fn tool_schema_bytes_are_deterministic_across_requests() {
    let provider = recording();
    let mut session = Session::new("recording", "claude-test");
    let opts = opts();
    for prompt in ["one", "two"] {
        run_loop(&mut session, prompt, &provider, &AlwaysAllow, &opts)
            .await
            .unwrap();
    }
    let reqs = provider.requests.lock().unwrap();
    assert_same_bytes(
        "tools",
        &anthropic_prefix(&reqs[0]).0,
        &anthropic_prefix(&reqs[1]).0,
    );
}

#[tokio::test]
async fn each_request_reports_usage_and_the_prefix_is_reported_once_per_session() {
    let provider = recording();
    let mut session = Session::new("recording", "claude-test");
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let opts = LoopOptions {
        event_tx: Some(tx),
        ..opts()
    };
    let drain = |rx: &mut tokio::sync::mpsc::Receiver<StreamEvent>| {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    };

    run_loop(&mut session, "first", &provider, &AlwaysAllow, &opts)
        .await
        .unwrap();
    let first = drain(&mut rx);
    let usages: Vec<_> = first
        .iter()
        .filter_map(|e| match e {
            StreamEvent::RequestUsage {
                input_tokens,
                output_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            } => Some((
                *input_tokens,
                *output_tokens,
                *cache_read_input_tokens,
                *cache_creation_input_tokens,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(usages, vec![(100, 7, 50, 20)], "one usage record per request");

    let prefixes: Vec<_> = first
        .iter()
        .filter_map(|e| match e {
            StreamEvent::PromptPrefix {
                system_bytes,
                tools_bytes,
                approx_tokens,
            } => Some((*system_bytes, *tools_bytes, *approx_tokens)),
            _ => None,
        })
        .collect();
    let req = provider.requests.lock().unwrap()[0].clone();
    let tools_bytes: usize = req
        .tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.input_schema_json.len())
        .sum();
    let system_bytes = req.system.len() as u64;
    let tools_bytes = tools_bytes as u64;
    assert_eq!(
        prefixes,
        vec![(system_bytes, tools_bytes, (system_bytes + tools_bytes) / 4)],
        "the prefix is reported on the session's first prompt"
    );

    run_loop(&mut session, "second", &provider, &AlwaysAllow, &opts)
        .await
        .unwrap();
    let second = drain(&mut rx);
    assert_eq!(
        second
            .iter()
            .filter(|e| matches!(e, StreamEvent::RequestUsage { .. }))
            .count(),
        1
    );
    assert!(
        !second
            .iter()
            .any(|e| matches!(e, StreamEvent::PromptPrefix { .. })),
        "the prefix is reported once per session, not per prompt"
    );
}
