//! `graph_rebuild` — rebuild the code graph over a path set (mutating).

use std::path::PathBuf;

use thiserror::Error;

#[allow(clippy::module_name_repetitions)] // `GraphRebuildError` follows `RecallError` precedent
#[derive(Debug, Error)]
pub enum GraphRebuildError {
    #[error("not yet wired to the live index")]
    Unwired,
}

/// # Errors
/// Returns [`GraphRebuildError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex` rebuild pipeline; the tool's registration is what P7.7
/// verifies.
#[allow(clippy::module_name_repetitions)] // `graph_rebuild_tool` follows `recall_tool` precedent
#[allow(clippy::needless_pass_by_value)] // matches future wired signature
pub fn graph_rebuild_tool(_paths: Vec<PathBuf>) -> Result<String, GraphRebuildError> {
    Err(GraphRebuildError::Unwired)
}

crate::origin_tool! {
    name: "graph_rebuild",
    description: "Rebuild the code graph over { paths: string[] } (empty array = full repo). Asynchronous; returns a job handle.",
    tier: crate::Tier::RequiresPermission,
    urgency: crate::Urgency::Medium,
    side_effects: crate::SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": {"type": "string"}
            }
        },
        "required": ["paths"]
    }"#,
}
