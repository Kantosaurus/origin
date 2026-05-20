//! Per-platform CPU/RAM cap helpers. Linux + macOS bodies use `setrlimit` via
//! `pre_exec`; Windows quotas are attached via the Job Object in
//! `backend_windows.rs`. Non-supported targets get a no-op so the crate still
//! compiles on every host.

use crate::SandboxError;

#[cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]
/// Apply `RLIMIT_CPU` (60 s) + `RLIMIT_AS` (1 GiB) to the spawned child.
///
/// # Errors
/// Returns [`SandboxError::Apply`] if the underlying `setrlimit` fails.
pub fn apply_caps(cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the forked child between clone() and execve.
    // We touch only async-signal-safe APIs (`setrlimit`).
    unsafe {
        cmd.pre_exec(|| {
            let cpu = libc::rlimit {
                rlim_cur: 60,
                rlim_max: 60,
            };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mem = libc::rlimit {
                rlim_cur: 1 << 30,
                rlim_max: 1 << 30,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &mem) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]
/// Apply `RLIMIT_CPU` (60 s) + `RLIMIT_AS` (1 GiB) to the spawned child.
///
/// # Errors
/// Returns [`SandboxError::Apply`] if the underlying `setrlimit` fails.
pub fn apply_caps(cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` runs in the forked child between fork() and execve.
    // `setrlimit` is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            let cpu = libc::rlimit {
                rlim_cur: 60,
                rlim_max: 60,
            };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mem = libc::rlimit {
                rlim_cur: 1 << 30,
                rlim_max: 1 << 30,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &mem) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(any(
    all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")),
    all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")),
)))]
/// No-op cap helper for non-supported targets. Windows uses Job Object quotas
/// installed in [`crate::backend_windows`] instead.
///
/// # Errors
/// Never returns an error in the current implementation.
pub fn apply_caps(_cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    Ok(())
}
