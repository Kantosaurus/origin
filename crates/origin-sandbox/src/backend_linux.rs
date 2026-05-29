// SPDX-License-Identifier: Apache-2.0
//! Linux backend: landlock + seccomp BPF + rlimit (CPU/RAM caps).
//!
//! The filter is constructed in the parent and installed inside the forked
//! child's `pre_exec` hook so we never poison the daemon's own thread.

#![cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;

use landlock::{
    Access, AccessFs, BitFlags, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI as LL_ABI,
};
use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use crate::{SandboxError, SandboxProfile};

#[cfg(target_arch = "x86_64")]
const NATIVE_ARCH: TargetArch = TargetArch::x86_64;
#[cfg(target_arch = "aarch64")]
const NATIVE_ARCH: TargetArch = TargetArch::aarch64;

/// Install the requested sandbox profile on `cmd`. The landlock ruleset is
/// applied in the forked child between `clone()` and `execve()`; the seccomp
/// filter is loaded in the same window.
///
/// # Errors
/// Returns [`SandboxError::Apply`] if landlock/seccomp construction fails in
/// the parent. Failures inside `pre_exec` propagate as `std::io::Error` from
/// `Command::spawn`.
pub fn apply(profile: SandboxProfile, cmd: &mut Command) -> Result<(), SandboxError> {
    let policy = Arc::new(LinuxPolicy::for_profile(profile)?);

    let policy_for_hook = Arc::clone(&policy);
    // SAFETY: `pre_exec` runs in the forked child between clone() and execve.
    // The closure only touches async-signal-safe APIs (landlock ioctls,
    // seccomp(2)) and an `Arc` clone made in the parent.
    unsafe {
        cmd.pre_exec(move || policy_for_hook.install().map_err(std::io::Error::other));
    }
    crate::caps::apply_caps(cmd)?;
    Ok(())
}

struct LinuxPolicy {
    landlock: Vec<PathRule>,
    seccomp: BpfProgram,
}

struct PathRule {
    path: std::path::PathBuf,
    access: BitFlags<AccessFs>,
}

impl LinuxPolicy {
    fn for_profile(profile: SandboxProfile) -> Result<Self, SandboxError> {
        let cwd = std::env::current_dir().map_err(SandboxError::Io)?;
        let mut rules: Vec<PathRule> = Vec::with_capacity(6);

        let ro: BitFlags<AccessFs> = AccessFs::from_read(LL_ABI::V4);
        let rw: BitFlags<AccessFs> = AccessFs::from_all(LL_ABI::V4);

        match profile {
            SandboxProfile::Inherit => {
                return Ok(Self {
                    landlock: vec![],
                    seccomp: empty_filter()?,
                })
            }
            SandboxProfile::ReadFs => {
                rules.push(PathRule {
                    path: cwd.clone(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/usr/lib".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/lib".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/etc/ssl/certs".into(),
                    access: ro,
                });
            }
            SandboxProfile::WriteCwd => {
                rules.push(PathRule {
                    path: cwd.clone(),
                    access: rw,
                });
                rules.push(PathRule {
                    path: "/usr/lib".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/lib".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/etc/ssl/certs".into(),
                    access: ro,
                });
            }
            SandboxProfile::Shell => {
                rules.push(PathRule {
                    path: cwd.clone(),
                    access: rw,
                });
                rules.push(PathRule {
                    path: "/usr".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/bin".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/lib".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/etc".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/tmp".into(),
                    access: rw,
                });
            }
            SandboxProfile::Network => {
                rules.push(PathRule {
                    path: cwd.clone(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/etc/ssl/certs".into(),
                    access: ro,
                });
                rules.push(PathRule {
                    path: "/etc/resolv.conf".into(),
                    access: ro,
                });
            }
        }

        let seccomp = match profile {
            SandboxProfile::Network => network_allow_filter()?,
            SandboxProfile::Inherit => empty_filter()?,
            _ => deny_network_filter()?,
        };

        Ok(Self {
            landlock: rules,
            seccomp,
        })
    }

    fn install(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.landlock.is_empty() {
            let mut rs = Ruleset::default()
                .handle_access(AccessFs::from_all(LL_ABI::V4))?
                .create()?;
            for rule in &self.landlock {
                let fd = PathFd::new(&rule.path)?;
                rs = rs.add_rule(PathBeneath::new(fd, rule.access))?;
            }
            rs.restrict_self()?;
        }
        seccompiler::apply_filter(&self.seccomp)?;
        Ok(())
    }
}

fn empty_filter() -> Result<BpfProgram, SandboxError> {
    let filter = SeccompFilter::new(
        std::collections::BTreeMap::new(),
        SeccompAction::Allow,
        SeccompAction::Allow,
        NATIVE_ARCH,
    )
    .map_err(|e| SandboxError::Apply(e.to_string()))?;
    filter
        .try_into()
        .map_err(|e: BackendError| SandboxError::Apply(e.to_string()))
}

fn deny_network_filter() -> Result<BpfProgram, SandboxError> {
    use std::collections::BTreeMap;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for fam in [2_u64 /* AF_INET */, 10_u64 /* AF_INET6 */] {
        let rule = SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            fam,
        )
        .map_err(|e| SandboxError::Apply(e.to_string()))?])
        .map_err(|e| SandboxError::Apply(e.to_string()))?;
        rules.entry(libc::SYS_socket).or_default().push(rule);
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        NATIVE_ARCH,
    )
    .map_err(|e| SandboxError::Apply(e.to_string()))?;
    filter
        .try_into()
        .map_err(|e: BackendError| SandboxError::Apply(e.to_string()))
}

fn network_allow_filter() -> Result<BpfProgram, SandboxError> {
    use std::collections::BTreeMap;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for sys in [libc::SYS_listen, libc::SYS_accept, libc::SYS_accept4] {
        rules.insert(sys, vec![]);
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        NATIVE_ARCH,
    )
    .map_err(|e| SandboxError::Apply(e.to_string()))?;
    filter
        .try_into()
        .map_err(|e: BackendError| SandboxError::Apply(e.to_string()))
}
