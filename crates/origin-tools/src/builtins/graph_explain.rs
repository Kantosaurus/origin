//! `graph_explain` — run a typed query, route the result through the sidecar.

use origin_codegraph::query::Query;
use thiserror::Error;

#[allow(clippy::module_name_repetitions)] // `GraphExplainError` follows `RecallError` precedent
#[derive(Debug, Error)]
pub enum GraphExplainError {
    #[error("not yet wired to the live index")]
    Unwired,
}

/// # Errors
/// Returns [`GraphExplainError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex` and sidecar; the tool's registration is what P7.7 verifies.
#[allow(clippy::module_name_repetitions)] // `graph_explain_tool` follows `recall_tool` precedent
#[allow(clippy::needless_pass_by_value)] // matches future wired signature
pub const fn graph_explain_tool(_q: Query) -> Result<String, GraphExplainError> {
    Err(GraphExplainError::Unwired)
}

crate::origin_tool! {
    name: "graph_explain",
    description: "Run a typed query, then route its result through the sidecar with a tight NL template. Args: same as `graph_query`. The only NL-output graph tool.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "kind": {"type": "string"},
            "args": {"type": "object", "additionalProperties": true}
        },
        "required": ["kind"]
    }"#,
}
