//! Linux backend: landlock + seccomp BPF + rlimit (CPU/RAM caps).
//!
//! Filled in by P11.2. Placeholder skeleton so `cargo fmt`/`cargo check` can
//! resolve the module even though the per-OS feature is off on most hosts.

#![cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]

use crate::{SandboxError, SandboxProfile};

/// Placeholder Linux apply entry — will be replaced with the landlock/seccomp
/// pipeline in P11.2.
///
/// # Errors
/// Returns [`SandboxError::Unavailable`] until the real implementation lands.
pub fn apply(_profile: SandboxProfile, _cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    Err(SandboxError::Unavailable("linux"))
}
