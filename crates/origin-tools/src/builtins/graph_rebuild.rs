//! `graph_rebuild` — rebuild the code graph over a path set (mutating).

use std::path::PathBuf;

use origin_codegraph::index::CodeGraphIndex;
use origin_codegraph::rebuild::{rebuild_paths, RebuildError, RebuildReport};
use origin_codegraph::Language;

/// Re-extract nodes for each path and upsert them into `idx`.
///
/// # Errors
/// Propagates [`RebuildError`] for fatal CAS / `SQLite` failures; per-file
/// diagnostics are aggregated into the returned report.
#[allow(clippy::module_name_repetitions)] // `graph_rebuild_tool` follows `recall_tool` precedent
#[allow(clippy::needless_pass_by_value)] // matches future wired signature
pub fn graph_rebuild_tool(
    idx: &mut CodeGraphIndex,
    paths: Vec<PathBuf>,
    lang: Language,
) -> Result<RebuildReport, RebuildError> {
    rebuild_paths(idx, &paths, lang)
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
