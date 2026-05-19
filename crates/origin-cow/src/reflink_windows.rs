//! Windows reflink driver — stubbed pending Phase 11.
//!
//! Windows reflinks are supported only on `ReFS` (and, increasingly,
//! Dev Drive volumes) via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`. The
//! ioctl is non-trivial: it requires sparse-aware destination files,
//! 4 KiB block alignment, explicit ranges per call, and a handle
//! opened with `FILE_WRITE_DATA`. Wiring it up correctly is in scope
//! for **Phase 11** (`p11-complete`).
//!
//! For Phase 9 we always return [`Error::Unsupported`] so that
//! [`Workspace::clone_into`](crate::Workspace::clone_into) falls
//! through to the cross-platform eager-copy path defined in
//! `hardlink_fallback.rs`. That path satisfies the isolation contract
//! on NTFS (the default Windows filesystem) and on every other host
//! filesystem.
//!
//! This file is only compiled on `target_os = "windows"`.

use std::path::Path;

use crate::Error;

/// Always returns [`Error::Unsupported`] in Phase 9.
///
/// # Errors
/// Always returns [`Error::Unsupported`]; see module docs.
#[allow(clippy::unnecessary_wraps)] // signature mirrors the Linux/macOS drivers.
pub fn reflink_tree(_src: &Path, _dst: &Path) -> Result<(), Error> {
    Err(Error::Unsupported(
        "Windows reflink (FSCTL_DUPLICATE_EXTENTS_TO_FILE) is a Phase 11 follow-up; \
         falling back to eager copy"
            .to_string(),
    ))
}
