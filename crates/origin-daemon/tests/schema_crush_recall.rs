// SPDX-License-Identifier: Apache-2.0
//! `SchemaCrush` ↔ CAS ↔ Recall contract (headroom `SmartCrusher` port).
//!
//! Proves the reversibility guarantee the daemon wiring relies on: when a
//! large homogeneous JSON array tool result is crushed, the lossy offload
//! sentinel's `recall` handle resolves — via the real `origin_cas::Store` and
//! the real `Recall` tool — back to the byte-identical original. This is the
//! same path `agent.rs` takes when `ORIGIN_SCHEMA_CRUSH=1`.
#![allow(clippy::unwrap_used, clippy::panic)]

use origin_cas::{Store, StoreConfig};
use origin_tools::array_crush::{crush_result_bytes, CrushConfig, CrushOutcome};
use origin_tools::builtins::recall::recall_tool;
use serde_json::{json, Value};
use tempfile::tempdir;

fn open_cas() -> (Store, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let store = Store::open(StoreConfig {
        root: dir.path().to_path_buf(),
        hot_capacity: 64,
        warm_pack_target_bytes: 64 * 1024,
        cold_zstd_level: 3,
    })
    .expect("open cas");
    // Return the TempDir guard so its directory survives for the test's life.
    (store, dir)
}

/// Build a realistic `Grep`-style result: `{ "matches": [ {path,line,match}, … ] }`.
fn grep_like(n: usize) -> Value {
    let matches: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "path": format!("crates/origin-daemon/src/file_{i}.rs"),
                "line": i * 13 + 7,
                "match": format!("    let handler_{i} = compute_thing(input_{i}, &ctx);"),
            })
        })
        .collect();
    json!({ "matches": matches, "count": n })
}

#[tokio::test]
async fn lossy_offload_handle_recalls_the_original() {
    let (cas, _guard) = open_cas();

    let original = grep_like(400);
    let original_bytes = serde_json::to_vec(&original).unwrap();

    // Mirror agent.rs: CAS-put the FULL original first to anchor retrieval.
    let h = cas.put(&original_bytes).unwrap();
    let hex = hex::encode(h.as_bytes());

    // Force the lossy tier with a tight budget.
    let cfg = CrushConfig {
        budget_tokens: 300,
        head_rows: 10,
        ..CrushConfig::default()
    };
    let (crushed, outcome) =
        crush_result_bytes(&original_bytes, &hex, 0, &cfg).expect("a 400-row homogeneous array should crush");

    // It's the lossy tier, and it's strictly smaller.
    assert!(matches!(outcome, CrushOutcome::Lossy { .. }), "got {outcome:?}");
    assert!(
        crushed.len() < original_bytes.len(),
        "crushed {} should be < original {}",
        crushed.len(),
        original_bytes.len()
    );

    // The model-visible body carries a recall handle pointing at the original.
    let crushed_val: Value = serde_json::from_slice(&crushed).unwrap();
    let recall_hex = crushed_val["matches"]["__offloaded"]["recall"]
        .as_str()
        .expect("offload sentinel carries a recall handle");
    assert_eq!(recall_hex, hex);

    // Recall that handle → byte-identical original.
    let mut handle = [0u8; 32];
    hex::decode_to_slice(recall_hex, &mut handle).unwrap();
    let recalled = recall_tool(&cas, handle, None).expect("recall resolves");
    let recalled_val: Value = serde_json::from_str(&recalled).unwrap();
    assert_eq!(
        recalled_val, original,
        "Recall must reconstruct the exact original"
    );
}

/// Plays one `Block` per turn, then "done".
struct Script(std::sync::Mutex<Vec<origin_core::types::Block>>);

#[async_trait::async_trait]
impl origin_provider::Provider for Script {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(
        &self,
        _req: origin_provider::ChatRequest,
    ) -> Result<origin_provider::ChatResponse, origin_provider::ProviderError> {
        use origin_core::types::{Block, Message, Role};
        let mut q = self.0.lock().unwrap();
        let b = if q.is_empty() {
            Block::text("done")
        } else {
            q.remove(0)
        };
        Ok(origin_provider::ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(b),
            usage: origin_provider::Usage::default(),
        })
    }
}

/// The body the loop records for one `Recall` of `stored` (put in a fresh CAS).
async fn recalled_through_the_loop(stored: &[u8]) -> String {
    use origin_core::types::Block;
    use origin_daemon::agent::{run_loop, LoopOptions};
    use std::sync::{Arc, Mutex};

    let (cas, _guard) = open_cas();
    let cas = Arc::new(cas);
    let hex = hex::encode(cas.put(stored).unwrap().as_bytes());
    let provider = Script(Mutex::new(vec![Block::ToolUse {
        id: "r".into(),
        name: "Recall".into(),
        input_json: serde_json::to_vec(&json!({ "handle": hex })).unwrap(),
        cache_marker: None,
    }]));
    let opts = LoopOptions::default()
        .with_cas(Arc::clone(&cas))
        .without_streaming();
    let mut session = origin_daemon::session::Session::new("scripted", "m");
    run_loop(
        &mut session,
        "go",
        &provider,
        &origin_permission::prompt::AlwaysAllow,
        &opts,
    )
    .await
    .unwrap();
    session
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult { handle: Some(h), .. } => {
                Some(String::from_utf8(cas.get(origin_cas::Hash::from_bytes(*h)).unwrap().unwrap()).unwrap())
            }
            _ => None,
        })
        .unwrap()
}

/// A1: `Recall` of an offloaded array returns its rows — the loop does not
/// crush the recalled body again (which would offload the same rows behind
/// the same handle, so they could never be seen).
#[tokio::test]
async fn recall_of_an_offloaded_array_is_not_crushed_again() {
    let body = recalled_through_the_loop(&serde_json::to_vec(&grep_like(600)).unwrap()).await;
    assert!(
        body.contains("file_599.rs") && !body.contains("__offloaded"),
        "recalled rows were crushed again: {}",
        &body[..body.len().min(400)]
    );
}

/// A7: one tool result is capped at 200,000 bytes plus a truncation note
/// where results are captured; a 150 KB result is untouched.
#[tokio::test]
async fn a_single_tool_result_is_capped_at_200_kb() {
    let big = "x".repeat(1_000_000);
    let body = recalled_through_the_loop(big.as_bytes()).await;
    assert!(
        body.len() <= 200_100 && body.ends_with("[truncated: 200000 of 1000000 bytes shown]"),
        "len {} ends {:?}",
        body.len(),
        &body[body.len().saturating_sub(60)..]
    );
    let mid = "y".repeat(150_000);
    assert_eq!(recalled_through_the_loop(mid.as_bytes()).await, mid);
}

#[tokio::test]
async fn lossless_tier_is_smaller_and_below_budget_keeps_all_rows() {
    let (cas, _guard) = open_cas();
    let original = grep_like(60);
    let original_bytes = serde_json::to_vec(&original).unwrap();
    let h = cas.put(&original_bytes).unwrap();
    let hex = hex::encode(h.as_bytes());

    // Generous budget ⇒ lossless columnar rewrite, no rows dropped.
    let cfg = CrushConfig {
        budget_tokens: 1_000_000,
        ..CrushConfig::default()
    };
    let (crushed, outcome) = crush_result_bytes(&original_bytes, &hex, 0, &cfg).expect("crush");
    assert_eq!(outcome, CrushOutcome::Lossless);
    assert!(crushed.len() < original_bytes.len());

    let v: Value = serde_json::from_slice(&crushed).unwrap();
    // All 60 rows are still present in columnar form (nothing offloaded).
    assert_eq!(v["matches"]["__schema_crush"], json!(1));
    assert_eq!(v["matches"]["rows"].as_array().unwrap().len(), 60);
    assert!(v["matches"].get("__offloaded").is_none());
}
