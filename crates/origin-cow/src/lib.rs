// SPDX-License-Identifier: Apache-2.0
//! Workspace copy-on-write clones with a platform-reflink fast path and a
//! cross-platform eager-copy fallback.
//!
//! # Overview
//!
//! `origin-cow` exposes a single primitive — [`Workspace`] — that lets
//! callers take an isolated **clone** of an existing directory tree. The
//! clone is guaranteed to satisfy the *isolation contract*:
//!
//! > Writes performed via the clone path are not observable from the
//! > parent path, and writes performed via the parent path after the
//! > clone is taken are not observable from the clone path.
//!
//! On filesystems that support copy-on-write at the block layer
//! (Btrfs / XFS-cow / APFS / `ReFS`) the implementation prefers a reflink
//! fast path: only the metadata block is duplicated; data extents are
//! shared until one side writes. On every other filesystem the
//! [`HardlinkOverlay`](Strategy::HardlinkOverlay) strategy is used,
//! which currently performs an eager byte-for-byte copy. The name
//! reflects the long-term plan (Phase 11) of layering a hardlink farm
//! over a backing CAS pack; for Phase 9 the eager-copy implementation
//! is correct on every filesystem the daemon is run on, including the
//! Windows NTFS host this crate ships with.
//!
//! # Public surface
//!
//! - [`Workspace::open`] — wrap an existing directory; infallible.
//! - [`Workspace::clone_into`] — take an isolated clone at a new path.
//! - [`Workspace::path`] — borrow the workspace root.
//! - [`Workspace::strategy`] — strategy actually used for this workspace.
//! - [`Strategy`] — enum of available clone strategies.
//! - [`Error`] — top-level error type.

#![doc(html_root_url = "https://docs.rs/origin-cow/0.0.1")]

mod hardlink_fallback;
mod strategy;

#[cfg(target_os = "linux")]
mod reflink_linux;
#[cfg(target_os = "macos")]
mod reflink_macos;
// Public + hidden so the Phase 11 ReFS integration test in
// `tests/windows_refs_reflink.rs` can call `reflink_tree` directly when
// `ORIGIN_REFS_TEST_DIR` is set to a ReFS / Dev Drive volume. Documented as
// internal so the surface area users see remains `Workspace`.
#[cfg(target_os = "windows")]
#[doc(hidden)]
pub mod reflink_windows;

use std::path::{Path, PathBuf};

pub use strategy::Strategy;

/// Errors returned by [`Workspace`] operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O error from the host filesystem.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The reflink fast path is not supported on this filesystem.
    ///
    /// Callers never see this error today: [`Workspace::clone_into`]
    /// silently downgrades to [`Strategy::HardlinkOverlay`] when reflink
    /// fails. It is retained as a public variant for downstream tooling
    /// that wants to introspect the failure reason.
    #[error("reflink not supported on this filesystem: {0}")]
    Unsupported(String),
}

/// Isolated view over a workspace directory tree.
///
/// `Workspace` is intentionally lightweight: it stores a path and a
/// [`Strategy`] discriminant. All filesystem I/O happens in
/// [`Workspace::clone_into`].
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    strategy: Strategy,
}

impl Workspace {
    /// Wrap an existing directory tree as a workspace.
    ///
    /// This call is **infallible** by design: it does not stat `root`
    /// nor probe the filesystem. Strategy detection is deferred to
    /// [`clone_into`](Self::clone_into) so that opening a workspace is
    /// free and so that the in-memory `Strategy` reported by an
    /// un-cloned workspace reflects the *intended* strategy rather than
    /// a fuzzy probe result.
    #[must_use]
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            strategy: strategy::detect(),
        }
    }

    /// Borrow the workspace root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Clone strategy this workspace will use (or used) for `clone_into`.
    ///
    /// For a workspace returned by [`open`](Self::open) this is the
    /// strategy the host platform *prefers*. For a workspace returned
    /// by [`clone_into`](Self::clone_into) it is the strategy that was
    /// actually used — which may be [`Strategy::HardlinkOverlay`] even
    /// when the host prefers [`Strategy::Reflink`], if reflink failed
    /// at runtime and the eager-copy fallback was triggered.
    #[must_use]
    pub const fn strategy(&self) -> Strategy {
        self.strategy
    }

    /// Clone `self` into `dest`, returning a new isolated [`Workspace`].
    ///
    /// The destination's parent directory chain is created as needed.
    /// On strategies that support it the data is reflinked; on every
    /// other filesystem the implementation falls back to eager copy.
    /// In either case the returned `Workspace` satisfies the isolation
    /// contract described at the crate level.
    ///
    /// # Errors
    /// Returns [`Error::Io`] when the underlying filesystem rejects a
    /// read, write, mkdir, or directory walk. Reflink-specific errors
    /// are not surfaced — they trigger an internal fallback to eager
    /// copy.
    pub fn clone_into(&self, dest: impl AsRef<Path>) -> Result<Self, Error> {
        let dest = dest.as_ref();
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let strategy = match self.strategy {
            Strategy::Reflink => {
                if try_reflink_tree(&self.root, dest).is_ok() {
                    Strategy::Reflink
                } else {
                    // Best-effort cleanup of any partial reflink output.
                    let _ = std::fs::remove_dir_all(dest);
                    hardlink_fallback::eager_copy_tree(&self.root, dest)?;
                    Strategy::HardlinkOverlay
                }
            }
            Strategy::HardlinkOverlay => {
                hardlink_fallback::eager_copy_tree(&self.root, dest)?;
                Strategy::HardlinkOverlay
            }
        };

        Ok(Self {
            root: dest.to_path_buf(),
            strategy,
        })
    }
}

/// Try the platform reflink path. On every platform we currently
/// short-circuit to `Err(Unsupported)` for any per-file failure so that
/// [`Workspace::clone_into`] consistently falls back to eager copy.
///
/// Linux / macOS / Windows have working scaffolding in
/// `reflink_{linux,macos,windows}.rs`; in Phase 9 only the eager-copy
/// fallback is exercised by the test suite. Phase 11 will harden each
/// fast path against real Btrfs / APFS / `ReFS` volumes.
#[allow(
    unused_variables,
    clippy::needless_pass_by_value,
    clippy::missing_const_for_fn
)]
fn try_reflink_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        reflink_linux::reflink_tree(src, dst)
    }
    #[cfg(target_os = "macos")]
    {
        reflink_macos::reflink_tree(src, dst)
    }
    #[cfg(target_os = "windows")]
    {
        reflink_windows::reflink_tree(src, dst)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(Error::Unsupported(format!(
            "no reflink driver for target_os; src={}, dst={}",
            src.display(),
            dst.display()
        )))
    }
}
