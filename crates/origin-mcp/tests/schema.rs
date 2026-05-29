// SPDX-License-Identifier: Apache-2.0
//! P11.8 — JSON-Schema validation for MCP `tools/call` arguments.

use origin_mcp::schema::{SchemaCache, ValidationError};
use serde_json::json;

#[test]
fn accepts_valid_args() {
    let cache = SchemaCache::new();
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type":"string"}},
        "required": ["path"]
    });
    cache.register("read_file", &schema).expect("compile");
    let result = cache.validate("read_file", &json!({"path": "/tmp/x"}));
    assert!(result.is_ok(), "valid args rejected: {result:?}");
}

#[test]
fn rejects_missing_required() {
    let cache = SchemaCache::new();
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type":"string"}},
        "required": ["path"]
    });
    cache.register("read_file", &schema).expect("compile");
    let result = cache.validate("read_file", &json!({}));
    assert!(
        matches!(result, Err(ValidationError::Invalid(_))),
        "expected Invalid, got {result:?}"
    );
}

#[test]
fn rejects_wrong_type() {
    let cache = SchemaCache::new();
    let schema = json!({
        "type": "object",
        "properties": {"count": {"type":"integer"}}
    });
    cache.register("count_tool", &schema).expect("compile");
    let result = cache.validate("count_tool", &json!({"count": "not-a-number"}));
    assert!(
        matches!(result, Err(ValidationError::Invalid(_))),
        "expected Invalid, got {result:?}"
    );
}

#[test]
fn unknown_tool_passes_through() {
    let cache = SchemaCache::new();
    // No registered schema → treat as no-op. The daemon's tool-list refresh
    // populates the cache; unknown tools are an MCP-server bug, not ours.
    assert!(cache.validate("nope", &json!({})).is_ok());
}

#[test]
fn rejects_bad_schema_at_register() {
    let cache = SchemaCache::new();
    // `type` set to an unknown value should refuse to compile.
    let bad = json!({"type": "not-a-real-type"});
    let result = cache.register("oops", &bad);
    assert!(
        matches!(result, Err(ValidationError::Compile(_))),
        "expected Compile, got {result:?}"
    );
}
