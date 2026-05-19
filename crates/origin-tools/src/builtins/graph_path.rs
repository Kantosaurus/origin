//! `graph_path` — shortest-path query between two code entities.

use origin_codegraph::index::{CodeGraphIndex, EntityId};
use origin_codegraph::query::{dispatch, Query, QueryError, QueryResult};

/// Find a shortest path of length ≤ `max_hops` from `from` to `to`.
///
/// # Errors
/// Propagates [`QueryError`] from [`origin_codegraph::query::dispatch`].
#[allow(clippy::module_name_repetitions)] // `graph_path_tool` follows `recall_tool` precedent
pub fn graph_path_tool(
    idx: &CodeGraphIndex,
    from: EntityId,
    to: EntityId,
    max_hops: usize,
) -> Result<QueryResult, QueryError> {
    dispatch(idx, Query::Path { from, to, max_hops })
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
