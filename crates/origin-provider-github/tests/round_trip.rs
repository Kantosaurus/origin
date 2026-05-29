// SPDX-License-Identifier: Apache-2.0
//! Round-trip wiremock test: the GitHub Models provider reads its bearer
//! token from the vault on every call, hits `POST /inference/chat/completions`,
//! and decodes an OpenAI-shape response into canonical [`Block::Text`].

use origin_core::types::{Block, Message, Role};
use origin_keyvault::{KeyVault, Secret};
use origin_provider::{ChatRequest, Provider, ProviderError};
use origin_provider_github::GitHubModels;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[allow(clippy::panic)]
async fn reads_token_from_vault_and_calls_github_models() {
    let vault = KeyVault::in_memory();
    let stored = r#"{"access":"gh-token-xyz","refresh":null,"expires_at":9999999999}"#;
    vault
        .set("github", "default/oauth", Secret::new(stored.to_string()))
        .await
        .expect("vault set");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/inference/chat/completions"))
        .and(header("authorization", "Bearer gh-token-xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1}
        })))
        .mount(&server)
        .await;

    let provider = GitHubModels::with_base_url(vault, "default", &server.uri());
    assert_eq!(provider.name(), "github-models");

    let resp = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
            model: "gpt-4o-mini".into(),
            tools: vec![],
        })
        .await
        .expect("github models chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.usage.input_tokens, 3);
    assert_eq!(resp.usage.output_tokens, 1);
    assert_eq!(resp.assistant.blocks.len(), 1);
    match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => assert_eq!(text, "hello"),
        other => panic!("expected Text block, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_vault_entry_maps_to_auth() {
    let vault = KeyVault::in_memory();
    let provider = GitHubModels::with_base_url(vault, "default", "http://127.0.0.1:1");
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "gpt-4o-mini".into(),
            tools: vec![],
        })
        .await
        .expect_err("missing vault entry must fail");
    assert!(matches!(err, ProviderError::Auth));
}

#[tokio::test]
async fn auth_error_status_maps_to_provider_auth() {
    let vault = KeyVault::in_memory();
    let stored = r#"{"access":"bad-token","refresh":null,"expires_at":9999999999}"#;
    vault
        .set("github", "default/oauth", Secret::new(stored.to_string()))
        .await
        .expect("vault set");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/inference/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "no auth" }
        })))
        .mount(&server)
        .await;
    let provider = GitHubModels::with_base_url(vault, "default", &server.uri());
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "gpt-4o-mini".into(),
            tools: vec![],
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, ProviderError::Auth));
}

#[tokio::test]
async fn token_is_reread_on_each_chat_call() {
    let vault = KeyVault::in_memory();
    let first = r#"{"access":"token-1","refresh":null,"expires_at":9999999999}"#;
    vault
        .set("github", "default/oauth", Secret::new(first.to_string()))
        .await
        .expect("vault set first");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/inference/chat/completions"))
        .and(header("authorization", "Bearer token-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "a"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/inference/chat/completions"))
        .and(header("authorization", "Bearer token-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "b"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = GitHubModels::with_base_url(vault.clone(), "default", &server.uri());
    provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "m".into(),
            tools: vec![],
        })
        .await
        .expect("first call");

    // Simulate a background refresh rotating the stored token.
    let second = r#"{"access":"token-2","refresh":null,"expires_at":9999999999}"#;
    vault
        .set("github", "default/oauth", Secret::new(second.to_string()))
        .await
        .expect("vault set second");

    provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "m".into(),
            tools: vec![],
        })
        .await
        .expect("second call");
}
