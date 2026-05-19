//! `graph_explain` — run a typed query, route the result through the sidecar.

use origin_codegraph::query::Query;

// TODO(p5): swap NoopSidecar for `origin_sidecar::Sidecar` once Phase 5 lands.
/// Placeholder explanation. Phase 5's sidecar pipes typed-query results
/// through a tight NL template; until then we surface a stable sentinel string
/// so the tool's registration is observable end-to-end.
#[must_use]
#[allow(clippy::module_name_repetitions)] // `graph_explain_tool` follows `recall_tool` precedent
#[allow(clippy::needless_pass_by_value)] // matches future wired signature
pub fn graph_explain_tool(_q: Query) -> String {
    "<explanation pending Phase 5 sidecar>".to_owned()
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
