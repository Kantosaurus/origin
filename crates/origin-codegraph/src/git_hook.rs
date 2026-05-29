// SPDX-License-Identifier: Apache-2.0
//! Minimal `post-commit` hook installer.
//!
//! P10 will generalize hooks; P7.8 installs a one-shot script that calls the
//! `origin` daemon via the existing IPC channel. Unix → `post-commit`;
//! Windows → `post-commit.cmd` (Git for Windows runs `.cmd` hooks).

use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
const HOOK_BODY: &str = "#!/bin/sh\n\
                         # origin post-commit hook (Phase 7, P7.8). Generalized in P10.\n\
                         exec origin rebuild-codegraph --changed-only \"$@\"\n";

#[cfg(windows)]
const HOOK_BODY: &str = "@echo off\r\n\
                         REM origin post-commit hook (Phase 7, P7.8). Generalized in P10.\r\n\
                         origin rebuild-codegraph --changed-only %*\r\n";

/// Install the hook into `<repo>/.git/hooks/post-commit` (`.cmd` on Windows).
///
/// # Errors
/// Returns I/O errors from creating the hooks directory, writing the hook
/// file, or (on Unix) updating its file mode to be executable.
pub fn install_post_commit(repo: &Path) -> io::Result<()> {
    let hooks = repo.join(".git").join("hooks");
    fs::create_dir_all(&hooks)?;
    let name = if cfg!(windows) {
        "post-commit.cmd"
    } else {
        "post-commit"
    };
    let path = hooks.join(name);
    fs::write(&path, HOOK_BODY)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    Ok(())
}
