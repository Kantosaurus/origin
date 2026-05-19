use origin_cas::{dict, Store, StoreConfig};
use std::sync::Arc;
use tempfile::tempdir;

fn store(dir: &std::path::Path) -> Arc<Store> {
    Arc::new(
        Store::open(StoreConfig {
            root: dir.to_path_buf(),
            hot_capacity: 4,
            warm_pack_target_bytes: 1_000,
            cold_zstd_level: 3,
        })
        .expect("open"),
    )
}

#[test]
fn train_rejects_insufficient_samples() {
    let samples: Vec<Vec<u8>> = (0..5).map(|i| format!("sample {i}").into_bytes()).collect();
    let err = dict::train(&samples).expect_err("should fail");
    assert!(matches!(err, dict::DictError::Insufficient { .. }));
}

#[test]
fn train_produces_nonempty_dict_from_repetitive_samples() {
    let samples: Vec<Vec<u8>> = (0..32)
        .map(|i| {
            format!("the quick brown fox jumps over the lazy dog. iter={i}\n")
                .repeat(20)
                .into_bytes()
        })
        .collect();
    let dict_bytes = dict::train(&samples).expect("train");
    assert!(!dict_bytes.is_empty());
    assert!(dict_bytes.len() <= dict::TARGET_DICT_BYTES);
}

#[test]
fn train_dict_from_sample_persists_and_returns_version() {
    let dir = tempdir().expect("tempdir");
    let s = store(dir.path());
    for i in 0..40 {
        let body = format!("the quick brown fox jumps over the lazy dog. seq={i}\n")
            .repeat(20)
            .into_bytes();
        let h = s.put(&body).expect("put");
        s.demote_to_cold(h).expect("demote");
    }
    let v = s.train_dict_from_sample(32).expect("train");
    assert_eq!(s.active_dict_version(), Some(v));
    assert!(dir.path().join(format!("dict-v{}.zstd", v.0)).exists());
}

#[test]
fn predict_shards_remain_readable_after_dict_training() {
    let dir = tempdir().expect("tempdir");
    let s = store(dir.path());
    let body = b"hello world".repeat(100);
    let h = s.put(&body).expect("put");
    s.demote_to_cold(h).expect("demote");
    for i in 0..40 {
        let _ = s
            .put(&format!("filler {i}").repeat(50).into_bytes())
            .expect("put");
    }
    let _v = s.train_dict_from_sample(32).expect("train");
    let got = s.get(h).expect("get").expect("Some");
    assert_eq!(got, body);
}
