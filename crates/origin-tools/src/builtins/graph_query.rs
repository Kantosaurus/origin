//! `graph_query` — typed code-graph query, dispatched against the live index.

use origin_codegraph::index::CodeGraphIndex;
use origin_codegraph::query::{dispatch, Query, QueryError, QueryResult};

/// Dispatch a typed [`Query`] against `idx`.
///
/// # Errors
/// Propagates [`QueryError`] from [`origin_codegraph::query::dispatch`].
#[allow(clippy::module_name_repetitions)] // `graph_query_tool` follows `recall_tool` precedent
pub fn graph_query_tool(idx: &CodeGraphIndex, q: Query) -> Result<QueryResult, QueryError> {
    dispatch(idx, q)
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
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
