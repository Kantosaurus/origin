// SPDX-License-Identifier: Apache-2.0
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_planner::{Band, Plan};
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use serde_json::Value;
use std::sync::Arc;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A1: CAS-handle tool results are inlined verbatim whatever their size or
/// registered band. A >2,048-byte `Volatile` (Mutating-tool) body used to be
/// swapped for an unresolvable `<result handle:XXXXXXXX — N bytes>` stub;
/// small bodies were always inlined and must stay byte-identical.
#[tokio::test]
async fn tool_result_handles_inlined_verbatim_regardless_of_size_or_band() {
    let server = MockServer::start().await;
    let captured = Arc::new(std::sync::Mutex::new(None::<Value>));
    let cap = captured.clone();
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            *cap.lock().expect("lock") = Some(req.body_json().expect("json"));
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1,
                          "cache_read_input_tokens": 0,
                          "cache_creation_input_tokens": 0}
            }))
        })
        .mount(&server)
        .await;

    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 3,
        })
        .expect("open"),
    );

    let small = vec![b's'; 2_048]; // at the old inline budget
    let big = vec![b'.'; 8_000]; // well over it
    let h_small = store.put(&small).expect("put");
    let h_big = store.put(&big).expect("put");
    // Register both as `Volatile` exactly as the daemon does for Mutating tools.
    let plan = Plan::default();
    plan.register_handle(*h_small.as_bytes(), Band::Volatile);
    plan.register_handle(*h_big.as_bytes(), Band::Volatile);

    let client = Anthropic::with_endpoint(server.uri(), "test", "claude-3-5-haiku-20241022")
        .with_cas(store.clone())
        .with_plan(plan);

    let tool_use = |id: &str| Block::ToolUse {
        id: id.into(),
        name: "Bash".into(),
        input_json: b"{}".to_vec(),
        cache_marker: None,
    };
    let tool_result = |id: &str, h: &origin_cas::Hash| Block::ToolResult {
        tool_use_id: id.into(),
        handle: Some(*h.as_bytes()),
        inline: None,
        cache_marker: None,
        is_error: false,
    };
    let _ = client
        .chat(origin_provider::ChatRequest {
            system: String::new(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    blocks: vec![tool_use("small"), tool_use("big")],
                },
                Message {
                    role: Role::Tool,
                    blocks: vec![tool_result("small", &h_small), tool_result("big", &h_big)],
                },
            ],
            model: "claude-3-5-haiku-20241022".into(),
            tools: vec![],
            effort: None,
            thinking_tokens: None,
            attachments: Vec::new(),
        })
        .await
        .expect("ok");

    let body = captured.lock().expect("lock").clone().expect("captured");
    let content = |i: usize| {
        body["messages"][1]["content"][i]["content"]
            .as_str()
            .expect("content str")
            .to_owned()
    };
    assert_eq!(
        content(0).as_bytes(),
        small.as_slice(),
        "small result sent verbatim"
    );
    let sent_big = content(1);
    assert!(
        sent_big.as_bytes() == big.as_slice(),
        "large result must be sent verbatim, got {} bytes: {:.80}",
        sent_big.len(),
        sent_big
    );
}
