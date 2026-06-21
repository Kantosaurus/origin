// SPDX-License-Identifier: Apache-2.0
//! `Write` v2 — atomic write, read-before-write guard, EOL preservation.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct WriteArgs {
    pub file_path: String,
    pub content: String,
    pub force: bool,
}

/// Per-session record of which paths have been Read, with a size+mtime snapshot.
///
/// So the Write/Edit guard permits overwrites the model has seen, and forces a
/// fresh Read when something out of band (`Bash` `sed -i`, `cargo fmt`, codegen,
/// `git checkout`) changed the file since. Without the snapshot, `has_read`
/// stayed true forever and the next Edit ran `old_string` against stale bytes.
/// path → size+mtime stamp at Read time (`None` if the stat failed then).
type ReadStamps = HashMap<String, Option<(u64, u128)>>;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Default, Clone)]
pub struct WriteGuard {
    // `None` stamp ⇒ stat failed at read time; fall back to presence-only
    // (today's behaviour) for that path so the guard never gets *stricter*.
    read_paths: Arc<RwLock<ReadStamps>>,
}

/// `(len, mtime-nanos)` for `path`, or `None` if it cannot be stat-ed.
fn stat_key(path: &str) -> Option<(u64, u128)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some((m.len(), mtime))
}

impl WriteGuard {
    /// Mark `path` as having been read in this session, snapshotting its current
    /// size+mtime so a later out-of-band change can be detected.
    ///
    /// # Panics
    /// Panics if the internal `RwLock` is poisoned (i.e., a prior writer panicked
    /// while holding the lock — not expected in normal operation).
    pub fn note_read(&self, path: &str) {
        let canon = canonical_key(path);
        let stat = stat_key(&canon);
        self.read_paths
            .write()
            .expect("WriteGuard RwLock poisoned")
            .insert(canon, stat);
    }

    /// Returns `true` if `path` was Read this session AND has not changed on disk
    /// since (same size+mtime). Fail-open: if either the stored or the current
    /// stat is unavailable, fall back to presence-only so we never demand a
    /// needless re-Read on a stat error.
    ///
    /// # Panics
    /// Panics if the internal `RwLock` is poisoned.
    #[must_use]
    pub fn has_read(&self, path: &str) -> bool {
        let canon = canonical_key(path);
        // Copy the snapshot out and drop the lock BEFORE the disk stat below.
        let snapshot = {
            let guard = self.read_paths.read().expect("WriteGuard RwLock poisoned");
            match guard.get(&canon) {
                Some(s) => *s,
                None => return false,
            }
        };
        match (snapshot, stat_key(&canon)) {
            // Both stats known → the file is "read" only if it is unchanged.
            (Some(old), Some(now)) => old == now,
            // Either stat unavailable → presence-only fallback (was Read).
            _ => true,
        }
    }
}

fn canonical_key(path: &str) -> String {
    std::fs::canonicalize(path).map_or_else(|_| path.to_string(), |p| p.to_string_lossy().into_owned())
}

/// # Errors
/// `edit.read_required` if overwriting an existing file the model did not Read
/// this session and `force=false`. `io.permission` on disk errors.
#[allow(clippy::module_name_repetitions)]
pub fn write_v2(args: WriteArgs, guard: &WriteGuard) -> Result<(), ToolError> {
    let path = std::path::Path::new(&args.file_path);
    let existed = path.exists();

    if existed && !args.force && !guard.has_read(&args.file_path) {
        return Err(ToolError::new(
            ErrClass::Edit,
            "read_required",
            format!("refusing to overwrite '{}' that has not been Read in this session; pass force=true to override", args.file_path),
        ).recoverable(true).hint("call Read on this file first, then re-Write"));
    }

    // Preserve original convention if the file existed.
    let bytes_out = if existed {
        let prior = std::fs::read(&args.file_path)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", e.to_string()))?;
        let det = text_fmt::detect(&prior);
        text_fmt::denormalise(&args.content, &det)
    } else {
        args.content.into_bytes()
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::new(
                    ErrClass::Io,
                    "permission",
                    format!("mkdir {}: {e}", parent.display()),
                )
            })?;
        }
    }

    atomic_write(&args.file_path, &bytes_out)
}

fn atomic_write(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;
    let p = std::path::Path::new(path);
    let pid = std::process::id();
    let tmp = p.with_extension(format!("tmp{pid}"));
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("create tmp: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("write tmp: {e}")))?;
        f.sync_all()
            .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("fsync: {e}")))?;
    }
    std::fs::rename(&tmp, p)
        .map_err(|e| ToolError::new(ErrClass::Io, "permission", format!("rename: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("origin_wg_{}_{tag}", std::process::id()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn unchanged_file_stays_read_but_out_of_band_change_forces_reread() {
        let path = tmp("stale");
        std::fs::write(&path, b"original\n").expect("write temp");
        let g = WriteGuard::default();
        g.note_read(&path);
        assert!(g.has_read(&path), "just-read, unmodified file is read");

        // Simulate a Bash/format/codegen mutation: bytes (and len) change.
        std::fs::write(&path, b"mutated by bash sed -i\n").expect("rewrite temp");
        assert!(
            !g.has_read(&path),
            "an out-of-band change since Read must force a fresh Read"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn never_read_is_not_read() {
        let g = WriteGuard::default();
        assert!(!g.has_read(&tmp("never")));
    }
}

crate::origin_tool! {
    name: "Write",
    description: "Create or overwrite a UTF-8 file. Atomic. Refuses overwrite of unread existing files unless force=true.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path": { "type": "string" },
            "content":   { "type": "string" },
            "force":     { "type": "boolean", "default": false }
        },
        "required": ["file_path", "content"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::WriteCwd,
    token_budget: 1_000,
}
