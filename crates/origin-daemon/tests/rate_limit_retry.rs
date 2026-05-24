//! `run_loop` retries `ProviderError::RateLimit` with backoff instead of
//! killing the turn. These tests exercise both the recovery path (a few
//! 429s then a real response succeeds) and the give-up path (a provider
//! that never recovers eventually surfaces the error).

#![allow(clippy::panic)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::atomic::{AtomicU32, Ordering};

/// Returns `RateLimit { retry_after_secs: 1 }` for the first `fail_n` calls,
/// then a single `done` `ChatResponse` thereafter.
struct RateLimitedThenOk {
    fail_n: u32,
    attempts: AtomicU32,
}

impl RateLimitedThenOk {
    const fn new(fail_n: u32) -> Self {
        Self {
            fail_n,
            attempts: AtomicU32::new(0),
        }
    }

    fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for RateLimitedThenOk {
    fn name(&self) -> &'static str {
        "rate-limited-then-ok"
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let n = self.attempts.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_n {
            return Err(ProviderError::RateLimit {
                retry_after_secs: 1,
            });
        }
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

/// Always returns `RateLimit`, so the retry layer exhausts its budget.
struct AlwaysRateLimited {
    attempts: AtomicU32,
}

impl AlwaysRateLimited {
    const fn new() -> Self {
        Self {
            attempts: AtomicU32::new(0),
        }
    }

    fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for AlwaysRateLimited {
    fn name(&self) -> &'static str {
        "always-rate-limited"
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::RateLimit {
            retry_after_secs: 1,
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn run_loop_retries_rate_limit_then_succeeds() {
    let provider = RateLimitedThenOk::new(2);
    let mut session = Session::new("test", "claude-test");
    let summary = run_loop(
        &mut session,
        "hello",
        &provider,
        &AlwaysAllow,
        &LoopOptions::default().without_streaming(),
    )
    .await
    .expect("loop should recover after rate-limit retries");
    assert_eq!(summary.assistant_text, "done");
    // 2 rate-limited calls + 1 successful call = 3 total
    assert_eq!(provider.attempts(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn run_loop_gives_up_after_max_rate_limit_retries() {
    let provider = AlwaysRateLimited::new();
    let mut session = Session::new("test", "claude-test");
    let err = run_loop(
        &mut session,
        "hello",
        &provider,
        &AlwaysAllow,
        &LoopOptions::default().without_streaming(),
    )
    .await
    .expect_err("must give up eventually");
    match &err {
        LoopError::RateLimitExhausted {
            model,
            attempts,
            last_retry_after_secs,
        } => {
            assert_eq!(model, "claude-test");
            assert_eq!(*attempts, 4);
            assert_eq!(*last_retry_after_secs, 1);
        }
        other => panic!("expected RateLimitExhausted, got {other:?}"),
    }
    // The Display string must surface the mid-session model-swap hint so a
    // human reading the TUI error knows the workaround without grepping docs.
    let rendered = format!("{err}");
    assert!(
        rendered.contains("/model"),
        "expected `/model` hint in rendered error, got: {rendered}"
    );
    assert!(
        rendered.contains("claude-test"),
        "expected failing model in rendered error, got: {rendered}"
    );
    // 1 initial attempt + MAX_PROVIDER_RETRIES (3) = 4 attempts total
    assert_eq!(provider.attempts(), 4);
}
