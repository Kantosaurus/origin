//! End-to-end: catalog row → ProviderFactory → wiremock → real chat response.

#![allow(clippy::unwrap_used)]

use origin_core::types::{Block, Message, Role};
use origin_daemon::provider_factory::{ProviderFactory, ProviderId};
use origin_keyvault::{KeyVault, Secret};
use origin_provider::catalog::Catalog;
use origin_provider::ChatRequest;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn deepseek_row_builds_and_chats() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "hi from deepseek" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 4 }
        })))
        .mount(&server)
        .await;

    // Substitute the deepseek base_url with the wiremock URL by inserting a
    // user-defined catalog row that shadows nothing (different id).
    let mut catalog = Catalog::builtin();
    let entry = origin_provider::catalog::ProviderEntry {
        id: "test-deepseek".into(),
        display_name: "Test DeepSeek".into(),
        wire: origin_provider::catalog::WireFormat::OpenAIChat,
        auth: origin_provider::catalog::AuthScheme::ApiKey {
            header: "Authorization".into(),
            prefix: "Bearer ".into(),
        },
        base_url: server.uri().into(),
        chat_path: "/v1/chat/completions".into(),
        default_model: "deepseek-chat".into(),
        capabilities: origin_provider::catalog::Capabilities::default(),
    };
    catalog.merge_custom(vec![entry]).unwrap();

    let vault = KeyVault::in_memory();
    vault.set("test-deepseek", "default", Secret::new("sk-test".to_string())).await.unwrap();

    let factory = ProviderFactory::new(vault, catalog);
    let id = ProviderId::parse("test-deepseek", factory.catalog()).unwrap();
    let provider = factory.build(&id, "default").await.unwrap();

    let resp = provider.chat(ChatRequest {
        system: String::new(),
        messages: vec![Message { role: Role::User, blocks: vec![Block::Text { text: "hi".into(), cache_marker: None }] }],
        model: "deepseek-chat".to_string(),
        tools: vec![],
    }).await.unwrap();

    let text: String = resp.assistant.blocks.iter().filter_map(|b| match b {
        Block::Text { text, .. } => Some(text.clone()),
        _ => None,
    }).collect();
    assert_eq!(text, "hi from deepseek");
}
