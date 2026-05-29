// SPDX-License-Identifier: Apache-2.0
//! macOS backend: `sandbox-exec` profile.
//!
//! Renders an SBPL profile string per profile variant and re-points the
//! `Command` at `/usr/bin/sandbox-exec` with the original argv passed after
//! `--`. CPU/RAM caps are layered on via `crate::caps::apply_caps` (`setrlimit`).

#![cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]

use std::ffi::OsString;
use std::process::Command;

use crate::{SandboxError, SandboxProfile};

/// Wrap `cmd` in `/usr/bin/sandbox-exec -p <profile> -- <orig argv>` and
/// install `setrlimit` caps on the resulting child.
///
/// # Errors
/// Returns [`SandboxError::Io`] if the cwd cannot be resolved.
pub fn apply(profile: SandboxProfile, cmd: &mut Command) -> Result<(), SandboxError> {
    if profile == SandboxProfile::Inherit {
        return crate::caps::apply_caps(cmd);
    }
    let profile_text = render_profile(profile)?;

    let orig_program: OsString = cmd.get_program().to_owned();
    let orig_args: Vec<OsString> = cmd.get_args().map(ToOwned::to_owned).collect();

    *cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(&profile_text).arg("--").arg(orig_program);
    for a in orig_args {
        cmd.arg(a);
    }
    crate::caps::apply_caps(cmd)?;
    Ok(())
}

fn render_profile(profile: SandboxProfile) -> Result<String, SandboxError> {
    let cwd = std::env::current_dir().map_err(SandboxError::Io)?;
    let cwd_str = cwd.to_string_lossy();
    let body = match profile {
        SandboxProfile::Inherit => unreachable!("handled by caller"),
        SandboxProfile::ReadFs => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n\
             (allow file-read*)\n(deny file-write*)\n\
             (allow file-read* (subpath \"{cwd_str}\"))\n(deny network*)\n"
        ),
        SandboxProfile::WriteCwd => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n\
             (allow file-read*)\n(allow file-write* (subpath \"{cwd_str}\"))\n\
             (deny file-write* (subpath \"/etc\"))\n(deny network*)\n"
        ),
        SandboxProfile::Shell => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n\
             (allow file-read*)\n(allow file-write* (subpath \"{cwd_str}\"))\n\
             (allow file-write* (subpath \"/tmp\"))\n(deny network*)\n(allow sysctl-read)\n"
        ),
        SandboxProfile::Network => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n\
             (allow file-read*)\n(deny file-write*)\n\
             (allow file-read* (subpath \"{cwd_str}\"))\n\
             (allow network-outbound (remote tcp))\n(allow system-socket)\n"
        ),
    };
    Ok(body)
}
