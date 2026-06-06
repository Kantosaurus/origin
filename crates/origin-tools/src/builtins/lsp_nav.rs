// SPDX-License-Identifier: Apache-2.0
//! `LspNavigate` — go-to-definition / find-references / call-hierarchy via the
//! warm language server.
//!
//! Requires a [`NavigationHandle`](crate::ra_bridge::NavigationHandle) provided
//! by the daemon (see `ra_impl.rs`). Tests use a `FakeNav` (`tests/lsp_nav.rs`).

use crate::error::{ErrClass, ToolError};
use crate::ra_bridge::{NavCallItem, NavLocation, NavigationHandle};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};
use std::path::Path;

/// Arguments for the `LspNavigate` tool call.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct LspNavArgs {
    /// `definition` | `references` | `incoming_calls` | `outgoing_calls`.
    pub op: String,
    /// File path the cursor sits in.
    pub path: String,
    /// 1-based line of the symbol under the cursor.
    pub line: u32,
    /// 1-based column of the symbol under the cursor.
    pub col: u32,
    /// For `references`: include the declaration itself in the results.
    pub include_declaration: bool,
}

fn locations_to_json(locs: &[NavLocation]) -> Value {
    Value::Array(
        locs.iter()
            .map(|l| json!({ "file": l.file, "line": l.line, "col": l.col }))
            .collect(),
    )
}

fn calls_to_json(items: &[NavCallItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|i| json!({ "name": i.name, "file": i.file, "line": i.line, "col": i.col }))
            .collect(),
    )
}

/// Run an LSP navigation query through the daemon-supplied handle.
///
/// `definition`/`references` return `[{file,line,col}]`; the call-hierarchy ops
/// return `[{name,file,line,col}]`. All positions are 1-based.
///
/// # Errors
/// `validation.bad_op` for an unrecognised `op`; otherwise propagates the
/// handle's `subsystem.*` failure (e.g. the language server is unreachable).
pub async fn lsp_navigate(args: LspNavArgs, h: &dyn NavigationHandle) -> Result<Value, ToolError> {
    let path = Path::new(&args.path);
    match args.op.as_str() {
        "definition" => Ok(locations_to_json(&h.definition(path, args.line, args.col).await?)),
        "references" => Ok(locations_to_json(
            &h.references(path, args.line, args.col, args.include_declaration)
                .await?,
        )),
        "incoming_calls" => Ok(calls_to_json(
            &h.incoming_calls(path, args.line, args.col).await?,
        )),
        "outgoing_calls" => Ok(calls_to_json(
            &h.outgoing_calls(path, args.line, args.col).await?,
        )),
        other => Err(ToolError::new(
            ErrClass::Validation,
            "bad_op",
            format!("unknown lsp navigate op: {other:?}"),
        )
        .hint("op must be one of: definition, references, incoming_calls, outgoing_calls")),
    }
}

crate::origin_tool! {
    name: "LspNavigate",
    description: "Navigate code semantically via the warm language server. op: definition (go-to-definition), references (find-references), incoming_calls / outgoing_calls (call hierarchy). path + 1-based line/col point at the symbol. Returns [{file,line,col}] (definition/references) or [{name,file,line,col}] (calls).",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "op":   { "type": "string", "enum": ["definition", "references", "incoming_calls", "outgoing_calls"] },
            "path": { "type": "string" },
            "line": { "type": "integer", "minimum": 1 },
            "col":  { "type": "integer", "minimum": 1 },
            "include_declaration": { "type": "boolean", "default": false }
        },
        "required": ["op", "path", "line", "col"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
