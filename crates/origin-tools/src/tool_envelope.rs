//! Tool envelope — orchestration layer between `dispatch_tool` and the
//! per-tool function.
//!
//! Phase 1 ships a pure passthrough: it accepts a tool-fn future, awaits it,
//! and serialises a `ToolError` to its `{kind, message, ...}` JSON form. Later
//! phases extend this with input canon, output-CAS lookup, budget writing,
//! and streaming.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;

use crate::error::ToolError;

#[derive(Debug, Default, Clone)]
pub struct EnvelopeCtx {
    /// Session-scoped state. Stub in Phase 1; populated in later phases
    /// (output-CAS handle, supervisor handle, RA bridge, etc.).
    pub session_id: Option<Arc<str>>,
}

/// Phase 1 passthrough: invoke `tool_fn(args)` and return either its value
/// (on Ok) or the structured error JSON (on Err).
///
/// # Errors
/// Never returns `Err` in Phase 1: any `ToolError` from `tool_fn` is
/// converted to its JSON representation and returned as `Ok(json)`. This
/// signature is forward-compatible with Phase 2+ where envelope-level errors
/// (e.g. CAS I/O failure) will propagate as `Err`.
pub async fn run_passthrough<F, Fut>(
    _ctx: &EnvelopeCtx,
    _tool_name: &str,
    args: Value,
    tool_fn: F,
) -> Result<Value, ToolError>
where
    F: FnOnce(Value) -> Fut + Send,
    Fut: Future<Output = Result<Value, ToolError>> + Send,
{
    match tool_fn(args).await {
        Ok(v) => Ok(v),
        Err(e) => Ok(e.to_json()),
    }
}
