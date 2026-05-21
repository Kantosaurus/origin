//! `Browser` tool — dual-backend browser with transparent agent-detection fallback.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::{BrowserRouter, Verb, SnapshotResp};

/// Run a single verb against the router. Caller holds the router across
/// turns; this function is intentionally not lifetimed to the registry
/// (Origin's coordinator owns a `BrowserRouter` and dispatches into this).
///
/// # Errors
/// `String` describing primary/fallback failure.
pub async fn browser(router: &mut BrowserRouter, verb: Verb) -> Result<SnapshotResp, String> {
    router.run(&verb).await.map_err(|e| e.to_string())
}

crate::origin_tool! {
    name: "Browser",
    description: "Stateful browser with agent-detection fallback to CloakBrowser.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{"type":"object","required":["v","session"],"properties":{"v":{"type":"string","enum":["open","click","fill","extract","snapshot","screenshot","close"]},"session":{"type":"string"},"url":{"type":"string"},"ref":{"type":"string"},"value":{"type":"string"},"path":{"type":"string"}}}"#,
}
