//! macOS backend: `sandbox-exec` profile.
//!
//! Filled in by P11.3. Placeholder skeleton so `cargo fmt`/`cargo check` can
//! resolve the module even though the per-OS feature is off on most hosts.

#![cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]

use crate::{SandboxError, SandboxProfile};

/// Placeholder macOS apply entry — replaced with the `sandbox-exec` wrapper in
/// P11.3.
///
/// # Errors
/// Returns [`SandboxError::Unavailable`] until the real implementation lands.
pub fn apply(_profile: SandboxProfile, _cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    Err(SandboxError::Unavailable("macos"))
}
