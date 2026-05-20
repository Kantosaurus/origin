//! End-to-end: catalog row → ProviderFactory → wiremock → real chat response.

#![allow(clippy::unwrap_used)]

use origin_core::types::{Block, Message, Role};
use origin_daemon::provider_factory::{ProviderFactory, ProviderId};
use origin_keyvault::{KeyVault, Secret};
use origin_provider::catalog::Catalog;
use origin_provider::ChatRequest;
use serde_json::json;
use wiremock::matchers::{header, method, path};
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

#[tokio::test]
async fn anthropic_oauth_row_uses_bearer_header() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer anthro-oauth-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "hello from anthropic oauth" }],
            "usage": { "input_tokens": 2, "output_tokens": 5,
                       "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0 }
        })))
        .mount(&server)
        .await;

    // Inject a custom catalog row pointing at the wiremock server.
    let mut catalog = Catalog::builtin();
    let entry = origin_provider::catalog::ProviderEntry {
        id: "test-anthropic-oauth".into(),
        display_name: "Test Anthropic OAuth".into(),
        wire: origin_provider::catalog::WireFormat::Anthropic,
        auth: origin_provider::catalog::AuthScheme::OAuth(origin_provider::catalog::OAuthSpec {
            authorize_url: "https://claude.ai/oauth/authorize".into(),
            token_url: "https://console.anthropic.com/v1/oauth/token".into(),
            client_id: "test-client-id".into(),
            scopes: std::borrow::Cow::Borrowed(&[]),
            redirect_uri: "http://localhost".into(),
            pkce: true,
            device_flow: false,
        }),
        base_url: server.uri().into(),
        chat_path: "/v1/messages".into(),
        default_model: "claude-sonnet-4-6".into(),
        capabilities: origin_provider::catalog::Capabilities::default(),
    };
    catalog.merge_custom(vec![entry]).unwrap();

    // Provision an OAuth blob with a far-future expires_at.
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let oauth_blob = format!(
        r#"{{"access":"anthro-oauth-tok","refresh":null,"expires_at":{expires_at}}}"#
    );
    let vault = KeyVault::in_memory();
    vault
        .set(
            "test-anthropic-oauth",
            "default/oauth",
            Secret::new(oauth_blob),
        )
        .await
        .unwrap();

    let factory = ProviderFactory::new(vault, catalog);
    let id = ProviderId::parse("test-anthropic-oauth", factory.catalog()).unwrap();
    let provider = factory.build(&id, "default").await.unwrap();

    let resp = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![Message {
                role: Role::User,
                blocks: vec![Block::Text {
                    text: "hi".into(),
                    cache_marker: None,
                }],
            }],
            model: "claude-sonnet-4-6".to_string(),
            tools: vec![],
        })
        .await
        .unwrap();

    let text: String = resp
        .assistant
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello from anthropic oauth");
}

#[tokio::test]
async fn openai_codex_oauth_row_uses_bearer() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer oauth-tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "codex says hi" } }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 3 }
        })))
        .mount(&server)
        .await;

    // Inject a custom catalog row for test-codex pointing at wiremock.
    let mut catalog = Catalog::builtin();
    let entry = origin_provider::catalog::ProviderEntry {
        id: "test-codex".into(),
        display_name: "Test Codex".into(),
        wire: origin_provider::catalog::WireFormat::OpenAIChat,
        auth: origin_provider::catalog::AuthScheme::OAuth(origin_provider::catalog::OAuthSpec {
            authorize_url: "https://auth.openai.com/oauth/authorize".into(),
            token_url: "https://auth.openai.com/oauth/token".into(),
            client_id: "app_test".into(),
            scopes: std::borrow::Cow::Borrowed(&[]),
            redirect_uri: "http://localhost:1455/auth/callback".into(),
            pkce: true,
            device_flow: false,
        }),
        base_url: server.uri().into(),
        chat_path: "/v1/chat/completions".into(),
        default_model: "gpt-5-codex".into(),
        capabilities: origin_provider::catalog::Capabilities::default(),
    };
    catalog.merge_custom(vec![entry]).unwrap();

    // Provision an OAuth blob with a far-future expires_at.
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let oauth_blob = format!(
        r#"{{"access":"oauth-tok","refresh":null,"expires_at":{expires_at}}}"#
    );
    let vault = KeyVault::in_memory();
    vault
        .set("test-codex", "default/oauth", Secret::new(oauth_blob))
        .await
        .unwrap();

    let factory = ProviderFactory::new(vault, catalog);
    let id = ProviderId::parse("test-codex", factory.catalog()).unwrap();
    let provider = factory.build(&id, "default").await.unwrap();

    let resp = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![Message {
                role: Role::User,
                blocks: vec![Block::Text {
                    text: "hello".into(),
                    cache_marker: None,
                }],
            }],
            model: "gpt-5-codex".to_string(),
            tools: vec![],
        })
        .await
        .unwrap();

    let text: String = resp
        .assistant
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "codex says hi");
}

#[tokio::test]
async fn github_copilot_row_builds() {
    let catalog = Catalog::builtin();
    assert!(catalog.lookup("github-copilot").is_some());
}
