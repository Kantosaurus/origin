//! `WebSearch` tool — Tavily-backed search.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::web_search::{search, SearchHit};

/// Search `query` via Tavily; returns up to `count` hits.
///
/// # Errors
/// `String` describing missing API key or HTTP failure.
pub async fn web_search(query: &str, count: usize) -> Result<Vec<SearchHit>, String> {
    search(query, count).await.map_err(|e| e.to_string())
}

crate::origin_tool! {
    name: "WebSearch",
    description: "Search the web via Tavily. Requires TAVILY_API_KEY.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"count":{"type":"integer","minimum":1,"maximum":20}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
