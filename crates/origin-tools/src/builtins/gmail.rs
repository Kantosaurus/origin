// SPDX-License-Identifier: Apache-2.0
//! `gmail` tool — read-only access to a user's Gmail over Google OAuth 2.0.
//!
//! Registration only: this module submits the `gmail` [`crate::ToolMeta`] into
//! the inventory so the model can call it. The actual execution lives in the
//! daemon's tool dispatch (it loads credentials from the keyvault, mints a
//! token, and runs the requested op via [`origin_gmail::run_tool`]).
//!
//! The tier is [`Tier::RequiresPermission`] because the tool reads private mail
//! — it MUST be permission-gated. [`SideEffects::Pure`] because it never
//! mutates the mailbox (`search` / `get` / `list_threads` are all read-only).
//!
//! The `input_schema:` literal below is byte-identical (modulo serde's stable
//! key ordering) to [`origin_gmail::input_schema()`]; the `schema_matches_crate`
//! test enforces that the two never drift apart.

use crate::{SideEffects, Tier, Urgency};

/// JSON-Schema for the `gmail` tool, inlined as a `&'static str` literal so it
/// can feed the `origin_tool!` macro's `input_schema:` arm. Kept in lock-step
/// with [`origin_gmail::input_schema()`] by the drift-guard test below.
const GMAIL_INPUT_SCHEMA: &str = r#"{"type":"object","properties":{"op":{"type":"string","enum":["search","get","list_threads"],"description":"Which Gmail operation to run."},"query":{"type":"string","description":"Gmail search expression (same syntax as the Gmail search box), e.g. 'from:alice is:unread newer_than:7d'. Required for 'search' and 'list_threads'."},"id":{"type":"string","description":"Message id. Required for 'get'."},"max":{"type":"integer","minimum":1,"maximum":500,"description":"Max results for list operations (default 25)."},"include_body":{"type":"boolean","description":"For 'get': fetch the full message and decode its text body (costs more tokens). Defaults to false (metadata only)."}},"required":["op"],"additionalProperties":false}"#;

crate::origin_tool! {
    name: "gmail",
    description: "Read the user's Gmail (read-only). `op` selects the operation: `search` (find messages by Gmail search expression, e.g. 'from:alice is:unread'), `get` (fetch one message by `id`; set `include_body:true` for the full text body), or `list_threads` (list threads matching a query). Requires Google credentials in the keyvault and explicit user permission.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: GMAIL_INPUT_SCHEMA,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Drift guard: the inlined `&'static str` schema literal must parse to the
    /// exact same JSON value the `origin-gmail` crate produces. If the crate's
    /// schema changes, this fails until the literal is updated to match.
    #[test]
    fn schema_matches_crate() {
        let from_literal: serde_json::Value =
            serde_json::from_str(GMAIL_INPUT_SCHEMA).unwrap();
        assert_eq!(from_literal, origin_gmail::input_schema());
    }
}
