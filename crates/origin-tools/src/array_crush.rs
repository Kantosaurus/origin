// SPDX-License-Identifier: Apache-2.0
//! First-occurrence reversible array compression (`SchemaCrush`).
//!
//! # Why
//!
//! The output-CAS dedup in [`crate::tool_envelope`] only saves tokens when a
//! tool result is *byte-identical* to a prior result in the session. The
//! `SmartCrusher` insight ports cleanly here. The
//! single largest token sink in an agent loop, however, is the **first**
//! emission of a large, homogeneous JSON array — `Grep`/`Glob` hit lists,
//! `graph_query` rows, `mem_search` results, MCP tool payloads, etc. Those
//! arrays repeat the same object keys on every element, so the schema is paid
//! for once per row.
//!
//! This module ports the core insight of headroom's `SmartCrusher`: an array
//! of like-shaped records is a *table*. State the schema once, render rows
//! compactly, and — only if still over budget — offload the redundant tail
//! behind a content-addressed handle the model can retrieve on demand. Origin
//! already ships the perfect reversibility substrate ([`crate::result_cas`] +
//! the `Recall` tool), so the lossy tier stays fully reversible.
//!
//! # Tiers (lossless first, then bounded-lossy)
//!
//! 1. **Columnar rewrite (lossless).** A JSON array whose elements are objects
//!    sharing a dominant key set is rewritten to
//!    `{"__schema_crush":1,"columns":[…],"rows":[[…],…]}`. Every value is
//!    preserved byte-for-byte after a round-trip; only the repeated keys and
//!    structural punctuation are dropped. Typically 40–70% smaller.
//! 2. **Tail offload (lossy, reversible).** If the columnar form still exceeds
//!    the budget, the first `head_rows` are kept inline and the remaining rows
//!    are replaced by a sentinel carrying `rows_offloaded` and a `recall`
//!    handle. The caller is expected to `put` the full original into the CAS
//!    under that handle so `Recall` can inflate it.
//!
//! The transform is conservative: it only fires for arrays at or above a size
//! threshold whose elements are *mostly* homogeneous objects of scalar-ish
//! values. Anything it does not understand passes through untouched, so it can
//! never make a result larger or lose data silently.

use serde_json::{json, Map, Value};

/// Default minimum serialised size before [`crush_result_bytes`] does any work.
/// Below this the array is small enough that crushing rarely pays for the
/// schema-table indirection. ~2 KiB ≈ 500 tokens.
pub const DEFAULT_MIN_BYTES: usize = 2_048;

/// Tuning knobs for [`crush_value`].
#[derive(Debug, Clone, Copy)]
pub struct CrushConfig {
    /// Minimum element count before an array is eligible for columnar rewrite.
    /// Below this the per-row key overhead is not worth the indirection.
    pub min_rows: usize,
    /// Minimum fraction of elements that must share the dominant key set for
    /// the array to be treated as a homogeneous table. In `[0.0, 1.0]`.
    pub min_homogeneity: f64,
    /// Approximate token budget for the whole serialised result. When the
    /// lossless columnar form still exceeds this, the lossy tail-offload tier
    /// engages. `0` disables the lossy tier (lossless only).
    pub budget_tokens: usize,
    /// Number of rows to keep inline when the lossy tier engages.
    pub head_rows: usize,
}

impl Default for CrushConfig {
    fn default() -> Self {
        Self {
            min_rows: 8,
            min_homogeneity: 0.75,
            budget_tokens: 6_000,
            head_rows: 12,
        }
    }
}

/// Outcome of a crush attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrushOutcome {
    /// Nothing matched; the original value should be used unchanged.
    Unchanged,
    /// Lossless columnar rewrite. Fully reversible by [`expand_value`].
    Lossless,
    /// Lossy tail offload. `rows_offloaded` rows were replaced by a sentinel;
    /// the caller must store the original bytes in the CAS keyed by the handle
    /// it later substitutes into the sentinel via [`set_offload_handle`].
    Lossy { rows_offloaded: usize },
}

/// Estimate token count the same way [`crate::budget_writer::approx_tokens`]
/// does, but over an already-serialised [`Value`] without re-borrowing it.
fn approx_tokens_of(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.chars().count() / 4)
}

/// True if `v` is a "scalar-ish" leaf that columnarises cleanly. Nested
/// arrays/objects are allowed too (they round-trip), but we use this to gauge
/// homogeneity quality, not to reject rows.
const fn is_scalarish(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

/// Pick the dominant ordered key set across `rows`. Returns `None` when fewer
/// than `min_homogeneity` of the rows are objects sharing the modal key set.
fn dominant_columns(rows: &[Value], min_homogeneity: f64) -> Option<Vec<String>> {
    // Count objects keyed by their sorted-key signature; remember the first
    // insertion order so the emitted columns are stable and human-readable.
    use std::collections::HashMap;
    let mut sig_count: HashMap<Vec<String>, usize> = HashMap::new();
    let mut sig_order: HashMap<Vec<String>, Vec<String>> = HashMap::new();
    let mut object_rows = 0_usize;

    for row in rows {
        let Value::Object(map) = row else { continue };
        object_rows += 1;
        let mut sig: Vec<String> = map.keys().cloned().collect();
        sig.sort();
        let order: Vec<String> = map.keys().cloned().collect();
        *sig_count.entry(sig.clone()).or_insert(0) += 1;
        sig_order.entry(sig).or_insert(order);
    }

    if object_rows == 0 {
        return None;
    }

    let (best_sig, best_n) = sig_count.into_iter().max_by_key(|(_, n)| *n)?;
    // Homogeneity is measured against ALL rows (not just objects): a column of
    // strings interleaved with the occasional object should not crush.
    #[allow(clippy::cast_precision_loss)] // row counts are far below 2^52
    let threshold = (rows.len() as f64) * min_homogeneity;
    #[allow(clippy::cast_precision_loss)]
    if (best_n as f64) < threshold {
        return None;
    }
    sig_order.remove(&best_sig)
}

/// Attempt to crush `value` in place. Returns the outcome.
///
/// On [`CrushOutcome::Lossless`]/[`CrushOutcome::Lossy`] the `value` has been
/// rewritten. The lossy sentinel's `recall` field is left as a placeholder
/// (`""`); the caller fills it with [`set_offload_handle`] after storing the
/// original bytes in the CAS.
///
/// We descend into the top-level object's fields by one level too: most tool
/// results are `{"matches": [...]}` or `{"results": [...]}` rather than a bare
/// array, and crushing the inner array is where the savings live.
pub fn crush_value(value: &mut Value, cfg: &CrushConfig) -> CrushOutcome {
    // Case 1: the value is itself the array.
    if let Value::Array(_) = value {
        return crush_array_in_place(value, cfg);
    }

    // Case 2: a single-array-bearing wrapper object. Find the largest array
    // field and crush it; leave everything else untouched.
    if let Value::Object(map) = value {
        let mut best_key: Option<String> = None;
        let mut best_len = 0_usize;
        for (k, v) in map.iter() {
            if let Value::Array(arr) = v {
                if arr.len() > best_len {
                    best_len = arr.len();
                    best_key = Some(k.clone());
                }
            }
        }
        if let Some(k) = best_key {
            if let Some(field) = map.get_mut(&k) {
                return crush_array_in_place(field, cfg);
            }
        }
    }

    CrushOutcome::Unchanged
}

/// Crush an array `Value` in place (must be `Value::Array`).
fn crush_array_in_place(value: &mut Value, cfg: &CrushConfig) -> CrushOutcome {
    let Value::Array(rows) = value else {
        return CrushOutcome::Unchanged;
    };
    if rows.len() < cfg.min_rows {
        return CrushOutcome::Unchanged;
    }
    let Some(columns) = dominant_columns(rows, cfg.min_homogeneity) else {
        return CrushOutcome::Unchanged;
    };

    // Build the columnar table. Rows that match the dominant schema become a
    // positional array of values; rows that DON'T (the homogeneity slack) are
    // preserved verbatim under a side channel so nothing is lost.
    let mut table_rows: Vec<Value> = Vec::with_capacity(rows.len());
    let mut exceptions: Map<String, Value> = Map::new();
    let mut scalar_cells = 0_usize;
    let mut total_cells = 0_usize;

    for (i, row) in rows.iter().enumerate() {
        match row {
            Value::Object(map) if columns.iter().all(|c| map.contains_key(c)) && map.len() == columns.len() => {
                let cells: Vec<Value> = columns
                    .iter()
                    .map(|c| {
                        let cell = map.get(c).cloned().unwrap_or(Value::Null);
                        total_cells += 1;
                        if is_scalarish(&cell) {
                            scalar_cells += 1;
                        }
                        cell
                    })
                    .collect();
                table_rows.push(Value::Array(cells));
            }
            other => {
                // Off-schema row: keep it verbatim, indexed by position so
                // expansion can splice it back exactly.
                exceptions.insert(i.to_string(), other.clone());
                table_rows.push(Value::Null);
            }
        }
    }

    // Guardrail: if the table is dominated by nested non-scalar cells the
    // columnar form may not actually be smaller. Only commit when it wins.
    let original_tokens = approx_tokens_of(value);

    let mut crushed = Map::new();
    crushed.insert("__schema_crush".into(), json!(1));
    crushed.insert("columns".into(), json!(columns));
    crushed.insert("rows".into(), Value::Array(table_rows));
    if !exceptions.is_empty() {
        crushed.insert("exceptions".into(), Value::Object(exceptions));
    }
    let crushed_value = Value::Object(crushed);
    let crushed_tokens = approx_tokens_of(&crushed_value);

    // Require a real win (≥10% smaller) or bail — never risk a regression.
    if crushed_tokens * 10 >= original_tokens * 9 || total_cells == 0 {
        return CrushOutcome::Unchanged;
    }
    let _ = scalar_cells; // reserved for future ratio-based heuristics

    *value = crushed_value;

    // Lossless tier is enough if we're under budget (or the lossy tier is off).
    if cfg.budget_tokens == 0 || crushed_tokens <= cfg.budget_tokens {
        return CrushOutcome::Lossless;
    }

    // Lossy tier: keep `head_rows` inline, offload the rest behind a sentinel.
    lossy_offload(value, cfg)
}

/// Replace the tail of an already-columnarised `value`'s rows with a sentinel.
fn lossy_offload(value: &mut Value, cfg: &CrushConfig) -> CrushOutcome {
    let Value::Object(map) = value else {
        return CrushOutcome::Lossless;
    };
    let Some(Value::Array(rows)) = map.get_mut("rows") else {
        return CrushOutcome::Lossless;
    };
    if rows.len() <= cfg.head_rows {
        return CrushOutcome::Lossless;
    }
    let offloaded = rows.len() - cfg.head_rows;
    rows.truncate(cfg.head_rows);
    map.insert(
        "__offloaded".into(),
        json!({
            // Filled in by `set_offload_handle` once the original is stored.
            "recall": "",
            "rows_offloaded": offloaded,
            "hint": "call Recall with this handle to retrieve the full, uncompressed result",
        }),
    );
    CrushOutcome::Lossy {
        rows_offloaded: offloaded,
    }
}

/// Stamp the CAS handle into the lossy sentinel produced by [`crush_value`].
/// No-op if `value` is not a lossy-crushed object.
pub fn set_offload_handle(value: &mut Value, handle_hex: &str) {
    if let Value::Object(map) = value {
        if let Some(Value::Object(off)) = map.get_mut("__offloaded") {
            off.insert("recall".into(), json!(handle_hex));
        }
    }
}

/// Reverse a [`CrushOutcome::Lossless`] rewrite, reconstructing the original
/// array of objects. Returns `None` if `value` is not a schema-crush object.
/// (The lossy tier is reversed by `Recall`, not here.)
#[must_use]
pub fn expand_value(value: &Value) -> Option<Value> {
    let Value::Object(map) = value else {
        return None;
    };
    if map.get("__schema_crush").and_then(Value::as_i64) != Some(1) {
        return None;
    }
    let columns: Vec<String> = map
        .get("columns")?
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str().map(str::to_owned))
        .collect();
    let rows = map.get("rows")?.as_array()?;
    let exceptions = map.get("exceptions").and_then(Value::as_object);

    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        if let Some(exc) = exceptions.and_then(|e| e.get(&i.to_string())) {
            out.push(exc.clone());
            continue;
        }
        let cells = row.as_array()?;
        let mut obj = Map::new();
        for (col, cell) in columns.iter().zip(cells.iter()) {
            obj.insert(col.clone(), cell.clone());
        }
        out.push(Value::Object(obj));
    }
    Some(Value::Array(out))
}

/// Daemon-facing convenience: crush a serialised tool result's `bytes`.
///
/// Returns the (possibly smaller) bytes to send to the model.
///
/// `original_handle_hex` is the CAS hash (lowercase hex, no prefix) under
/// which the **full, uncrushed** `bytes` are already stored — origin CAS-puts
/// every tool result, so the original is always retrievable. When the lossy
/// tier engages, the offload sentinel's `recall` field is stamped with this
/// handle so the model can call `Recall` to inflate the dropped rows.
///
/// Returns `None` (the caller should keep `bytes` verbatim) when the body is
/// below `min_bytes`, is not JSON, or no homogeneous array large enough to win
/// was found. On `Some((crushed, outcome))`, `crushed` is guaranteed smaller.
#[must_use]
pub fn crush_result_bytes(
    bytes: &[u8],
    original_handle_hex: &str,
    min_bytes: usize,
    cfg: &CrushConfig,
) -> Option<(Vec<u8>, CrushOutcome)> {
    if bytes.len() < min_bytes {
        return None;
    }
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    let outcome = crush_value(&mut value, cfg);
    match outcome {
        CrushOutcome::Unchanged => None,
        CrushOutcome::Lossless => {
            let out = serde_json::to_vec(&value).ok()?;
            if out.len() >= bytes.len() {
                return None; // never emit a larger body than we received
            }
            Some((out, outcome))
        }
        CrushOutcome::Lossy { rows_offloaded } => {
            stamp_into(&mut value, original_handle_hex);
            let out = serde_json::to_vec(&value).ok()?;
            if out.len() >= bytes.len() {
                return None;
            }
            Some((out, CrushOutcome::Lossy { rows_offloaded }))
        }
    }
}

/// Stamp the handle into either a bare crushed object or a wrapper object whose
/// crushed array sits one level down (mirrors [`crush_value`]'s descent).
fn stamp_into(value: &mut Value, handle_hex: &str) {
    set_offload_handle(value, handle_hex);
    if let Value::Object(map) = value {
        for v in map.values_mut() {
            set_offload_handle(v, handle_hex);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn sample_rows(n: usize) -> Value {
        let arr: Vec<Value> = (0..n)
            .map(|i| {
                json!({
                    "path": format!("src/file_{i}.rs"),
                    "line": i * 7 + 1,
                    "match": format!("fn handler_{i}() -> Result<()>"),
                })
            })
            .collect();
        Value::Array(arr)
    }

    #[test]
    fn lossless_roundtrip_preserves_every_value() {
        let original = sample_rows(40);
        let mut v = original.clone();
        let cfg = CrushConfig {
            budget_tokens: 0, // lossless only
            ..CrushConfig::default()
        };
        let outcome = crush_value(&mut v, &cfg);
        assert_eq!(outcome, CrushOutcome::Lossless);
        // Smaller than the original.
        assert!(approx_tokens_of(&v) < approx_tokens_of(&original));
        // And exactly reversible.
        let restored = expand_value(&v).expect("crushed object expands");
        assert_eq!(restored, original);
    }

    #[test]
    fn wrapper_object_inner_array_is_crushed() {
        let mut v = json!({ "matches": sample_rows(30), "count": 30 });
        let cfg = CrushConfig::default();
        let outcome = crush_value(&mut v, &cfg);
        assert!(matches!(
            outcome,
            CrushOutcome::Lossless | CrushOutcome::Lossy { .. }
        ));
        // Sibling scalar field untouched.
        assert_eq!(v["count"], json!(30));
        // Inner array became a schema-crush table.
        assert_eq!(v["matches"]["__schema_crush"], json!(1));
    }

    #[test]
    fn small_arrays_pass_through() {
        let mut v = sample_rows(3);
        let before = v.clone();
        let outcome = crush_value(&mut v, &CrushConfig::default());
        assert_eq!(outcome, CrushOutcome::Unchanged);
        assert_eq!(v, before);
    }

    #[test]
    fn heterogeneous_arrays_pass_through() {
        let mut arr = vec![json!({"a": 1}), json!("string"), json!(42)];
        for i in 0..20 {
            arr.push(json!(i));
        }
        let mut v = Value::Array(arr);
        let before = v.clone();
        let outcome = crush_value(&mut v, &CrushConfig::default());
        assert_eq!(outcome, CrushOutcome::Unchanged);
        assert_eq!(v, before);
    }

    #[test]
    fn off_schema_rows_preserved_via_exceptions() {
        let mut arr: Vec<Value> = (0..30)
            .map(|i| json!({"path": format!("f{i}"), "line": i, "match": "x"}))
            .collect();
        // One row with an extra key — must survive verbatim.
        arr[5] = json!({"path": "weird", "line": 5, "match": "x", "extra": [1, 2, 3]});
        let original = Value::Array(arr);
        let mut v = original.clone();
        let cfg = CrushConfig {
            budget_tokens: 0,
            ..CrushConfig::default()
        };
        crush_value(&mut v, &cfg);
        let restored = expand_value(&v).expect("expands");
        assert_eq!(restored, original);
    }

    #[test]
    fn lossy_tier_offloads_tail_and_sets_handle() {
        let mut v = sample_rows(200);
        let cfg = CrushConfig {
            budget_tokens: 200, // force lossy
            head_rows: 10,
            ..CrushConfig::default()
        };
        let outcome = crush_value(&mut v, &cfg);
        let CrushOutcome::Lossy { rows_offloaded } = outcome else {
            panic!("expected lossy outcome, got {outcome:?}");
        };
        assert_eq!(rows_offloaded, 190);
        assert_eq!(v["rows"].as_array().unwrap().len(), 10);
        assert_eq!(v["__offloaded"]["recall"], json!(""));
        set_offload_handle(&mut v, "blake3:deadbeef");
        assert_eq!(v["__offloaded"]["recall"], json!("blake3:deadbeef"));
    }

    #[test]
    fn crush_result_bytes_lossless_is_smaller_and_reversible() {
        let original = sample_rows(60);
        let bytes = serde_json::to_vec(&original).unwrap();
        let cfg = CrushConfig {
            budget_tokens: 0,
            ..CrushConfig::default()
        };
        let (crushed, outcome) =
            crush_result_bytes(&bytes, "abc123", 0, &cfg).expect("should crush");
        assert_eq!(outcome, CrushOutcome::Lossless);
        assert!(crushed.len() < bytes.len());
        let v: Value = serde_json::from_slice(&crushed).unwrap();
        assert_eq!(expand_value(&v).unwrap(), original);
    }

    #[test]
    fn crush_result_bytes_lossy_stamps_handle() {
        let bytes = serde_json::to_vec(&sample_rows(300)).unwrap();
        let cfg = CrushConfig {
            budget_tokens: 200,
            head_rows: 8,
            ..CrushConfig::default()
        };
        let (crushed, outcome) =
            crush_result_bytes(&bytes, "feedface", 0, &cfg).expect("should crush");
        assert!(matches!(outcome, CrushOutcome::Lossy { .. }));
        let v: Value = serde_json::from_slice(&crushed).unwrap();
        assert_eq!(v["__offloaded"]["recall"], json!("feedface"));
        assert!(crushed.len() < bytes.len());
    }

    #[test]
    fn crush_result_bytes_passes_through_non_json() {
        assert!(crush_result_bytes(b"not json at all, just text", "h", 0, &CrushConfig::default()).is_none());
    }

    #[test]
    fn crush_result_bytes_respects_min_bytes() {
        let bytes = serde_json::to_vec(&sample_rows(60)).unwrap();
        // Threshold above the body size ⇒ skip.
        assert!(crush_result_bytes(&bytes, "h", bytes.len() + 1, &CrushConfig::default()).is_none());
    }
}
