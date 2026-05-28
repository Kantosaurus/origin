//! Guard against pre-draft-2020-12 forms in any registered tool's
//! `input_schema`. The Anthropic API rejects tool schemas that don't match
//! draft 2020-12, and the most common offender is the legacy `items: [...]`
//! array (tuple validation), which in 2020-12 became `prefixItems`.
//!
//! We don't need a full validator here — we just need to refuse any subtree
//! where `items` is a JSON array instead of an object/bool.

use serde_json::Value;

fn walk_for_array_items(v: &Value, path: &str, failures: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(items) = map.get("items") {
                if items.is_array() {
                    failures.push(format!(
                        "{path}.items is an array (draft-04 tuple form); use prefixItems in 2020-12"
                    ));
                }
            }
            for (k, child) in map {
                walk_for_array_items(child, &format!("{path}.{k}"), failures);
            }
        }
        Value::Array(arr) => {
            for (i, child) in arr.iter().enumerate() {
                walk_for_array_items(child, &format!("{path}[{i}]"), failures);
            }
        }
        _ => {}
    }
}

#[test]
fn no_tool_schema_uses_legacy_tuple_items() {
    let mut all_failures: Vec<String> = Vec::new();
    for m in origin_tools::registry_iter() {
        let v: Value = serde_json::from_str(m.input_schema)
            .unwrap_or_else(|e| panic!("tool {} schema not valid JSON: {e}", m.name));
        let mut failures = Vec::new();
        walk_for_array_items(&v, &format!("tool[{}]", m.name), &mut failures);
        all_failures.extend(failures);
    }
    assert!(
        all_failures.is_empty(),
        "tool schemas violate draft 2020-12:\n  - {}",
        all_failures.join("\n  - ")
    );
}
