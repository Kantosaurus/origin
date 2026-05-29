// SPDX-License-Identifier: Apache-2.0
//! `Diagnostics` — query LSP diagnostics from the warm rust-analyzer.
//!
//! Requires a `DiagnosticsHandle` provided by the daemon (see `ra_impl.rs`).
//! Tests use `FakeRa` (in `tests/diagnostics.rs`).

use crate::error::ToolError;
use crate::ra_bridge::{DiagnosticsHandle, Severity};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Arguments for the `Diagnostics` tool call.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct DiagnosticsArgs {
    /// Optional file path to filter diagnostics to. `None` = entire workspace.
    pub path: Option<String>,
    /// Severity filter.
    pub severity: Severity,
}

/// Fetch diagnostics from the rust-analyzer bridge.
///
/// # Errors
/// `subsystem.ra_unavailable` if RA is unreachable.
pub async fn diagnostics(args: DiagnosticsArgs, h: &dyn DiagnosticsHandle) -> Result<Value, ToolError> {
    let path: Option<PathBuf> = args.path.as_deref().map(PathBuf::from);
    let diags = h.diagnostics(path.as_deref(), args.severity).await?;
    let filtered: Vec<Value> = diags
        .into_iter()
        .filter(|d| args.severity.allows(d.severity))
        .map(|d| {
            json!({
                "file": d.file,
                "line": d.line,
                "col": d.col,
                "severity": d.severity,
                "message": d.message,
                "code": d.code,
            })
        })
        .collect();
    Ok(Value::Array(filtered))
}

crate::origin_tool! {
    name: "Diagnostics",
    description: "Return LSP diagnostics from the warm rust-analyzer for a path or the whole workspace. Severity filter: error|warning|hint|any.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path":     { "type": "string" },
            "severity": { "type": "string", "enum": ["error", "warning", "hint", "any"], "default": "any" }
        }
    }"#,
}
