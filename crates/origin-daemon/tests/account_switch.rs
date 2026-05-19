//! P8.9 — `ProviderFactory` builds the right provider for each
//! `ProviderId`, and the `ClientMessage`/`StreamEvent::ProviderActive`
//! protocol additions round-trip through JSON cleanly.
#![allow(clippy::panic)]

use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use origin_daemon::provider_factory::{ProviderFactory, ProviderId};
use origin_keyvault::{KeyVault, Secret};

#[tokio::test]
async fn factory_builds_anthropic_from_vault() {
    let vault = KeyVault::in_memory();
    vault
        .set("anthropic", "default", Secret::new("sk-ant-A".to_string()))
        .await
        .expect("vault set anthropic");
    vault
        .set("openai", "default", Secret::new("sk-openai-A".to_string()))
        .await
        .expect("vault set openai");

    let factory = ProviderFactory::new(vault);
    let provider = factory
        .build(ProviderId::Anthropic, "default")
        .await
        .expect("build anthropic");
    assert_eq!(provider.name(), "anthropic");
}

#[tokio::test]
async fn factory_builds_openai_from_vault() {
    let vault = KeyVault::in_memory();
    vault
        .set("anthropic", "default", Secret::new("sk-ant-A".to_string()))
        .await
        .expect("vault set anthropic");
    vault
        .set("openai", "default", Secret::new("sk-openai-A".to_string()))
        .await
        .expect("vault set openai");

    let factory = ProviderFactory::new(vault);
    let provider = factory
        .build(ProviderId::OpenAi, "default")
        .await
        .expect("build openai");
    assert_eq!(provider.name(), "openai");
}

#[tokio::test]
async fn factory_missing_credential_surfaces_error() {
    let vault = KeyVault::in_memory();
    let factory = ProviderFactory::new(vault);
    let result = factory.build(ProviderId::Anthropic, "default").await;
    let err = match result {
        Ok(_) => panic!("expected MissingCredential, got Ok"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("anthropic") && msg.contains("default"),
        "error message should name the missing provider/account: {msg}"
    );
}

#[test]
fn client_message_prompt_round_trips() {
    let msg = ClientMessage::prompt(PromptRequest {
        system: "sys".into(),
        model: "claude-opus-4-7".into(),
        user_text: "hello".into(),
    });
    let json = serde_json::to_string(&msg).expect("serialize prompt");
    // Internally-tagged: `kind` discriminator sits next to the flattened fields.
    assert!(json.contains("\"kind\":\"prompt\""), "json was: {json}");
    assert!(json.contains("\"user_text\":\"hello\""), "json was: {json}");

    let back: ClientMessage = serde_json::from_str(&json).expect("deserialize prompt");
    match back {
        ClientMessage::Prompt(req) => {
            assert_eq!(req.system, "sys");
            assert_eq!(req.model, "claude-opus-4-7");
            assert_eq!(req.user_text, "hello");
        }
        ClientMessage::SwitchAccount { .. } | ClientMessage::MemoryDecision { .. } => {
            panic!("expected Prompt variant")
        }
    }
}

#[test]
fn client_message_switch_account_round_trips() {
    let msg = ClientMessage::SwitchAccount {
        provider: "openai".into(),
        account_id: "work".into(),
    };
    let json = serde_json::to_string(&msg).expect("serialize switch");
    assert!(json.contains("\"kind\":\"switch_account\""), "json was: {json}");
    assert!(json.contains("\"provider\":\"openai\""), "json was: {json}");

    let back: ClientMessage = serde_json::from_str(&json).expect("deserialize switch");
    match back {
        ClientMessage::SwitchAccount { provider, account_id } => {
            assert_eq!(provider, "openai");
            assert_eq!(account_id, "work");
        }
        ClientMessage::Prompt(_) | ClientMessage::MemoryDecision { .. } => {
            panic!("expected SwitchAccount variant")
        }
    }
}

#[test]
fn stream_event_provider_active_round_trips() {
    let ev = StreamEvent::ProviderActive {
        provider: "gemini".into(),
        account_id: "default".into(),
    };
    let json = serde_json::to_string(&ev).expect("serialize provider_active");
    assert!(json.contains("\"kind\":\"provider_active\""), "json was: {json}");
    assert!(json.contains("\"provider\":\"gemini\""), "json was: {json}");

    let back: StreamEvent = serde_json::from_str(&json).expect("deserialize provider_active");
    match back {
        StreamEvent::ProviderActive { provider, account_id } => {
            assert_eq!(provider, "gemini");
            assert_eq!(account_id, "default");
        }
        other => panic!("expected ProviderActive, got {other:?}"),
    }
}

#[test]
fn provider_id_parse_and_as_str_round_trip() {
    for s in ["anthropic", "openai", "gemini", "ollama"] {
        let id = ProviderId::parse(s).expect("known id");
        assert_eq!(id.as_str(), s);
    }
}
