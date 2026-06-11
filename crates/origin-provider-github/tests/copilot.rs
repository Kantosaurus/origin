// SPDX-License-Identifier: Apache-2.0
//! Round-trip test for the GitHub **Copilot** provider: it exchanges the stored
//! GitHub OAuth token (`token` scheme) for a short-lived Copilot session token
//! at `/copilot_internal/v2/token`, then calls `POST /chat/completions` with the
//! SESSION token (`Bearer`) plus the editor-identity headers Copilot requires.

use origin_core::types::{Block, Message, Role};
use origin_keyvault::{KeyVault, Secret};
use origin_provider::{ChatRequest, Provider, ProviderError};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req(text: &str) -> ChatRequest {
    ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text(text))],
        model: "gpt-4o".into(),
        tools: vec![],
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
    }
}

#[tokio::test]
#[allow(clippy::panic)]
async fn exchanges_oauth_token_then_calls_copilot_chat_with_editor_headers() {
    let vault = KeyVault::in_memory();
    // The device flow stores the ghu_ token under the catalog id `github-copilot`.
    let stored = r#"{"access":"ghu_oauth123","expires_at":9999999999}"#;
    vault
        .set("github-copilot", "default/oauth", Secret::new(stored.to_string()))
        .await
        .expect("vault set");

    let server = MockServer::start().await;
    // 1. Token exchange: GitHub OAuth token (token scheme) -> Copilot session token.
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .and(header("authorization", "token ghu_oauth123"))
        .and(header("editor-version", "vscode/1.85.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token": "copilot-session-tok",
            "expires_at": 9_999_999_999u64
        })))
        .mount(&server)
        .await;
    // 2. Chat: must carry the SESSION token as Bearer + the Copilot integration id.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer copilot-session-tok"))
        .and(header("copilot-integration-id", "vscode-chat"))
        .and(header("editor-version", "vscode/1.85.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hi from copilot"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        })))
        .mount(&server)
        .await;

    // Redirect the exchange endpoint to the mock server (only the Copilot source
    // reads this env var).
    std::env::set_var(
        "ORIGIN_COPILOT_TOKEN_URL",
        format!("{}/copilot_internal/v2/token", server.uri()),
    );

    let provider = origin_provider_github::copilot::provider_with_base(vault, "default", &server.uri());
    assert_eq!(provider.name(), "github-copilot");

    let resp = provider
        .chat(req("ping"))
        .await
        .expect("copilot chat should succeed");

    std::env::remove_var("ORIGIN_COPILOT_TOKEN_URL");

    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 2);
    assert_eq!(resp.assistant.role, Role::Assistant);
    match resp.assistant.blocks.first() {
        Some(Block::Text { text, .. }) => assert_eq!(text, "hi from copilot"),
        other => panic!("expected a Text block, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_vault_entry_maps_to_auth() {
    // No stored token -> the exchange can't even start -> ProviderError::Auth.
    let vault = KeyVault::in_memory();
    let provider =
        origin_provider_github::copilot::provider_with_base(vault, "default", "http://127.0.0.1:1");
    let err = provider
        .chat(req("ping"))
        .await
        .expect_err("missing vault entry must fail");
    assert!(matches!(err, ProviderError::Auth));
}
