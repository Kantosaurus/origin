// SPDX-License-Identifier: Apache-2.0
//! `ApplyPatch` — apply a patch atomically across files.
//!
//! Two input formats are accepted; the format is auto-detected from the body:
//!
//! 1. **Unified diff** (the original v2 path), the common subset:
//!    - one or more `--- a/<path>` / `+++ b/<path>` file headers
//!    - `@@ -L1,C1 +L2,C2 @@` hunks with `-`/`+`/` ` lines
//!
//! 2. **Marker envelope** (the codex/opencode `*** Begin Patch` format), a
//!    multi-file operation script supporting four directives:
//!    - `*** Add File: <path>` — following `+`-prefixed lines (with the `+`
//!      stripped) become the new file's content. Errors if the file exists.
//!    - `*** Delete File: <path>` — removes the file. Errors if it is absent.
//!    - `*** Update File: <path>` — followed by `@@` hunks (same syntax as
//!      the unified-diff path) applied to the existing file.
//!    - `*** Move to: <newpath>` — may follow an `*** Update File:` block;
//!      after the hunks apply, the file is renamed to `<newpath>`.
//!
//! The optional `*** Begin Patch` / `*** End Patch` wrappers are tolerated
//! whether present or absent.
//!
//! In BOTH formats every operation is validated against the on-disk files
//! before any write/delete/rename: Add targets must not exist, Delete/Update
//! targets must exist, and Move targets must resolve. If any operation fails
//! validation, no file is modified.

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

/// A single file operation parsed from a `*** Begin Patch` marker envelope.
#[derive(Debug)]
enum Op {
    /// Create a new file from the verbatim content. Target must not exist.
    Add { path: String, content: String },
    /// Remove an existing file. Target must exist.
    Delete { path: String },
    /// Apply `@@` hunks to an existing file, optionally renaming it afterwards.
    Update {
        path: String,
        hunks: Vec<Hunk>,
        move_to: Option<String>,
    },
}

/// Apply a patch to the working tree.
///
/// The input is dispatched by [`is_marker_patch`]: a marker envelope (codex /
/// opencode `*** ... File:` directives) goes through [`apply_marker_patch`];
/// anything else is treated as a unified diff by [`apply_unified_diff`].
///
/// # Errors
/// `edit.no_match` if a hunk's context does not match disk;
/// `validation.bad_patch` for malformed markers/hunks;
/// `io.exists` / `io.not_found` if an Add/Delete/Update target is in the wrong
/// state on disk.
pub fn apply_patch(args: &ApplyPatchArgs) -> Result<Value, ToolError> {
    if is_marker_patch(&args.patch) {
        apply_marker_patch(&args.patch)
    } else {
        apply_unified_diff(&args.patch)
    }
}

/// Detect the codex/opencode marker envelope. Returns `true` when any of the
/// recognised `*** ... File:` directives (or the `*** Begin Patch` wrapper)
/// appears on a line, after trimming leading whitespace. The unified-diff path
/// never emits these markers, so detection is unambiguous.
fn is_marker_patch(patch: &str) -> bool {
    patch.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("*** Begin Patch")
            || t.starts_with("*** Add File:")
            || t.starts_with("*** Delete File:")
            || t.starts_with("*** Update File:")
    })
}

/// The original unified-diff path, byte-identical to the pre-marker behavior.
///
/// # Errors
/// `edit.no_match` if a hunk's context does not match disk.
fn apply_unified_diff(patch: &str) -> Result<Value, ToolError> {
    let hunks = parse_patch(patch)?;

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

/// A validated filesystem action queued by [`stage_op`] for the commit phase.
/// Each variant carries the exact bytes/paths so the commit pass performs no
/// further validation or computation.
enum Staged {
    Write { native: String, bytes: Vec<u8> },
    Delete { native: String },
    Rename { from: String, to: String, bytes: Vec<u8> },
}

/// Running tally of operations, mirrored into the JSON result.
#[derive(Default)]
struct OpCounts {
    added: usize,
    updated: usize,
    deleted: usize,
    moved: usize,
}

/// Apply a codex/opencode marker-envelope patch.
///
/// Validates every operation against disk first (Add target absent,
/// Delete/Update target present, Move target dir resolvable) via [`stage_op`],
/// then commits the staged actions in a single pass via [`commit_plan`]. If any
/// operation fails validation, no file is touched.
///
/// # Errors
/// `validation.bad_patch` for malformed markers; `io.exists` if an Add target
/// already exists; `io.not_found` if a Delete/Update target is missing;
/// `edit.no_match` if an Update hunk's context does not match disk.
fn apply_marker_patch(patch: &str) -> Result<Value, ToolError> {
    let ops = parse_marker_patch(patch)?;

    // Build the full plan (validating every op) before any filesystem write.
    let mut plan: Vec<Staged> = Vec::new();
    let mut counts = OpCounts::default();
    for op in &ops {
        stage_op(op, &mut plan, &mut counts)?;
    }

    commit_plan(plan)?;

    Ok(json!({
        "ok": true,
        "files_added": counts.added,
        "files_updated": counts.updated,
        "files_deleted": counts.deleted,
        "files_moved": counts.moved,
    }))
}

/// Validate a single [`Op`] against disk and push its [`Staged`] action(s) onto
/// `plan`, updating `counts`. Performs no writes — only reads and checks.
fn stage_op(op: &Op, plan: &mut Vec<Staged>, counts: &mut OpCounts) -> Result<(), ToolError> {
    match op {
        Op::Add { path, content } => {
            let native = to_native_path(path);
            if std::path::Path::new(&native).exists() {
                return Err(ToolError::new(
                    ErrClass::Io,
                    "exists",
                    format!("Add File target already exists: {path}"),
                ));
            }
            counts.added += 1;
            plan.push(Staged::Write {
                native,
                bytes: content.clone().into_bytes(),
            });
        }
        Op::Delete { path } => {
            let native = to_native_path(path);
            if !std::path::Path::new(&native).exists() {
                return Err(ToolError::new(
                    ErrClass::Io,
                    "not_found",
                    format!("Delete File target does not exist: {path}"),
                ));
            }
            counts.deleted += 1;
            plan.push(Staged::Delete { native });
        }
        Op::Update {
            path,
            hunks,
            move_to,
        } => {
            let native = to_native_path(path);
            let bytes = std::fs::read(&native)
                .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{path}: {e}")))?;
            let det = text_fmt::detect(&bytes);
            let mut text = text_fmt::normalise_to_lf(&bytes, &det)?;
            // Hunks are in original-file coordinates; shift later hunks by the
            // net line delta of earlier ones on this same file.
            let mut offset = 0_isize;
            for h in hunks {
                let (updated, delta) = apply_one_hunk(&text, h, offset)?;
                text = updated;
                offset += delta;
            }
            let new_bytes = text_fmt::denormalise(&text, &det);
            counts.updated += 1;
            if let Some(dest) = move_to {
                let dest_native = to_native_path(dest);
                // Move target must be resolvable and must not already exist as a
                // different file (its parent dir is created in the commit step).
                if dest_native != native && std::path::Path::new(&dest_native).exists() {
                    return Err(ToolError::new(
                        ErrClass::Io,
                        "exists",
                        format!("Move to target already exists: {dest}"),
                    ));
                }
                counts.moved += 1;
                plan.push(Staged::Rename {
                    from: native,
                    to: dest_native,
                    bytes: new_bytes,
                });
            } else {
                plan.push(Staged::Write {
                    native,
                    bytes: new_bytes,
                });
            }
        }
    }
    Ok(())
}

/// Perform the side effects of a fully-validated plan. Every entry was checked
/// by [`stage_op`], so this pass only writes/deletes/renames.
fn commit_plan(plan: Vec<Staged>) -> Result<(), ToolError> {
    for staged in plan {
        match staged {
            Staged::Write { native, bytes } => {
                create_parent_dirs(&native)?;
                atomic_write(&native, &bytes)?;
            }
            Staged::Delete { native } => {
                std::fs::remove_file(&native).map_err(|e| {
                    ToolError::new(ErrClass::Io, "permission", format!("{native}: {e}"))
                })?;
            }
            Staged::Rename { from, to, bytes } => {
                create_parent_dirs(&to)?;
                // Write the updated content to the new path, then remove the old
                // one. (A bare rename would lose the in-place hunk edits.)
                atomic_write(&to, &bytes)?;
                if to != from {
                    std::fs::remove_file(&from).map_err(|e| {
                        ToolError::new(ErrClass::Io, "permission", format!("{from}: {e}"))
                    })?;
                }
            }
        }
    }
    Ok(())
}

/// Create the parent directory chain for a native path, if any. No-op when the
/// path has no parent or the parent already exists.
fn create_parent_dirs(native: &str) -> Result<(), ToolError> {
    if let Some(parent) = std::path::Path::new(native).parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::new(
                    ErrClass::Io,
                    "permission",
                    format!("{}: {e}", parent.display()),
                )
            })?;
        }
    }
    Ok(())
}

/// Parse a marker-envelope patch into an ordered list of [`Op`]s.
///
/// Tolerates the optional `*** Begin Patch` / `*** End Patch` wrappers. Hunk
/// lines inside an `*** Update File:` block reuse the same `@@`/` `/`-`/`+`
/// grammar as the unified-diff path.
fn parse_marker_patch(patch: &str) -> Result<Vec<Op>, ToolError> {
    let mut p = MarkerParser::default();
    for raw in patch.lines() {
        p.feed(raw)?;
    }
    let ops = p.finish();
    if ops.is_empty() {
        return Err(ToolError::new(
            ErrClass::Validation,
            "bad_patch",
            "marker patch contained no operations",
        ));
    }
    Ok(ops)
}

/// Whichever directive block the [`MarkerParser`] is currently accumulating.
#[derive(Default)]
enum Block {
    #[default]
    None,
    /// `*** Add File:` — `path` plus the `+`-stripped content lines so far.
    Add { path: String, lines: Vec<String> },
    /// `*** Update File:` — `path`, finished hunks, the in-progress hunk, and
    /// an optional pending `*** Move to:` destination.
    Update {
        path: String,
        hunks: Vec<Hunk>,
        cur: Option<Hunk>,
        move_to: Option<String>,
    },
}

/// Line-oriented state machine for the marker envelope. Each [`feed`] call
/// either opens a new directive (flushing the previous block) or appends the
/// line to the open block.
#[derive(Default)]
struct MarkerParser {
    ops: Vec<Op>,
    block: Block,
}

impl MarkerParser {
    /// Close the current block (if any), pushing its completed [`Op`].
    fn flush(&mut self) {
        match std::mem::take(&mut self.block) {
            Block::None => {}
            Block::Add { path, lines } => {
                // Join with '\n'; codex Add blocks are newline-terminated, so
                // append a trailing newline when any content line was present.
                let mut content = lines.join("\n");
                if !lines.is_empty() {
                    content.push('\n');
                }
                self.ops.push(Op::Add { path, content });
            }
            Block::Update {
                path,
                mut hunks,
                cur,
                move_to,
            } => {
                if let Some(h) = cur {
                    hunks.push(h);
                }
                self.ops.push(Op::Update {
                    path,
                    hunks,
                    move_to,
                });
            }
        }
    }

    /// Process one raw input line.
    fn feed(&mut self, raw: &str) -> Result<(), ToolError> {
        let line = raw.trim_start();
        if line.starts_with("*** Begin Patch") || line.starts_with("*** End Patch") {
            self.flush();
        } else if let Some(path) = line.strip_prefix("*** Add File:") {
            self.flush();
            self.block = Block::Add {
                path: path.trim().to_string(),
                lines: Vec::new(),
            };
        } else if let Some(path) = line.strip_prefix("*** Delete File:") {
            self.flush();
            self.ops.push(Op::Delete {
                path: path.trim().to_string(),
            });
        } else if let Some(path) = line.strip_prefix("*** Update File:") {
            self.flush();
            self.block = Block::Update {
                path: path.trim().to_string(),
                hunks: Vec::new(),
                cur: None,
                move_to: None,
            };
        } else if let Some(dest) = line.strip_prefix("*** Move to:") {
            let Block::Update { move_to, .. } = &mut self.block else {
                return Err(ToolError::new(
                    ErrClass::Validation,
                    "bad_patch",
                    "`*** Move to:` without a preceding `*** Update File:`",
                ));
            };
            *move_to = Some(dest.trim().to_string());
        } else {
            self.feed_body(raw, line)?;
        }
        Ok(())
    }

    /// Append a non-directive line to whichever block is open.
    fn feed_body(&mut self, raw: &str, line: &str) -> Result<(), ToolError> {
        match &mut self.block {
            Block::Add { lines, .. } => {
                // Add File bodies are `+`-prefixed; tolerate a bare line too.
                lines.push(raw.strip_prefix('+').unwrap_or(raw).to_string());
            }
            Block::Update {
                path, hunks, cur, ..
            } => {
                if let Some(rest) = line.strip_prefix("@@ -") {
                    if let Some(h) = cur.take() {
                        hunks.push(h);
                    }
                    let old_part = rest.split(' ').next().ok_or_else(|| {
                        ToolError::new(ErrClass::Validation, "bad_patch", "missing old range")
                    })?;
                    let l_str = old_part.split(',').next().unwrap_or("1");
                    let old_start: usize = l_str.parse().map_err(|_| {
                        ToolError::new(ErrClass::Validation, "bad_patch", "bad old line number")
                    })?;
                    *cur = Some(Hunk {
                        file: path.clone(),
                        old_start,
                        lines: Vec::new(),
                    });
                } else if let Some(h) = cur.as_mut() {
                    if raw.starts_with(' ') || raw.starts_with('-') || raw.starts_with('+') {
                        h.lines.push(raw.to_string());
                    }
                }
            }
            // Outside any block (e.g. blank lines around the envelope): ignore.
            Block::None => {}
        }
        Ok(())
    }

    /// Flush the trailing block and return the parsed operations.
    fn finish(mut self) -> Vec<Op> {
        self.flush();
        self.ops
    }
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
    let base = isize::try_from(h.old_start.saturating_sub(1))
        .unwrap_or(isize::MAX)
        .saturating_add(offset);
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
    let delta = isize::try_from(new_block.len()).unwrap_or(isize::MAX)
        - isize::try_from(old_block.len()).unwrap_or(isize::MAX);
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
    description: "Apply a patch atomically across one or more files. Accepts either a unified diff (--- a/ / +++ b/ headers with @@ hunks) OR a codex/opencode marker envelope: optional `*** Begin Patch`/`*** End Patch` wrappers around `*** Add File: <path>` (+-prefixed content; file must not exist), `*** Delete File: <path>` (file must exist), and `*** Update File: <path>` (followed by @@ hunks, optionally then `*** Move to: <newpath>` to rename after editing). All operations are validated before any write; if any fails, nothing changes.",
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
