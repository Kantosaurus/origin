// SPDX-License-Identifier: Apache-2.0
//! `ApplyPatch` — apply a unified diff atomically across files.
//!
//! v2 supports the common subset of unified diff:
//! - one or more `--- a/<path>` / `+++ b/<path>` file headers
//! - `@@ -L1,C1 +L2,C2 @@` hunks with `-`/`+`/` ` lines
//!
//! All hunks are validated against the on-disk files before any write; if
//! any hunk fails to apply, no file is modified.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::text_fmt::Detected;
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};
use std::collections::HashMap;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct ApplyPatchArgs {
    pub patch: String,
}

#[derive(Debug)]
struct Hunk {
    file: String,
    old_start: usize,
    lines: Vec<String>, // each line begins with ' ', '-' or '+'
}

/// # Errors
/// `edit.no_match` if a hunk's context does not match disk.
pub fn apply_patch(args: &ApplyPatchArgs) -> Result<Value, ToolError> {
    let hunks = parse_patch(&args.patch)?;

    // Plan all writes first; only commit if every hunk applies cleanly.
    // Key: forward-slash path from diff header; value: (orig_bytes, det, working_text)
    let mut staged: HashMap<String, (Vec<u8>, Detected, String)> = HashMap::new();
    // Per-file cumulative line delta applied by earlier hunks. Hunk `old_start`
    // values are in original-file coordinates, but each hunk is applied to the
    // working text already mutated by previous hunks on the same file, so later
    // hunks must be shifted by the net lines inserted/removed so far.
    let mut offsets: HashMap<String, isize> = HashMap::new();
    for h in &hunks {
        let entry = if let Some(e) = staged.get(&h.file) {
            e.2.clone()
        } else {
            // Resolve the diff path to a native OS path.
            let native_path = to_native_path(&h.file);
            let bytes = std::fs::read(&native_path)
                .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", h.file)))?;
            let det = text_fmt::detect(&bytes);
            let text = text_fmt::normalise_to_lf(&bytes, &det)?;
            staged.insert(h.file.clone(), (bytes, det, text.clone()));
            text
        };
        let offset = *offsets.get(&h.file).unwrap_or(&0);
        let (updated, delta) = apply_one_hunk(&entry, h, offset)?;
        offsets.insert(h.file.clone(), offset + delta);
        if let Some(slot) = staged.get_mut(&h.file) {
            slot.2 = updated;
        }
    }
    let files_updated = staged.len();
    for (path, (_orig_bytes, det, text)) in staged {
        let native_path = to_native_path(&path);
        let new_bytes = text_fmt::denormalise(&text, &det);
        atomic_write(&native_path, &new_bytes)?;
    }
    Ok(json!({"ok": true, "files_updated": files_updated}))
}

/// Convert a diff-style forward-slash path to a native OS path.
/// On Unix this is a no-op; on Windows it replaces '/' with '\\'.
fn to_native_path(path: &str) -> String {
    #[cfg(windows)]
    {
        path.replace('/', "\\")
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ToolError> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut cur_file: Option<String> = None;
    let mut cur: Option<Hunk> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            if let Some(h) = cur.take() {
                hunks.push(h);
            }
            cur_file = Some(rest.to_string());
        } else if line.starts_with("--- a/") || line.starts_with("diff --git") {
            // ignore; +++ line drives the path
        } else if let Some(rest) = line.strip_prefix("@@ -") {
            if let Some(h) = cur.take() {
                hunks.push(h);
            }
            // parse "L1,C1 +L2,C2 @@"
            let mut parts = rest.split(' ');
            let old_part = parts
                .next()
                .ok_or_else(|| ToolError::new(ErrClass::Validation, "bad_patch", "missing old range"))?;
            let l_str = old_part.split(',').next().unwrap_or("1");
            let old_start: usize = l_str
                .parse()
                .map_err(|_| ToolError::new(ErrClass::Validation, "bad_patch", "bad old line number"))?;
            let file = cur_file.clone().ok_or_else(|| {
                ToolError::new(ErrClass::Validation, "bad_patch", "hunk before file header")
            })?;
            cur = Some(Hunk {
                file,
                old_start,
                lines: Vec::new(),
            });
        } else if let Some(h) = cur.as_mut() {
            if line.starts_with(' ') || line.starts_with('-') || line.starts_with('+') {
                h.lines.push(line.to_string());
            }
        }
    }
    if let Some(h) = cur.take() {
        hunks.push(h);
    }
    Ok(hunks)
}

/// Apply one hunk to `text`. `offset` shifts the hunk's original-coordinate
/// `old_start` to account for line insertions/removals by earlier hunks on the
/// same file. Returns the rewritten text and the net line delta of this hunk.
fn apply_one_hunk(text: &str, h: &Hunk, offset: isize) -> Result<(String, isize), ToolError> {
    let lines: Vec<&str> = text.lines().collect();
    // Build the "old block" from ' ' and '-' lines.
    let mut old_block: Vec<&str> = Vec::new();
    let mut new_block: Vec<String> = Vec::new();
    for l in &h.lines {
        let body = &l[1..];
        match l.as_bytes()[0] {
            b' ' => {
                old_block.push(body);
                new_block.push(body.to_string());
            }
            b'-' => {
                old_block.push(body);
            }
            b'+' => {
                new_block.push(body.to_string());
            }
            _ => {}
        }
    }
    // start index in CURRENT (working) coordinates; isize so a negative result
    // from an out-of-range hunk start is rejected rather than wrapping.
    let base = h.old_start.saturating_sub(1) as isize + offset;
    let start_idx = usize::try_from(base).map_err(|_| {
        ToolError::new(
            ErrClass::Edit,
            "no_match",
            format!("hunk @{} resolves before start of {}", h.old_start, h.file),
        )
    })?;
    // Checked add so an attacker-controlled huge `old_start` cannot overflow
    // and slip past the bounds check into an out-of-bounds index panic.
    let past_eof = start_idx
        .checked_add(old_block.len())
        .is_none_or(|end| end > lines.len());
    if past_eof {
        return Err(ToolError::new(
            ErrClass::Edit,
            "no_match",
            format!("hunk @{} extends past EOF in {}", h.old_start, h.file),
        ));
    }
    for (off, exp) in old_block.iter().enumerate() {
        let got = lines[start_idx + off];
        if got != *exp {
            return Err(ToolError::new(
                ErrClass::Edit,
                "no_match",
                format!(
                    "context mismatch at {}:{}: expected `{exp}`, got `{got}`",
                    h.file,
                    h.old_start + off
                ),
            ));
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    out.extend(lines[..start_idx].iter().map(|s| (*s).to_string()));
    out.extend(new_block.iter().cloned());
    out.extend(
        lines[start_idx + old_block.len()..]
            .iter()
            .map(|s| (*s).to_string()),
    );
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    let delta = new_block.len() as isize - old_block.len() as isize;
    Ok((joined, delta))
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.write_all(bytes)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        f.sync_all()
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
    }
    std::fs::rename(&tmp, p).map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))
}

crate::origin_tool! {
    name: "ApplyPatch",
    description: "Apply a unified diff atomically across one or more files. Validates context lines before any write.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "patch": { "type": "string" }
        },
        "required": ["patch"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 4_000,
}
