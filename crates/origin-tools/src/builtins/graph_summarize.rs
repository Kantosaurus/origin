//! `graph_summarize` — summarize a community or node neighborhood.

use thiserror::Error;

#[allow(clippy::module_name_repetitions)] // `GraphSummarizeError` follows `RecallError` precedent
#[derive(Debug, Error)]
pub enum GraphSummarizeError {
    #[error("not yet wired to the live index")]
    Unwired,
}

/// # Errors
/// Returns [`GraphSummarizeError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex`; the tool's registration is what P7.7 verifies.
#[allow(clippy::module_name_repetitions)] // `graph_summarize_tool` follows `recall_tool` precedent
pub fn graph_summarize_tool(_target: String) -> Result<String, GraphSummarizeError> {
    Err(GraphSummarizeError::Unwired)
}

crate::origin_tool! {
    name: "graph_summarize",
    description: "Summarize a community ({ community_id }) or a node neighborhood ({ node }). Returns CAS-handled bullets.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "community_id": {"type": "integer"},
            "node": {"type": "string", "description": "Lowercase hex entity id (64 chars)."}
        }
    }"#,
}
