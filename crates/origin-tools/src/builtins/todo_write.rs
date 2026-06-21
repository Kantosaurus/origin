// SPDX-License-Identifier: Apache-2.0
//! `TodoWrite` — a lightweight in-conversation task checklist.
//!
//! Ordinary turns otherwise have zero plan tracking (the `/goal` block pins the
//! objective, but not a step list). The model writes/overwrites the current todo
//! list and the daemon renders it back into the prompt each turn so multi-step
//! work stays on-plan. State + rendering live in the daemon; this is a
//! schema-only registration the dispatch intercepts, exactly like `Task`.

use crate::{SideEffects, Tier, Urgency};

crate::origin_tool! {
    name: "TodoWrite",
    description: "Record/overwrite your task checklist for a multi-step request. Pass the FULL list every call (it REPLACES the previous one). Keep exactly one item `in_progress` at a time and flip items to `completed` as you finish. Use it for non-trivial work so you don't lose track of steps; skip it for trivial one-step tasks.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type":"object",
        "properties":{
            "todos":{
                "type":"array",
                "items":{
                    "type":"object",
                    "properties":{
                        "content":{"type":"string"},
                        "status":{"type":"string","enum":["pending","in_progress","completed"]}
                    },
                    "required":["content","status"]
                }
            }
        },
        "required":["todos"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: true,
}

/// Render a `todos` JSON array into the `<origin-todos>` block.
///
/// The daemon carries the result back into the prompt. `None` when the list is
/// empty or malformed (the caller then clears the block). Pure so it is
/// unit-testable without the dispatch loop.
#[must_use]
pub fn render_block(todos: &serde_json::Value) -> Option<String> {
    let arr = todos.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(arr.len());
    for item in arr {
        let content = item.get("content").and_then(serde_json::Value::as_str).unwrap_or("");
        if content.is_empty() {
            continue;
        }
        let glyph = match item.get("status").and_then(serde_json::Value::as_str) {
            Some("completed") => "[x]",
            Some("in_progress") => "[~]",
            _ => "[ ]",
        };
        lines.push(format!("{glyph} {content}"));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<origin-todos>\nYour current task checklist (keep it updated via TodoWrite):\n{}\n</origin-todos>",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::render_block;
    use serde_json::json;

    #[test]
    fn renders_status_glyphs() {
        let todos = json!([
            {"content":"design","status":"completed"},
            {"content":"build","status":"in_progress"},
            {"content":"test","status":"pending"},
        ]);
        let block = render_block(&todos).expect("non-empty");
        assert!(block.contains("<origin-todos>"));
        assert!(block.contains("[x] design"));
        assert!(block.contains("[~] build"));
        assert!(block.contains("[ ] test"));
    }

    #[test]
    fn empty_or_malformed_yields_none() {
        assert!(render_block(&json!([])).is_none());
        assert!(render_block(&json!("nope")).is_none());
        assert!(render_block(&json!([{"status":"pending"}])).is_none(), "no content ⇒ skipped");
    }
}
