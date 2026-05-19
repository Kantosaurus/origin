use origin_tools::builtins::mem::{mem_forget_execute, mem_save_execute, mem_search_execute};
use origin_tools::dispatch::{MemoryHandle, MemoryToolError, SearchHit};
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct MockMem {
    saved: Mutex<Vec<(String, Vec<String>)>>,
    forgotten: Mutex<Vec<String>>,
}

impl MemoryHandle for MockMem {
    fn search(&self, query: &str, _k: usize, _fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
        if query == "boom" {
            return Err(MemoryToolError::Unavailable);
        }
        Ok(vec![SearchHit {
            id: "01J0".into(),
            preview: format!("hit for {query}"),
            score: 0.9,
            age_days: 1.0,
            tags: vec!["t".into()],
        }])
    }
    fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError> {
        self.saved
            .lock()
            .expect("lock")
            .push((body.to_string(), tags.to_vec()));
        Ok("01J1".into())
    }
    fn forget(&self, id: &str) -> Result<(), MemoryToolError> {
        self.forgotten.lock().expect("lock").push(id.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn search_returns_hits() {
    let mem: Arc<dyn MemoryHandle> = Arc::new(MockMem::default());
    let json = mem_search_execute(&*mem, r#"{"query":"x","k":3}"#)
        .await
        .expect("ok");
    assert!(json.contains("\"id\":\"01J0\""));
    assert!(json.contains("\"preview\":\"hit for x\""));
}

#[tokio::test]
async fn save_persists_and_returns_id() {
    let mock = Arc::new(MockMem::default());
    let mem: Arc<dyn MemoryHandle> = Arc::clone(&mock) as _;
    let json = mem_save_execute(&*mem, r#"{"body":"hello","tags":["a","b"]}"#)
        .await
        .expect("ok");
    assert_eq!(json, r#"{"id":"01J1"}"#);
    let (body, tags) = {
        let saved = mock.saved.lock().expect("lock");
        (saved[0].0.clone(), saved[0].1.clone())
    };
    assert_eq!(body, "hello");
    assert_eq!(tags, vec!["a".to_string(), "b".to_string()]);
}

#[tokio::test]
async fn forget_returns_unit() {
    let mock = Arc::new(MockMem::default());
    let mem: Arc<dyn MemoryHandle> = Arc::clone(&mock) as _;
    let json = mem_forget_execute(&*mem, r#"{"id":"01J0"}"#).await.expect("ok");
    assert_eq!(json, "{}");
    assert_eq!(mock.forgotten.lock().expect("lock")[0], "01J0");
}
