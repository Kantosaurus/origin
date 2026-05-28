//! `Write` v2 — atomic write, read-before-write guard, EOL preservation.

use std::collections::HashSet;
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

/// Per-session record of which file paths have been Read so the Write guard
/// can permit overwrites that the model has actually seen.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Default, Clone)]
pub struct WriteGuard {
    read_paths: Arc<RwLock<HashSet<String>>>,
}

impl WriteGuard {
    /// Mark `path` as having been read in this session.
    ///
    /// # Panics
    /// Panics if the internal `RwLock` is poisoned (i.e., a prior writer panicked
    /// while holding the lock — not expected in normal operation).
    pub fn note_read(&self, path: &str) {
        let canon = canonical_key(path);
        self.read_paths
            .write()
            .expect("WriteGuard RwLock poisoned")
            .insert(canon);
    }

    /// Returns `true` if `path` has been marked as read in this session.
    ///
    /// # Panics
    /// Panics if the internal `RwLock` is poisoned.
    #[must_use]
    pub fn has_read(&self, path: &str) -> bool {
        self.read_paths
            .read()
            .expect("WriteGuard RwLock poisoned")
            .contains(&canonical_key(path))
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
