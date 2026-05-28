//! Memory tools — `mem_search`, `mem_save`, `mem_forget`.
//!
//! Each tool is registered via [`crate::origin_tool!`] and exposes a public
//! async `*_execute` function for unit-testing with any [`MemoryHandle`] impl.

use crate::dispatch::{MemoryHandle, MemoryToolError, SearchHit};
use crate::{SideEffects, Tier, Urgency};

// ── tool registrations ────────────────────────────────────────────────────────

crate::origin_tool! {
    name: "mem_search",
    description: "Semantic search over the user's cross-session memory. Returns top-k hits with previews.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "query":{"type":"string"},
            "k":{"type":"integer","minimum":1,"maximum":50,"default":5},
            "fresh":{"type":"boolean","default":false}
        },
        "required":["query"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

crate::origin_tool! {
    name: "mem_save",
    description: "Persist a memory across sessions. Tags optional.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "body":{"type":"string"},
            "tags":{"type":"array","items":{"type":"string"}}
        },
        "required":["body"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

crate::origin_tool! {
    name: "mem_forget",
    description: "Permanently delete a memory by id.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "id":{"type":"string"}
        },
        "required":["id"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Serialize a [`SearchHit`] to a compact JSON object.
fn hit_to_json(h: &SearchHit) -> serde_json::Value {
    serde_json::json!({
        "id":       h.id,
        "preview":  h.preview,
        "score":    h.score,
        "age_days": h.age_days,
        "tags":     h.tags,
    })
}

// ── execute functions ─────────────────────────────────────────────────────────

/// Execute a `mem_search` call.
///
/// Parses `input_json` for `query`, optional `k` (default 5), and optional
/// `fresh` (default `false`), calls [`MemoryHandle::search`], and returns a
/// compact JSON array of [`SearchHit`] objects.
///
/// # Errors
/// Returns [`MemoryToolError`] if JSON parsing fails or the handle errors.
#[allow(clippy::module_name_repetitions)] // `mem_search_execute` in module `mem` — name kept for API clarity
#[allow(clippy::unused_async)] // async for API stability: future impls may await background HNSW search
pub async fn mem_search_execute(mem: &dyn MemoryHandle, input_json: &str) -> Result<String, MemoryToolError> {
    let v: serde_json::Value = serde_json::from_str(input_json)
        .map_err(|e| MemoryToolError::Storage(format!("invalid input JSON: {e}")))?;

    let query = v["query"]
        .as_str()
        .ok_or_else(|| MemoryToolError::Storage("missing required field: query".into()))?;
    let k_raw = v["k"].as_u64().unwrap_or(5);
    let k = usize::try_from(k_raw).unwrap_or(usize::MAX);
    let fresh = v["fresh"].as_bool().unwrap_or(false);

    let hits = mem.search(query, k, fresh)?;

    let arr: serde_json::Value = serde_json::Value::Array(hits.iter().map(hit_to_json).collect());
    Ok(arr.to_string())
}

/// Execute a `mem_save` call.
///
/// Parses `input_json` for `body` and optional `tags`, calls
/// [`MemoryHandle::save`], and returns `{"id":"<ulid>"}`.
///
/// # Errors
/// Returns [`MemoryToolError`] if JSON parsing fails or the handle errors.
#[allow(clippy::module_name_repetitions)] // `mem_save_execute` in module `mem` — name kept for API clarity
#[allow(clippy::unused_async)] // async for API stability: future impls may await background HNSW search
pub async fn mem_save_execute(mem: &dyn MemoryHandle, input_json: &str) -> Result<String, MemoryToolError> {
    let v: serde_json::Value = serde_json::from_str(input_json)
        .map_err(|e| MemoryToolError::Storage(format!("invalid input JSON: {e}")))?;

    let body = v["body"]
        .as_str()
        .ok_or_else(|| MemoryToolError::Storage("missing required field: body".into()))?;

    let tags: Vec<String> = v["tags"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|t| t.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let id = mem.save(body, &tags)?;
    Ok(serde_json::json!({"id": id}).to_string())
}

/// Execute a `mem_forget` call.
///
/// Parses `input_json` for `id`, calls [`MemoryHandle::forget`], and returns
/// `{}` on success.
///
/// # Errors
/// Returns [`MemoryToolError`] if JSON parsing fails or the handle errors.
#[allow(clippy::module_name_repetitions)] // `mem_forget_execute` in module `mem` — name kept for API clarity
#[allow(clippy::unused_async)] // async for API stability: future impls may await background HNSW search
pub async fn mem_forget_execute(mem: &dyn MemoryHandle, input_json: &str) -> Result<String, MemoryToolError> {
    let v: serde_json::Value = serde_json::from_str(input_json)
        .map_err(|e| MemoryToolError::Storage(format!("invalid input JSON: {e}")))?;

    let id = v["id"]
        .as_str()
        .ok_or_else(|| MemoryToolError::Storage("missing required field: id".into()))?;

    mem.forget(id)?;
    Ok("{}".into())
}
