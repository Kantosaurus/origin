//! Windows backend: AppContainer SID + restricted Job Object.
//!
//! Filled in by P11.4. Placeholder skeleton so `cargo fmt`/`cargo check` can
//! resolve the module even though the per-OS feature is off on most hosts.

#![cfg(all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")))]

use std::process::Child;

use crate::{SandboxError, SandboxProfile};

/// Placeholder Windows apply entry — replaced with the AppContainer/Job Object
/// pipeline in P11.4.
///
/// # Errors
/// Returns [`SandboxError::Unavailable`] until the real implementation lands.
pub fn apply(_profile: SandboxProfile, _cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    Err(SandboxError::Unavailable("windows"))
}

/// Placeholder for the post-spawn Job Object attach. Replaced in P11.4.
///
/// # Errors
/// Returns [`SandboxError::Unavailable`] until the real implementation lands.
pub fn attach_job_object_if_needed(_child: &mut Child) -> Result<(), SandboxError> {
    Err(SandboxError::Unavailable("windows"))
}
