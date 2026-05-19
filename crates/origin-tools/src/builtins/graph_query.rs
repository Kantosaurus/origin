//! `graph_query` — typed code-graph query, returns a CAS handle.

use origin_codegraph::query::Query;
use thiserror::Error;

#[allow(clippy::module_name_repetitions)] // `GraphQueryError` mirrors `RecallError` precedent
#[derive(Debug, Error)]
pub enum GraphQueryError {
    #[error("not yet wired to the live index")]
    Unwired,
}

/// # Errors
/// Returns [`GraphQueryError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex`; the tool's registration is what P7.7 verifies.
#[allow(clippy::module_name_repetitions)] // `graph_query_tool` follows `recall_tool` precedent
#[allow(clippy::needless_pass_by_value)] // matches future wired signature
pub const fn graph_query_tool(_q: Query) -> Result<String, GraphQueryError> {
    Err(GraphQueryError::Unwired)
}

crate::origin_tool! {
    name: "graph_query",
    description: "Run a typed code-graph query: { kind: \"neighbors\" | \"path\" | \"communities\" | \"god_nodes\" | \"recent_changes\", ... }. Returns a CAS handle to the result set.",
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
