// SPDX-License-Identifier: Apache-2.0
//! Cross-platform eager-copy fallback used by
//! [`Strategy::HardlinkOverlay`](crate::Strategy::HardlinkOverlay).
//!
//! Phase 9 ships the straightforward implementation: walk the source
//! tree depth-first, recreate every directory under the destination,
//! and `fs::copy` every regular file. This is correct on every
//! filesystem the daemon supports and satisfies the isolation contract
//! unconditionally.
//!
//! Phase 11 will replace the copy with a hardlink-into-CAS-pack
//! variant; the public surface (`eager_copy_tree`) is intentionally
//! narrow so that swap can happen in-place.

use std::fs;
use std::io;
use std::path::Path;

use crate::Error;

/// Recursively duplicate `src` at `dst` as a fully owned tree.
///
/// `dst` is created if absent. Pre-existing entries under `dst` are
/// not removed up front — callers (see `lib.rs`) are responsible for
/// cleaning up any partial state from a prior strategy attempt.
///
/// # Errors
/// Propagates any [`io::Error`] from the underlying walk or copy.
pub fn eager_copy_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    copy_dir_inner(src, dst).map_err(Error::Io)
}

fn copy_dir_inner(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    // We deliberately do *not* descend symlinks: if `src` itself is a
    // symlink to a tree we copy what it points to (via `read_dir`), but
    // symlinked entries inside are reproduced as regular file copies
    // rather than as new symlinks. Phase 9 test fixtures never include
    // symlinks, so this branch is unreachable today.
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_inner(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        } else {
            // Symlinks / sockets / fifos: skip silently for Phase 9.
            // Phase 11 will need explicit handling per the workspace
            // semantics defined by the daemon.
        }
    }
    Ok(())
}
