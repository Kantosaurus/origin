use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_sidecar::{summarize, SummaryDeliverer};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Default)]
struct EchoProvider;
#[async_trait]
impl Provider for EchoProvider {
    fn name(&self) -> &'static str {
        "echo"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message {
                role: Role::Assistant,
                blocks: vec![Block::Text {
                    text: "User asked for X and assistant did Y.".to_string(),
                    cache_marker: None,
                }],
            },
            usage: Usage::default(),
        })
    }
}

#[derive(Debug, Default)]
struct Capture(Mutex<Option<(String, u32, String)>>);
#[async_trait]
impl SummaryDeliverer for Capture {
    async fn deliver(&self, s: &str, t: u32, summary: &str) {
        *self.0.lock().await = Some((s.to_string(), t, summary.to_string()));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_invokes_provider_and_delivers_text() {
    let provider: Arc<dyn Provider> = Arc::new(EchoProvider);
    let cap = Arc::new(Capture::default());
    let transcript = vec![Message {
        role: Role::User,
        blocks: vec![Block::Text {
            text: "do thing".into(),
            cache_marker: None,
        }],
    }];
    summarize::run(&provider, "stub-model", "sess-1", 3, &transcript, cap.as_ref()).await;
    let got = cap.0.lock().await.clone().expect("delivered");
    assert_eq!(got.0, "sess-1");
    assert_eq!(got.1, 3);
    assert!(got.2.contains("User asked for X"), "got summary {:?}", got.2);
}

#[derive(Debug, Default)]
struct ErroringProvider;
#[async_trait]
impl Provider for ErroringProvider {
    fn name(&self) -> &'static str {
        "err"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Api("simulated".into()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn provider_error_falls_back_to_synthesized_summary() {
    let provider: Arc<dyn Provider> = Arc::new(ErroringProvider);
    let cap = Arc::new(Capture::default());
    let transcript = vec![Message {
        role: Role::Assistant,
        blocks: vec![Block::Text {
            text: "This is the final assistant message in the turn.".repeat(3),
            cache_marker: None,
        }],
    }];
    summarize::run(&provider, "m", "s", 0, &transcript, cap.as_ref()).await;
    let (_, _, summary) = cap.0.lock().await.clone().expect("delivered");
    assert!(!summary.is_empty());
    assert!(
        summary.len() <= 160,
        "fallback summary should be short, got {}",
        summary.len()
    );
}
