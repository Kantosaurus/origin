//! Tool envelope — orchestration layer between `dispatch_tool` and the
//! per-tool function.
//!
//! Phase 1 shipped a pure passthrough. Phase 2 adds output-CAS dedup:
//! non-mutating tools whose serialised result is byte-identical to a prior
//! result in this session return a short `{tool_result_ref: "blake3:…"}` token
//! instead of the full body. Mutating tools always return the raw value.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;

use crate::error::{ErrClass, ToolError};
use crate::result_cas::{ref_token, ResultStore};
use crate::SideEffects;

#[derive(Debug, Default, Clone)]
pub struct EnvelopeCtx {
    /// Session-scoped state.
    pub session_id: Option<Arc<str>>,
    /// Output-CAS store shared across all tool calls in the session.
    pub result_store: ResultStore,
}

/// Whether the envelope should attempt output-CAS dedup for this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeMode {
    /// Subject to output-CAS dedup (overridden to "no" if
    /// `side_effects == SideEffects::Mutating`).
    CasEligible,
    /// Never dedup (e.g. tools whose output is intentionally per-call).
    CasOptOut,
}

/// Phase-1 passthrough kept for compatibility with existing tests.
///
/// Delegates to [`run`] with `SideEffects::Pure` and [`EnvelopeMode::CasOptOut`]
/// so it never deduplicates (identical behaviour to Phase 1).
///
/// # Errors
/// Returns `Err` only if the tool result cannot be serialised to JSON.
pub async fn run_passthrough<F, Fut>(
    ctx: &EnvelopeCtx,
    tool_name: &str,
    args: Value,
    tool_fn: F,
) -> Result<Value, ToolError>
where
    F: FnOnce(Value) -> Fut + Send,
    Fut: Future<Output = Result<Value, ToolError>> + Send,
{
    run(ctx, tool_name, SideEffects::Pure, EnvelopeMode::CasOptOut, args, tool_fn).await
}

/// Full envelope: runs `tool_fn`, then for non-mutating CAS-eligible tools,
/// stores the serialised body and returns a short-form `tool_result_ref` on
/// byte-identical repeats within the session.
///
/// # Errors
/// Returns `Err` only if the tool result cannot be serialised to JSON (which
/// should never happen for well-formed `Value` objects).
pub async fn run<F, Fut>(
    ctx: &EnvelopeCtx,
    _tool_name: &str,
    side_effects: SideEffects,
    mode: EnvelopeMode,
    args: Value,
    tool_fn: F,
) -> Result<Value, ToolError>
where
    F: FnOnce(Value) -> Fut + Send,
    Fut: Future<Output = Result<Value, ToolError>> + Send,
{
    let value = match tool_fn(args).await {
        Ok(v) => v,
        Err(e) => return Ok(e.to_json()),
    };

    // Mutating tools and CasOptOut callers bypass dedup entirely.
    if side_effects == SideEffects::Mutating || mode == EnvelopeMode::CasOptOut {
        return Ok(value);
    }

    let body_str = serde_json::to_string(&value).map_err(|e| {
        ToolError::new(ErrClass::Validation, "serialise", e.to_string())
    })?;
    let body_bytes = body_str.as_bytes();
    let hash = blake3::hash(body_bytes);

    // Check if this body already exists in the store.
    if ctx.result_store.get(&hash).is_some() {
        // Repeat hit — return short-form.
        return Ok(ref_token(&hash, body_bytes.len(), &body_str));
    }

    // First occurrence — store and return full value.
    let _ = ctx.result_store.put(body_bytes);
    Ok(value)
}
