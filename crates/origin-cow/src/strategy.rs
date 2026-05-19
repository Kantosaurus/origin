//! Clone-strategy discriminant and host-preference detection.
//!
//! Strategy detection is deliberately coarse: we report the host's
//! *preferred* strategy based purely on `target_os`. Filesystems that
//! advertise `CoW` support but reject the actual reflink ioctl at
//! runtime are handled by the fallback path in `lib.rs::clone_into`,
//! not here.

/// Workspace clone strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Reflink (block-level copy-on-write).
    ///
    /// Linux Btrfs / XFS-cow via `FICLONE`; macOS APFS via `clonefile`;
    /// Windows `ReFS` via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`. The fast
    /// path is opportunistic — runtime failures cause the
    /// [`Workspace::clone_into`](crate::Workspace::clone_into) call to
    /// silently fall back to [`Strategy::HardlinkOverlay`].
    Reflink,
    /// Eager byte-for-byte copy (Phase 9 implementation).
    ///
    /// Named "`HardlinkOverlay`" to reflect the Phase 11 design — a
    /// hardlink farm layered over a backing CAS pack — but currently
    /// implemented as plain `fs::copy` recursion. Both implementations
    /// satisfy the isolation contract.
    HardlinkOverlay,
}

/// Host-preferred default strategy.
#[must_use]
pub const fn detect() -> Strategy {
    // Until per-filesystem probing lands in Phase 11 we conservatively
    // default to the cross-platform fallback. The `clone_into`
    // implementation still calls into the reflink driver on supported
    // OSes; this just controls what `Workspace::strategy()` reports
    // for an un-cloned workspace.
    Strategy::HardlinkOverlay
}
