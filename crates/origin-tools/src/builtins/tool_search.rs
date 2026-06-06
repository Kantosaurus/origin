// SPDX-License-Identifier: Apache-2.0
//! `ToolSearch` — fetch full schemas for deferred tools on demand.

use crate::error::ToolError;
use crate::registry_iter;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ToolSearchArgs {
    pub query: String,
    pub max_results: Option<u32>,
}

/// # Errors
/// None today; signature kept for future validation.
pub fn tool_search(args: &ToolSearchArgs) -> Result<Value, ToolError> {
    let max = args.max_results.unwrap_or(5) as usize;
    if let Some(rest) = args.query.strip_prefix("select:") {
        let names: Vec<&str> = rest.split(',').map(str::trim).collect();
        let arr: Vec<Value> = registry_iter()
            .filter(|m| !m.hot && names.contains(&m.name))
            .map(meta_to_json)
            .collect();
        return Ok(Value::Array(arr));
    }
    // Keyword search: rank by hit count in name + description.
    let terms: Vec<&str> = args.query.split_whitespace().collect();
    let mut scored: Vec<(i32, Value)> = registry_iter()
        .filter(|m| !m.hot)
        .map(|m| {
            let blob = format!("{} {}", m.name.to_lowercase(), m.description.to_lowercase());
            let score: i32 = terms
                .iter()
                .map(|t| i32::from(blob.contains(&t.to_lowercase())))
                .sum();
            (score, meta_to_json(m))
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by_key(|e| std::cmp::Reverse(e.0));
    let arr: Vec<Value> = scored.into_iter().take(max).map(|(_, v)| v).collect();
    Ok(Value::Array(arr))
}

fn meta_to_json(m: &crate::ToolMeta) -> Value {
    json!({
        "name": m.name,
        "description": m.description,
        "input_schema": serde_json::from_str::<Value>(m.input_schema).unwrap_or(Value::Null),
    })
}

crate::origin_tool! {
    name: "ToolSearch",
    description: "Fetch full schemas for deferred tools. `select:Name,Name` returns exact tools; keyword query ranks by relevance.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "query":       { "type": "string" },
            "max_results": { "type": "integer", "minimum": 1, "maximum": 50, "default": 5 }
        },
        "required": ["query"]
    }"#,
}
