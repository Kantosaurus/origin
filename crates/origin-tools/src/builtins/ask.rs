// SPDX-License-Identifier: Apache-2.0
//! `ask` — free-text question; classifier routes to code-graph, memory, or both.

use origin_codegraph::ask::{classify, AskResult, MemRouter, Route};
use origin_codegraph::index::CodeGraphIndex;

/// Classify `query` and dispatch the memory side. Code-side dispatch is
/// deferred to Phase 6 (which wires free-text → typed-Query translation), so
/// the function holds `_idx` for forward-compatibility.
#[must_use]
#[allow(clippy::module_name_repetitions)] // `ask_tool` follows `recall_tool` precedent
pub fn ask_tool(_idx: &CodeGraphIndex, mem: &dyn MemRouter, query: &str) -> AskResult {
    let route = classify(query);
    let mem_hits = match route {
        Route::Mem | Route::Both => mem.search(query),
        Route::Code => Vec::new(),
    };
    AskResult::new(route, mem_hits)
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
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
