// SPDX-License-Identifier: Apache-2.0
use origin_core::types::{Block, Message, Role};
use origin_planner::{Band, CachePlanner, PrefixLedger, Section, SectionId};
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cache_marker_emitted_on_planned_boundary() {
    let server = MockServer::start().await;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<Value>));
    let cap = captured.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test"))
        .respond_with(move |req: &wiremock::Request| {
            *cap.lock().expect("lock") = Some(req.body_json().expect("json"));
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "hi"}],
                "usage": {"input_tokens": 1, "output_tokens": 1,
                          "cache_read_input_tokens": 0,
                          "cache_creation_input_tokens": 0}
            }))
        })
        .mount(&server)
        .await;

    // Build a planner with sections that span Frozen→Sticky so the plan
    // contains one marker between section index 0 and 1.
    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let plan = planner.plan(&[
        Section::new(SectionId::new("system"), Band::Frozen, 0..32),
        Section::new(SectionId::new("memories"), Band::Sticky, 32..64),
    ]);

    let client = Anthropic::with_endpoint(server.uri(), "test", "claude-3-5-haiku-20241022").with_plan(plan);

    // One user message with two text blocks so block_idx 0 lines up with the
    // marker target.
    let msg = Message {
        role: Role::User,
        blocks: vec![
            Block::text("system-prompt placeholder"),
            Block::text("memories placeholder"),
        ],
    };

    let _ = client
        .chat(origin_provider::ChatRequest {
            system: String::new(),
            messages: vec![msg],
            model: "claude-3-5-haiku-20241022".into(),
            tools: vec![],
            effort: None,
            attachments: Vec::new(),
        })
        .await
        .expect("ok");

    let body = captured.lock().expect("lock").clone().expect("captured");
    let messages = body["messages"].as_array().expect("messages array");
    // At least one cache_control: ephemeral marker on a block.
    let saw_marker = messages.iter().any(|m| {
        m["content"]
            .as_array()
            .is_some_and(|cs| cs.iter().any(|c| c.get("cache_control").is_some()))
    });
    assert!(saw_marker, "expected at least one cache_control marker");
    let block_zero_has_marker = messages[0]["content"][0].get("cache_control").is_some();
    assert!(
        block_zero_has_marker,
        "marker must land on block 0 (the Frozen→Sticky boundary), got: {messages:?}"
    );
}
