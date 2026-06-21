// SPDX-License-Identifier: Apache-2.0
//! Schema-aligned coercion of LLM-produced tool arguments.
//!
//! Weaker/fast models often emit tool args with the right intent but the wrong
//! JSON shape: a scalar where an array is expected, `"3"` instead of `3`, a
//! Markdown-fenced JSON blob, or a near-miss enum value. This module reshapes
//! those common mismatches toward the declared schema *before* deserialization,
//! so a recoverable shape error does not waste a turn. It is deterministic,
//! table-driven, and fails open.

use serde_json::Value;

/// Best-effort coercion of `v` toward the shape declared by `schema_json`.
///
/// `schema_json` is a raw JSON-Schema string. Never errors — returns `v`
/// unchanged when it can't confidently improve it; the caller still runs real
/// validation/deserialization afterward. Idempotent: an already-correct value
/// is returned unchanged.
#[must_use]
pub fn coerce(schema_json: &str, v: &Value) -> Value {
    // (1) The whole args object may be double-encoded as a JSON string,
    // optionally fenced. Unwrap it before consulting the schema.
    let v = unwrap_double_encoded(v);

    let Ok(schema) = serde_json::from_str::<Value>(schema_json) else {
        return v;
    };
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return v;
    };
    let Some(obj) = v.as_object() else {
        return v;
    };

    let mut out = obj.clone();
    for (key, prop_schema) in props {
        if let Some(field) = out.get(key) {
            if let Some(fixed) = coerce_field(prop_schema, field) {
                out.insert(key.clone(), fixed);
            }
        }
    }
    Value::Object(out)
}

/// If `v` is a string whose content is itself a JSON object/array (possibly
/// wrapped in a Markdown code fence or carrying trailing commas), parse and
/// return the inner value. Otherwise return `v` cloned.
fn unwrap_double_encoded(v: &Value) -> Value {
    let Some(s) = v.as_str() else {
        return v.clone();
    };
    let inner = strip_fence(s).trim();
    if !(inner.starts_with('{') || inner.starts_with('[')) {
        return v.clone();
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(inner) {
        return parsed;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(&strip_trailing_commas(inner)) {
        return parsed;
    }
    v.clone()
}

/// Strip a leading Markdown code fence (with optional `json` tag) and its
/// matching trailing fence from `s`.
fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // Drop an optional language tag on the first line (e.g. ```json).
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    let rest = rest.trim_start_matches(['\r', '\n', ' ']);
    rest.strip_suffix("```").map_or(rest, str::trim)
}

/// Remove commas that sit immediately before a closing `}` or `]` (ignoring
/// whitespace). Good enough for the trailing-comma typo; not a full parser.
fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            // Look ahead past whitespace for a closer.
            let next = chars[i + 1..]
                .iter()
                .find(|&&n| !n.is_whitespace())
                .copied();
            if matches!(next, Some('}' | ']')) {
                continue; // drop this comma
            }
        }
        out.push(c);
    }
    out
}

/// Coerce a single field toward `prop_schema`. Returns `Some(new_value)` only
/// when it confidently improved the value; `None` means "leave as-is".
fn coerce_field(prop_schema: &Value, field: &Value) -> Option<Value> {
    // enum snap takes precedence — it targets the value, not the type.
    if let Some(variants) = prop_schema.get("enum").and_then(Value::as_array) {
        if let Some(snapped) = snap_enum(variants, field) {
            return Some(snapped);
        }
    }

    match schema_type(prop_schema)? {
        "array" if !field.is_array() => Some(Value::Array(vec![field.clone()])),
        "integer" | "number" => coerce_number(field),
        "boolean" => coerce_bool(field),
        "string" => coerce_string(field),
        _ => None,
    }
}

/// Extract the schema `type` keyword as a `&str`, when it is a plain string.
fn schema_type(prop_schema: &Value) -> Option<&str> {
    prop_schema.get("type").and_then(Value::as_str)
}

/// Snap `field` to an exact enum variant on a case-insensitive / trimmed match.
fn snap_enum(variants: &[Value], field: &Value) -> Option<Value> {
    let s = field.as_str()?;
    if variants.iter().any(|var| var == field) {
        return None; // already exact
    }
    let needle = s.trim().to_lowercase();
    variants
        .iter()
        .find(|var| var.as_str().is_some_and(|vs| vs.trim().to_lowercase() == needle))
        .cloned()
}

/// `"3"`/`"3.5"` → a JSON number; already-numeric or non-numeric → `None`.
fn coerce_number(field: &Value) -> Option<Value> {
    let s = field.as_str()?.trim();
    serde_json::from_str::<Value>(s).ok().filter(Value::is_number)
}

/// `"true"`/`"false"` (any case) → a JSON bool; otherwise `None`.
fn coerce_bool(field: &Value) -> Option<Value> {
    match field.as_str()?.trim().to_lowercase().as_str() {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => None,
    }
}

/// A number/bool where a string is wanted → its stringified form.
fn coerce_string(field: &Value) -> Option<Value> {
    match field {
        Value::Number(n) => Some(Value::String(n.to_string())),
        Value::Bool(b) => Some(Value::String(b.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::coerce;
    use serde_json::json;

    #[test]
    fn scalar_wrapped_into_array() {
        let schema = r#"{"properties":{"paths":{"type":"array"}}}"#;
        let v = json!({"paths": "src/main.rs"});
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"paths": ["src/main.rs"]}));
    }

    #[test]
    fn numeric_string_to_integer() {
        let schema = r#"{"properties":{"count":{"type":"integer"}}}"#;
        let v = json!({"count": "3"});
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"count": 3}));
    }

    #[test]
    fn numeric_string_to_number_float() {
        let schema = r#"{"properties":{"ratio":{"type":"number"}}}"#;
        let v = json!({"ratio": "3.5"});
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"ratio": 3.5}));
    }

    #[test]
    fn string_true_to_bool() {
        let schema = r#"{"properties":{"force":{"type":"boolean"}}}"#;
        let v = json!({"force": "True"});
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"force": true}));
    }

    #[test]
    fn number_to_string() {
        let schema = r#"{"properties":{"id":{"type":"string"}}}"#;
        let v = json!({"id": 42});
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"id": "42"}));
    }

    #[test]
    fn fenced_json_string_to_object() {
        let schema = r#"{"properties":{"path":{"type":"string"}}}"#;
        let v = json!("```json\n{\"path\": \"a.txt\"}\n```");
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"path": "a.txt"}));
    }

    #[test]
    fn double_encoded_object_string_to_object() {
        let schema = r#"{"properties":{"path":{"type":"string"}}}"#;
        let v = json!("{\"path\": \"a.txt\"}");
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"path": "a.txt"}));
    }

    #[test]
    fn trailing_comma_string_parsed() {
        let schema = r#"{"properties":{"path":{"type":"string"}}}"#;
        let v = json!("{\"path\": \"a.txt\",}");
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"path": "a.txt"}));
    }

    #[test]
    fn enum_snap_case_insensitive() {
        let schema = r#"{"properties":{"mode":{"type":"string","enum":["Read","Write"]}}}"#;
        let v = json!({"mode": " write "});
        let got = coerce(schema, &v);
        assert_eq!(got, json!({"mode": "Write"}));
    }

    #[test]
    fn idempotent_already_correct() {
        let schema = r#"{"properties":{"paths":{"type":"array"},"count":{"type":"integer"},"mode":{"type":"string","enum":["Read","Write"]}}}"#;
        let v = json!({"paths": ["a"], "count": 3, "mode": "Read"});
        let once = coerce(schema, &v);
        assert_eq!(once, v);
        let twice = coerce(schema, &once);
        assert_eq!(twice, v);
    }

    #[test]
    fn no_matching_props_left_identical() {
        let schema = r#"{"properties":{"other":{"type":"array"}}}"#;
        let v = json!({"unrelated": "3", "another": true});
        let got = coerce(schema, &v);
        assert_eq!(got, v);
    }

    #[test]
    fn unparseable_schema_returns_input() {
        let v = json!({"x": "3"});
        let got = coerce("not json", &v);
        assert_eq!(got, v);
    }

    #[test]
    fn non_coercible_left_alone() {
        // string wanted, value already a string of nonsense → unchanged.
        let schema = r#"{"properties":{"count":{"type":"integer"}}}"#;
        let v = json!({"count": "notanumber"});
        let got = coerce(schema, &v);
        assert_eq!(got, v);
    }
}
