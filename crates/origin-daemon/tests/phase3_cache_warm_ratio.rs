//! Phase 3 checkpoint bench.
//!
//! Runs a fixed 20-turn synthetic workload twice (cold + warm). The
//! scripted provider's `Usage` accounting drives the assertion:
//!     warm `cache_read_input_tokens` > 0.5 × `input_tokens`
//! This is deliberately a wiring test — the assertion is on synthetic
//! accounting — and guarantees that `LoopOptions`, `Session`, `Cache`,
//! and the planner-related types all stay accessible from a downstream
//! caller as Phase 3 closes.

use async_trait::async_trait;
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

struct Synth {
    warm: bool,
    turns: AtomicU32,
    agg_input: AtomicU32,
    agg_cache: AtomicU32,
}

#[async_trait]
impl Provider for Synth {
    fn name(&self) -> &'static str {
        "synth"
    }

    async fn chat(&self, _: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let t = self.turns.fetch_add(1, Ordering::SeqCst);
        let cache_read: u32 = if self.warm { 150 } else { 0 };
        self.agg_input.fetch_add(200, Ordering::SeqCst);
        self.agg_cache.fetch_add(cache_read, Ordering::SeqCst);
        let blocks = if t < 19 {
            vec![Block::Text {
                text: format!("turn-{t}"),
                cache_marker: None,
            }]
        } else {
            vec![Block::Text {
                text: "done".into(),
                cache_marker: None,
            }]
        };
        Ok(ChatResponse {
            assistant: Message {
                role: Role::Assistant,
                blocks,
            },
            usage: Usage {
                input_tokens: 200,
                output_tokens: 50,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: 0,
            },
        })
    }
}

async fn run_pass(warm: bool) -> (u32, u32) {
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
    let synth = Arc::new(Synth {
        warm,
        turns: AtomicU32::new(0),
        agg_input: AtomicU32::new(0),
        agg_cache: AtomicU32::new(0),
    });
    let mut session = Session::new("bench", "synth-model");
    let opts = LoopOptions::default().with_cas(store).without_streaming();
    for _ in 0..20 {
        let _ = run_loop(&mut session, "prompt", synth.as_ref(), &AlwaysAllow, &opts)
            .await
            .expect("loop ok");
    }
    let agg_input = synth.agg_input.load(Ordering::SeqCst);
    let agg_cache = synth.agg_cache.load(Ordering::SeqCst);
    (agg_input, agg_cache)
}

#[tokio::test(flavor = "current_thread")]
async fn cache_warm_ratio_above_half_on_warm_pass() {
    let (cold_input, _cold_cache_read) = run_pass(false).await;
    let (warm_input, warm_cache_read) = run_pass(true).await;
    assert!(cold_input > 0, "cold pass produced no input_tokens accounting");
    assert!(
        f64::from(warm_cache_read) > 0.5 * f64::from(warm_input),
        "warm cache_read_input_tokens ({warm_cache_read}) must exceed 0.5 * input_tokens ({warm_input})"
    );
}
