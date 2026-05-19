use async_trait::async_trait;
use origin_cas::{Hash, Store, StoreConfig};
use origin_core::types::{Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_sidecar::{ExtractDeliverer, Sidecar, SidecarConfig, SidecarJob, SummaryDeliverer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

#[derive(Debug, Default)]
struct StubProvider;
#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message {
                role: Role::Assistant,
                blocks: Vec::new(),
            },
            usage: Usage::default(),
        })
    }
}

#[derive(Debug)]
struct CountingSummary(Arc<AtomicU32>);
#[async_trait]
impl SummaryDeliverer for CountingSummary {
    async fn deliver(&self, _session: &str, _turn: u32, _summary: &str) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct CountingExtract(Arc<AtomicU32>);
#[async_trait]
impl ExtractDeliverer for CountingExtract {
    async fn deliver(&self, _src: Hash, _outline: Hash) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[allow(
    deprecated,
    reason = "tempfile 3.x: into_path() is deprecated in favour of keep(), which lands in 3.14+; acceptable in test helper"
)]
fn store() -> Arc<Store> {
    let dir = tempdir().expect("tempdir");
    Arc::new(
        Store::open(StoreConfig {
            root: dir.into_path(),
            hot_capacity: 16,
            warm_pack_target_bytes: 1_000_000,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn submit_summarize_drives_delivery() {
    let counter = Arc::new(AtomicU32::new(0));
    let sidecar = Sidecar::spawn(Arc::new(StubProvider), store(), SidecarConfig::default());
    sidecar
        .submit(SidecarJob::Summarize {
            session_id: "s1".into(),
            turn_index: 0,
            transcript: Vec::new(),
            deliver_to: Box::new(CountingSummary(counter.clone())),
        })
        .expect("submit");
    for _ in 0..50 {
        if counter.load(Ordering::Relaxed) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(counter.load(Ordering::Relaxed), 1, "deliverer should fire once");
    sidecar.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn submit_extract_drives_delivery() {
    let counter = Arc::new(AtomicU32::new(0));
    let cas = store();
    let body = b"the quick brown fox\n".repeat(1000); // > 16 KB
    let h = cas.put(&body).expect("put body");
    let sidecar = Sidecar::spawn(Arc::new(StubProvider), cas.clone(), SidecarConfig::default());
    sidecar
        .submit(SidecarJob::Extract {
            handle: h,
            deliver_to: Box::new(CountingExtract(counter.clone())),
        })
        .expect("submit");
    for _ in 0..50 {
        if counter.load(Ordering::Relaxed) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    sidecar.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn queue_full_returns_error() {
    let cfg = SidecarConfig {
        workers: 0,
        queue_capacity: 1,
        model: "stub".into(),
    };
    let sidecar = Sidecar::spawn(Arc::new(StubProvider), store(), cfg);
    let mk = || SidecarJob::Summarize {
        session_id: "s".into(),
        turn_index: 0,
        transcript: Vec::new(),
        deliver_to: Box::new(CountingSummary(Arc::new(AtomicU32::new(0)))),
    };
    sidecar.submit(mk()).expect("first submit (fills queue)");
    let err = sidecar.submit(mk()).expect_err("second submit should fail");
    assert!(matches!(err, origin_sidecar::SidecarError::QueueFull));
    sidecar.shutdown().await;
}
