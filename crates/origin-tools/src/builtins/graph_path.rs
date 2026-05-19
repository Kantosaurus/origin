//! `graph_path` — shortest-path query between two code entities.

use thiserror::Error;

#[allow(clippy::module_name_repetitions)] // `GraphPathError` follows `RecallError` precedent
#[derive(Debug, Error)]
pub enum GraphPathError {
    #[error("not yet wired to the live index")]
    Unwired,
}

/// # Errors
/// Returns [`GraphPathError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex`; the tool's registration is what P7.7 verifies.
#[allow(clippy::module_name_repetitions)] // `graph_path_tool` follows `recall_tool` precedent
pub const fn graph_path_tool(
    _from: [u8; 32],
    _to: [u8; 32],
    _max_hops: usize,
) -> Result<String, GraphPathError> {
    Err(GraphPathError::Unwired)
}

crate::origin_tool! {
    name: "graph_path",
    description: "Find a path from one code entity to another by id; { from, to, max_hops?: number }.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "from": {"type": "string", "description": "Lowercase hex entity id (64 chars)."},
            "to": {"type": "string", "description": "Lowercase hex entity id (64 chars)."},
            "max_hops": {"type": "integer", "minimum": 1}
        },
        "required": ["from", "to"]
    }"#,
}
