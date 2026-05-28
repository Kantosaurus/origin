//! `WebFetch` tool — GET a URL and return reader-mode markdown.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::web_fetch::{fetch, FetchOptions};

/// Fetch `url` and return markdown of the readable content.
///
/// # Errors
/// Returns a `String` describing HTTP, size, or parse failure.
pub async fn web_fetch(url: &str) -> Result<String, String> {
    let r = fetch(url, FetchOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(r.markdown)
}

crate::origin_tool! {
    name: "WebFetch",
    description: "Fetch a URL and return the reader-mode markdown.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","required":["url"],"properties":{"url":{"type":"string"}}}"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
