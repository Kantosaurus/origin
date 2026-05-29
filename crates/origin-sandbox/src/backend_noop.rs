// SPDX-License-Identifier: Apache-2.0
//! No-op backend; logs at `tracing::warn!` so operators spot accidental opt-out.

use crate::{SandboxError, SandboxProfile};

/// Drop-through implementation that mutates nothing. Logs a warning if the
/// caller asked for a non-`Inherit` profile, since that almost certainly
/// indicates a missing cargo feature.
///
/// # Errors
/// Never returns an error in the current implementation.
pub fn apply(profile: SandboxProfile, _cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    if profile != SandboxProfile::Inherit {
        tracing::warn!(
            target: "origin.sandbox",
            requested = ?profile,
            "no sandbox backend compiled in; profile dropped"
        );
    }
    Ok(())
}
