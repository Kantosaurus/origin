// SPDX-License-Identifier: Apache-2.0
//! `graph_explain` — produce a deterministic natural-language description
//! of a typed [`Query`].

use origin_codegraph::index::EntityId;
use origin_codegraph::query::Query;

/// Return the first 8 hex chars of an [`EntityId`]'s 32-byte payload.
fn hex8(id: EntityId) -> String {
    let bytes = id.0;
    // 4 bytes → 8 hex chars.
    let mut s = String::with_capacity(8);
    for b in &bytes[..4] {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Describe a typed [`Query`] in one short, deterministic English sentence.
///
/// This intentionally describes the *query itself*, not its results — the
/// function has no index handle. Used as the NL surface for the
/// `graph_explain` tool registration; result-aware explanation will be added
/// when callers can thread a `CodeGraphIndex` through this signature.
#[must_use]
#[allow(clippy::module_name_repetitions)] // `graph_explain_tool` follows `recall_tool` precedent
pub fn graph_explain_tool(q: &Query) -> String {
    match *q {
        Query::Path { from, to, max_hops } => format!(
            "shortest path from {} to {} within {max_hops} hops",
            hex8(from),
            hex8(to)
        ),
        Query::Neighbors { node, depth } => {
            format!("neighbors of {} up to depth {depth}", hex8(node))
        }
        Query::Communities => "all detected communities".to_owned(),
        Query::GodNodes { top_per_partition } => {
            format!("top {top_per_partition} god-nodes per community")
        }
        Query::RecentChanges { since_ms } => {
            format!("nodes changed since unix-ms {since_ms}")
        }
    }
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
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
