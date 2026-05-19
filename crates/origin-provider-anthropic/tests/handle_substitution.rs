use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_planner::{Band, CachePlanner, PrefixLedger, Section, SectionId};
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use serde_json::Value;
use std::sync::Arc;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn large_tool_result_emitted_as_reference_when_volatile() {
    let server = MockServer::start().await;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<Value>));
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

    let big = vec![b'.'; 8_000];
    let h = store.put(&big).expect("put");

    let ledger = PrefixLedger::new();
    let planner = CachePlanner::new(&ledger);
    let plan = planner.plan(&[Section::new(
        SectionId::new("turn-1"),
        Band::Volatile,
        0..big.len(),
    )]);

    let client = Anthropic::with_endpoint(server.uri(), "test", "claude-3-5-haiku-20241022")
        .with_cas(store.clone())
        .with_plan(plan);

    let msg = Message {
        role: Role::Tool,
        blocks: vec![Block::ToolResult {
            tool_use_id: "id1".into(),
            handle: Some(*h.as_bytes()),
            inline: None,
            cache_marker: None,
        }],
    };
    let _ = client
        .chat(origin_provider::ChatRequest {
            system: String::new(),
            messages: vec![msg],
            model: "claude-3-5-haiku-20241022".into(),
            tools: vec![],
        })
        .await
        .expect("ok");

    let body = captured.lock().expect("lock").clone().expect("captured");
    let content = body["messages"][0]["content"][0]["content"]
        .as_str()
        .expect("content str");
    assert!(
        content.starts_with("<result handle:") && content.contains("8000 bytes"),
        "expected reference, got: {content}"
    );
}
