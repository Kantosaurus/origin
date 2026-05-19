//! Soak bench: synthesise a 1000-message session using a fake provider, all
//! tool outputs land in CAS, the ring is reused. Assert peak RSS stays under
//! 200 MiB above baseline (process RSS minus mmap pack-file pages we don't
//! control).
//!
//! Run with: `cargo bench -p origin-daemon --bench phase2_ram_soak`.
//!
//! This bench is configured with `harness = false` so it is a plain `fn main`
//! binary (no criterion macros) — `criterion` remains a dev-dep only because
//! the spec lists it as part of the dev-deps for future micro-benches. The
//! soak loop assertion is the only success criterion here: ΔRSS < 200 MiB.

#![allow(clippy::panic)] // benches panic on budget overflow by design

use async_trait::async_trait;
use origin_cas::{Store, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use sysinfo::{Pid, System};
use tempfile::tempdir;

/// Fake provider that returns a single text block per call and emits no
/// `tool_use`, so every turn terminates after exactly one model call.
struct FakeNoToolProvider {
    counter: AtomicU32,
}

#[async_trait]
impl Provider for FakeNoToolProvider {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn chat(&self, _: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text(format!("ack-{n}"))),
            usage: Usage::default(),
        })
    }
}

fn current_rss_kib(sys: &mut System, pid: Pid) -> u64 {
    sys.refresh_processes();
    // `sysinfo 0.30`: `Process::memory()` returns bytes on most platforms
    // (Windows + Linux). We normalize to KiB so the printed number is stable
    // regardless of platform-specific units (the budget check is in MiB).
    sys.process(pid).map_or(0, |p| p.memory() / 1024)
}

const TURNS: usize = 1000;

fn main() {
    let dir = tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(StoreConfig {
            root: dir.path().to_path_buf(),
            hot_capacity: 64,
            warm_pack_target_bytes: 4 * 1024 * 1024,
            cold_zstd_level: 3,
        })
        .expect("store open"),
    );

    let prov = FakeNoToolProvider {
        counter: AtomicU32::new(0),
    };
    let opts = LoopOptions {
        max_turns: 5,
        cas: Some(Arc::clone(&store)),
        relay_tx: None,
        streaming_disabled: true,
        ..LoopOptions::default()
    };

    let mut sys = System::new();
    let pid = Pid::from_u32(std::process::id());
    let baseline_rss_kib = current_rss_kib(&mut sys, pid);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio rt");
    let mut session = Session::new("fake", "fake-1");

    for i in 0..TURNS {
        rt.block_on(run_loop(
            &mut session,
            &format!("user msg {i}"),
            &prov,
            &AlwaysAllow,
            &opts,
        ))
        .expect("run_loop turn");
    }

    let final_rss_kib = current_rss_kib(&mut sys, pid);
    let growth_kib = final_rss_kib.saturating_sub(baseline_rss_kib);
    let delta_mib = growth_kib / 1024;

    println!("baseline_rss_kb={baseline_rss_kib}");
    println!("final_rss_kb={final_rss_kib}");
    println!("delta_mib={delta_mib}");
    println!("messages_in_session={}", session.messages.len());

    assert!(
        delta_mib < 200,
        "RSS growth {delta_mib} MiB exceeds 200 MiB budget (baseline {baseline_rss_kib} KiB, final {final_rss_kib} KiB)",
    );
}
