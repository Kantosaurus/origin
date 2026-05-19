use async_trait::async_trait;
use origin_cas::{Hash, Store, StoreConfig};
use origin_sidecar::{extract, ExtractDeliverer};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::Mutex;

#[derive(Debug, Default)]
struct Capture(Mutex<Option<(Hash, Hash)>>);
#[async_trait]
impl ExtractDeliverer for Capture {
    async fn deliver(&self, src: Hash, outline: Hash) {
        *self.0.lock().await = Some((src, outline));
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
async fn extracts_outline_for_known_handle() {
    let cas = store();
    let body = b"line one\nline two\nline three\n".repeat(1000);
    let src = cas.put(&body).expect("put body");
    let capture = Arc::new(Capture::default());

    extract::run(&cas, src, capture.as_ref()).await;

    let (got_src, outline_handle) = (*capture.0.lock().await).expect("delivered");
    assert_eq!(got_src, src);
    let outline_bytes = cas.get(outline_handle).expect("get").expect("Some");
    let outline: extract::Outline = serde_json::from_slice(&outline_bytes).expect("decode");
    assert_eq!(outline.byte_count, body.len() as u64);
    assert!(outline.line_count > 0);
    assert!(outline.first_120_chars.starts_with("line one"));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_handle_is_silent_noop() {
    let cas = store();
    let capture = Arc::new(Capture::default());
    let nonexistent = Hash::of(b"never-stored");
    extract::run(&cas, nonexistent, capture.as_ref()).await;
    assert!(capture.0.lock().await.is_none(), "no delivery for unknown handle");
}
