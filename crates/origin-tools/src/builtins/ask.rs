//! `ask` — free-text question; classifier routes to code-graph, memory, or both.

use origin_codegraph::ask::{classify, Route};
use thiserror::Error;

#[allow(clippy::module_name_repetitions)] // `AskError` follows `RecallError` precedent
#[derive(Debug, Error)]
pub enum AskError {
    #[error("not yet wired to the live index / memory backends")]
    Unwired,
}

/// # Errors
/// Returns [`AskError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex` and `MemRouter`; the classifier itself runs here so the
/// route decision is observable in logs, but the actual fan-out is deferred.
#[allow(clippy::module_name_repetitions)] // `ask_tool` follows `recall_tool` precedent
pub fn ask_tool(query: &str) -> Result<String, AskError> {
    let _route: Route = classify(query);
    Err(AskError::Unwired)
}

crate::origin_tool! {
    name: "ask",
    description: "Free-text question; classifier routes to code-graph, memory, or both. No LLM in the router.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "query": {"type": "string"}
        },
        "required": ["query"]
    }"#,
}
