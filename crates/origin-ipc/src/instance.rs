// SPDX-License-Identifier: Apache-2.0
//! Per-project daemon instance identity.
//!
//! Historically every `origin` process rendezvoused on ONE global IPC path
//! (`\\.\pipe\origin` / `$TMPDIR/origin.sock`), so all terminals shared a
//! single daemon whose cwd was whichever project spawned it first — and a
//! newer-binary restart in one terminal `taskkill`ed every other project's
//! daemon by image name.
//!
//! An [`InstanceId`] scopes the rendezvous to a workspace root: the IPC
//! path, the session DB, the CAS root, and the spawn stamp/pid files all
//! derive from a stable hash of the canonicalized workspace directory, so
//! `origin` launched in n different projects yields n independent daemons
//! that never interfere. Launching twice from the SAME directory reuses
//! that directory's daemon (the id is deterministic).
//!
//! `ORIGIN_SOCK` still overrides the IPC path entirely (shared/global daemon,
//! remote tunnels, tests), in which case per-instance scoping is bypassed.

use std::path::{Path, PathBuf};

/// FNV-1a 64-bit — tiny, dependency-free, stable across platforms and
/// releases. Not cryptographic; it only needs to spread workspace paths
/// across distinct pipe names with negligible collision odds.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// A stable per-workspace identity: 16 lowercase hex chars derived from the
/// canonicalized workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceId {
    /// The canonicalized workspace root the id was derived from.
    pub workspace: PathBuf,
    hex: String,
}

impl InstanceId {
    /// Derive the instance id for `dir`.
    ///
    /// Canonicalizes first so `C:\proj`, `c:\proj\.` and a symlinked alias
    /// all map to the same daemon. Falls back to the path as-given when
    /// canonicalization fails (e.g. the directory vanished); the id is then
    /// still deterministic for that spelling.
    #[must_use]
    pub fn for_dir(dir: &Path) -> Self {
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        // Case-insensitive filesystems (Windows, default macOS): hash the
        // lowercased UTF-8 form so `C:\Proj` and `c:\proj` agree. Non-UTF-8
        // paths hash their raw lossy form.
        let norm = canon.to_string_lossy().to_lowercase();
        let hex = format!("{:016x}", fnv1a(norm.as_bytes()));
        Self { workspace: canon, hex }
    }

    /// Derive the instance id for the current working directory.
    #[must_use]
    pub fn for_cwd() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::for_dir(&cwd)
    }

    /// The 16-hex-char instance tag.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// The per-instance IPC rendezvous path (named pipe / Unix socket).
    #[must_use]
    pub fn ipc_path(&self) -> String {
        #[cfg(windows)]
        {
            format!(r"\\.\pipe\origin-{}", self.hex)
        }
        #[cfg(unix)]
        {
            format!(
                "{}/origin-{}.sock",
                std::env::temp_dir().display(),
                self.hex
            )
        }
    }

    /// The per-instance session DB path.
    #[must_use]
    pub fn db_path(&self) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("origin-{}.db", self.hex));
        p.to_string_lossy().into_owned()
    }

    /// The per-instance CAS root.
    #[must_use]
    pub fn cas_root(&self) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("origin-cas-{}", self.hex));
        p.to_string_lossy().into_owned()
    }

    /// Directory holding this instance's spawn-control files
    /// (`<home>/.origin/daemons`). `None` when no home dir is resolvable.
    #[must_use]
    pub fn control_dir(home: Option<PathBuf>) -> Option<PathBuf> {
        home.map(|h| h.join(".origin").join("daemons"))
    }

    /// Path of the stamp file recording when this instance's daemon was last
    /// spawned (mtime comparison drives newer-binary restarts).
    #[must_use]
    pub fn stamp_path(&self, home: Option<PathBuf>) -> Option<PathBuf> {
        Self::control_dir(home).map(|d| d.join(format!("{}.stamp", self.hex)))
    }

    /// Path of the pid file recording the daemon/supervisor process ids spawned
    /// for this instance, so a restart kills exactly those processes and never
    /// another project's daemon.
    #[must_use]
    pub fn pid_path(&self, home: Option<PathBuf>) -> Option<PathBuf> {
        Self::control_dir(home).map(|d| d.join(format!("{}.pid", self.hex)))
    }
}

/// Resolve the IPC path honoring the `ORIGIN_SOCK` override.
///
/// - `ORIGIN_SOCK` set ⇒ that exact path (shared daemon / tests / tunnels).
/// - Otherwise ⇒ the per-instance path for the current working directory.
#[must_use]
pub fn resolve_ipc_path() -> String {
    std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| InstanceId::for_cwd().ipc_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_dir_same_id() {
        let a = InstanceId::for_dir(Path::new("."));
        let b = InstanceId::for_dir(Path::new("."));
        assert_eq!(a, b);
        assert_eq!(a.ipc_path(), b.ipc_path());
    }

    #[test]
    fn different_dirs_different_ids() {
        // Use two directories that always exist.
        let a = InstanceId::for_dir(&std::env::temp_dir());
        let b = InstanceId::for_dir(Path::new("."));
        assert_ne!(a.hex(), b.hex(), "distinct dirs must yield distinct ids");
        assert_ne!(a.ipc_path(), b.ipc_path());
        assert_ne!(a.db_path(), b.db_path());
        assert_ne!(a.cas_root(), b.cas_root());
    }

    #[test]
    fn id_is_16_hex_chars() {
        let id = InstanceId::for_cwd();
        assert_eq!(id.hex().len(), 16);
        assert!(id.hex().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn dot_and_absolute_cwd_agree() {
        // `.` canonicalizes to the cwd, so both spellings map to one daemon.
        let dot = InstanceId::for_dir(Path::new("."));
        let abs = InstanceId::for_cwd();
        assert_eq!(dot, abs);
    }

    #[test]
    fn ipc_path_embeds_instance_tag() {
        let id = InstanceId::for_cwd();
        assert!(id.ipc_path().contains(id.hex()));
    }

    #[test]
    fn control_paths_are_scoped_per_instance() {
        let home = Some(PathBuf::from("/home/u"));
        let a = InstanceId::for_dir(&std::env::temp_dir());
        let b = InstanceId::for_dir(Path::new("."));
        assert_ne!(a.stamp_path(home.clone()), b.stamp_path(home.clone()));
        assert_ne!(a.pid_path(home.clone()), b.pid_path(home));
        assert!(a
            .stamp_path(Some(PathBuf::from("/home/u")))
            .is_some_and(|p| p.to_string_lossy().contains("daemons")));
    }

    #[test]
    fn no_home_yields_no_control_paths() {
        let id = InstanceId::for_cwd();
        assert_eq!(id.stamp_path(None), None);
        assert_eq!(id.pid_path(None), None);
    }

    #[test]
    fn nonexistent_dir_still_deterministic() {
        let p = Path::new("Z:/definitely/not/a/real/dir/origin-test");
        let a = InstanceId::for_dir(p);
        let b = InstanceId::for_dir(p);
        assert_eq!(a, b);
    }

    #[test]
    fn case_insensitive_normalization() {
        // On case-insensitive filesystems both spellings canonicalize the
        // same; even when canonicalization fails the lowercase fold makes
        // them agree.
        let a = InstanceId::for_dir(Path::new("Z:/Fake/Dir"));
        let b = InstanceId::for_dir(Path::new("z:/fake/dir"));
        assert_eq!(a.hex(), b.hex());
    }
}
