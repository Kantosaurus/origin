//! `origin-sandbox` — per-tool sandbox profiles for Linux, macOS, and Windows.
//!
//! The crate exposes a single [`apply`] entry point that mutates a
//! [`std::process::Command`] so the resulting child runs under the requested
//! [`SandboxProfile`]. Per-OS backends are gated behind `linux`/`macos`/`windows`
//! cargo features; the [`backend_noop`] backend is used otherwise (or when the
//! `no-sandbox` escape hatch is enabled for CI matrices).
//!
//! Hot-path discipline: the profile selector is a stable `u8` ordinal so
//! dispatch never touches a string table (cf. jcode's YAML lookup).

pub mod backend_noop;
pub mod caps;
pub mod profile;

#[cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]
pub mod backend_linux;
#[cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]
pub mod backend_macos;
#[cfg(all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")))]
pub mod backend_windows;

pub use profile::{ProfileOrdinal, SandboxProfile};

use thiserror::Error;

/// Errors raised by the per-OS backends.
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("backend `{0}` not available on this host")]
    Unavailable(&'static str),
    #[error("apply: {0}")]
    Apply(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Mutate `cmd` to enforce `profile` on the spawned child.
///
/// On unsupported hosts (or when no backend feature is active) this falls back
/// to [`backend_noop::apply`], which logs at `tracing::warn!` so operators
/// notice accidental opt-out.
///
/// # Errors
/// Returns [`SandboxError`] when the OS rejects the policy or a backend is
/// not available.
pub fn apply(profile: SandboxProfile, cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    #[cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]
    {
        return backend_linux::apply(profile, cmd);
    }
    #[cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]
    {
        return backend_macos::apply(profile, cmd);
    }
    #[cfg(all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")))]
    {
        return backend_windows::apply(profile, cmd);
    }
    #[cfg_attr(
        any(
            all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")),
            all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")),
            all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")),
        ),
        allow(unreachable_code)
    )]
    {
        backend_noop::apply(profile, cmd)
    }
}
