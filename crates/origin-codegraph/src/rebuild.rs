//! Incremental rebuild driver: input = changed paths, output = `RebuildReport`.
//!
//! P10 will split `nodes_added` vs `nodes_updated` (the upsert is currently
//! opaque to us — `insert_node` does an `INSERT … ON CONFLICT DO UPDATE` and
//! returns the entity id without telling the caller whether the row was new).
//! For P7.8 we bump `nodes_added` for every emitted node; the test asserts on
//! the sum, so the under-counting on `updated` is intentional and bounded.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::extract::extract_nodes;
use crate::index::{CodeGraphIndex, IndexError};
use crate::lang::Language;
use crate::record::CodeNodeRecord;

/// Aggregate counters for a rebuild pass. `errors` collects per-file diagnostics
/// without aborting the pass, so a single bad file can't stall the whole hook.
// `RebuildReport` matches the Phase 7 plan's public API and parallels
// `IndexError`/`QueryError`; the module-name prefix is intentional.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Default)]
pub struct RebuildReport {
    pub paths_seen: usize,
    pub nodes_added: usize,
    pub nodes_updated: usize,
    pub errors: Vec<String>,
}

/// Errors raised by [`rebuild_paths`].
// `RebuildError` parallels `IndexError`/`QueryError` and the `Rebuild` prefix
// is intentional — matches the Phase 7 plan's public API.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum RebuildError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] IndexError),
}

/// Re-extract nodes from each path and upsert into the index.
///
/// Per-file failures (read errors, parse errors) are folded into
/// `report.errors` so the rebuild can make forward progress; fatal CAS or
/// `SQLite` failures from the index layer bubble up as [`RebuildError::Index`].
///
/// # Errors
/// Returns [`RebuildError::Index`] when the underlying [`CodeGraphIndex`]
/// reports a CAS / `SQLite` failure. Returns [`RebuildError::Io`] for I/O
/// failures that escape the per-file recovery (currently none — included for
/// future-proofing once non-file inputs land in P10).
// `rebuild_paths` is the plan-mandated public verb; the module-name prefix is
// intentional and matches the Phase 7 plan's API.
#[allow(clippy::module_name_repetitions)]
pub fn rebuild_paths(
    idx: &mut CodeGraphIndex,
    paths: &[PathBuf],
    lang: Language,
) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();
    for path in paths {
        report.paths_seen += 1;
        match rebuild_one(idx, path, lang) {
            Ok((added, updated)) => {
                report.nodes_added += added;
                report.nodes_updated += updated;
            }
            Err(RebuildError::Index(e)) => return Err(RebuildError::Index(e)),
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        }
    }
    Ok(report)
}

fn rebuild_one(
    idx: &mut CodeGraphIndex,
    path: &Path,
    lang: Language,
) -> Result<(usize, usize), RebuildError> {
    let bytes = std::fs::read(path)?;
    let nodes = extract_nodes(lang, &bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut added = 0;
    let updated = 0; // see module docs — opaque to caller in P7.8.
    for n in nodes {
        let sig = format!("{:?} {} @{}-{}", n.kind, n.name, n.range.start, n.range.end);
        // tree-sitter byte offsets are bounded by `bytes.len()`; clamp
        // defensively so a future grammar change can't panic the slice.
        let end = n.range.end.min(bytes.len());
        let start = n.range.start.min(end);
        let rec = CodeNodeRecord {
            kind: n.kind,
            name: n.name,
            language: lang,
            file_path: path.display().to_string(),
            range: n.range,
            signature: sig.into_bytes(),
            body: bytes[start..end].to_vec(),
        };
        idx.insert_node(&rec)?;
        added += 1;
    }
    Ok((added, updated))
}
