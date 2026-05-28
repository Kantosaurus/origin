//! `graph_summarize` — placeholder until Phase 5's sidecar lands.
//!
//! P7.8 returns [`QueryResult::Empty`] so the tool's registration is observable
//! and the daemon doesn't have to special-case the call site.

use origin_codegraph::index::CodeGraphIndex;
use origin_codegraph::query::QueryResult;

/// Placeholder — Phase 5's sidecar wires the real summarizer.
///
/// `_idx` is intentionally unused at P7.8; it stays in the signature so future
/// wiring doesn't require a breaking API change. `_target` is the community
/// id or node hex passed in by the agent.
#[must_use]
#[allow(clippy::module_name_repetitions)] // `graph_summarize_tool` follows `recall_tool` precedent
#[allow(clippy::needless_pass_by_value)] // matches future wired signature once the summarizer lands
pub fn graph_summarize_tool(_idx: &CodeGraphIndex, _target: String) -> QueryResult {
    QueryResult::Empty
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
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
