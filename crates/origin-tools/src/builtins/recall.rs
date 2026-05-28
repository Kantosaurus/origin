//! `Recall` — inflate a CAS handle into the message stream (N5.5).

use origin_cas::{Hash, Store};
use thiserror::Error;

/// Region selector for `recall_tool`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Region {
    /// 1-based inclusive line range.
    Lines { start: usize, end: usize },
    /// Regex; matching lines are returned in original order, separated by `\n`.
    Match { pattern: String },
    /// Outline-only mode — Phase 3 returns a placeholder because the sidecar
    /// coroutine that emits structure summaries lands in Phase 6.
    OutlineOnly,
}

/// Errors returned by [`recall_tool`].
#[allow(clippy::module_name_repetitions)] // `RecallError` in module `recall` — name kept for API clarity
#[derive(Debug, Error)]
pub enum RecallError {
    /// CAS store error.
    #[error("cas: {0}")]
    Cas(#[from] origin_cas::StoreError),
    /// Requested handle was not present in the store.
    #[error("handle not in store")]
    Missing,
    /// Line range is out of bounds for the stored body.
    #[error("invalid line range {start}..={end} (body has {total} lines)")]
    BadRange { start: usize, end: usize, total: usize },
    /// Regex pattern failed to compile.
    #[error("invalid regex: {0}")]
    Regex(String),
    /// Stored bytes are not valid UTF-8.
    #[error("body is not valid utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Inflate a CAS handle and slice it per `region`.
///
/// # Errors
/// Returns [`RecallError::Cas`] on store I/O failure, [`RecallError::Missing`]
/// when the handle is not present, [`RecallError::BadRange`] for out-of-bounds
/// line selectors, [`RecallError::Regex`] for invalid patterns, and
/// [`RecallError::Utf8`] when the stored bytes are not valid UTF-8.
#[allow(clippy::module_name_repetitions)] // `recall_tool` in module `recall` — name kept for API clarity
pub fn recall_tool(store: &Store, handle: [u8; 32], region: Option<Region>) -> Result<String, RecallError> {
    let body_bytes = store.get(Hash::from_bytes(handle))?.ok_or(RecallError::Missing)?;
    let body = std::str::from_utf8(&body_bytes)?;

    match region {
        None => Ok(body.to_owned()),
        Some(Region::Lines { start, end }) => {
            let lines: Vec<&str> = body.split('\n').collect();
            let total = lines.len();
            if start == 0 || start > end || end > total {
                return Err(RecallError::BadRange { start, end, total });
            }
            Ok(lines[(start - 1)..end].join("\n"))
        }
        Some(Region::Match { pattern }) => {
            let re = regex::Regex::new(&pattern).map_err(|e| RecallError::Regex(e.to_string()))?;
            let matched: Vec<&str> = body.split('\n').filter(|l| re.is_match(l)).collect();
            Ok(matched.join("\n"))
        }
        Some(Region::OutlineOnly) => Ok(outline_only(body)),
    }
}

/// Cap on outline lines returned, to bound output for pathologically large inputs.
const OUTLINE_CAP: usize = 200;

/// Extract an "outline" of a UTF-8 body: markdown headings and language-ish
/// declaration signatures. Returns lines joined by `\n` in source order, with
/// each line counted at most once. If nothing matches, returns the literal
/// sentinel `"<no outline structure detected>"`. Output is capped at
/// [`OUTLINE_CAP`] entries.
fn outline_only(body: &str) -> String {
    // Compiled lazily-but-once via `std::sync::OnceLock` would be ideal; for a
    // single function call we just build them locally — `regex::Regex::new`
    // is cheap relative to the I/O that produced `body`.
    let rust_sig =
        regex::Regex::new(r"\b(?:fn|struct|enum|trait|impl)\s+[A-Za-z_]").expect("static regex compiles");
    let py_sig = regex::Regex::new(r"^\s*(?:def|class)\s+[A-Za-z_]").expect("static regex compiles");
    let js_fn = regex::Regex::new(r"\bfunction\s+[A-Za-z_$]").expect("static regex compiles");
    let js_export = regex::Regex::new(r"\bexport\s+(?:function|class|const|let|interface|type|enum)\b")
        .expect("static regex compiles");

    let mut out: Vec<&str> = Vec::new();
    for line in body.lines() {
        let trimmed_start = line.trim_start();
        let is_md_heading = trimmed_start.starts_with('#');
        let is_rust = rust_sig.is_match(line);
        let is_py = py_sig.is_match(line);
        let is_js = js_fn.is_match(line) || js_export.is_match(line);
        if is_md_heading || is_rust || is_py || is_js {
            out.push(line.trim());
            if out.len() >= OUTLINE_CAP {
                break;
            }
        }
    }
    if out.is_empty() {
        "<no outline structure detected>".to_owned()
    } else {
        out.join("\n")
    }
}

crate::origin_tool! {
    name: "Recall",
    description: "Inflate a CAS handle into the response. Optional region: { lines: [start, end] } | { match: \"regex\" } | { outline_only: true }.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "handle": {"type": "string", "description": "Lowercase hex CAS hash (64 chars)."},
            "region": {
                "type": "object",
                "description": "Optional slice selector.",
                "additionalProperties": true
            }
        },
        "required": ["handle"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Inherit,
    token_budget: crate::DEFAULT_TOKEN_BUDGET,
    hot: false,
}
