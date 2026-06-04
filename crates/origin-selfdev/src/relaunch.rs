// SPDX-License-Identifier: Apache-2.0
//! Relaunch manifest: the binary-swap contract the daemon hands to the supervisor.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::StoreError;

/// The on-disk contract the daemon writes after a green build+test so the
/// supervisor can swap binaries and relaunch.
#[allow(clippy::module_name_repetitions)] // `RelaunchManifest` is the documented public type re-exported at the crate root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaunchManifest {
    /// Absolute path to the freshly built binary the supervisor should exec.
    pub new_binary_path: PathBuf,
    /// Absolute path to the binary currently running (kept for rollback).
    pub previous_binary_path: PathBuf,
    /// Monotonic successor counter; mirrors `ReloadContext::generation`.
    pub generation: u64,
}

impl RelaunchManifest {
    /// Construct a manifest for swapping `previous_binary_path` -> `new_binary_path`.
    #[must_use]
    pub fn new(
        new_binary_path: impl Into<PathBuf>,
        previous_binary_path: impl Into<PathBuf>,
        generation: u64,
    ) -> Self {
        Self {
            new_binary_path: new_binary_path.into(),
            previous_binary_path: previous_binary_path.into(),
            generation,
        }
    }
}

/// A request to relaunch into a freshly built binary, recorded by the
/// driver/`RestartAuthority` side at restart-grant time.
///
/// This is the pure, IO-free counterpart of [`RelaunchManifest`]: the daemon
/// constructs it when a build+test pass and a restart is granted, carrying the
/// new binary path (and the binary currently running). Stamping it with the
/// `ReloadContext::generation` via [`RelaunchRequest::into_manifest`] (or
/// persisting it via [`RelaunchRequest::record`]) produces the manifest the
/// supervisor consumes. Keeping the generation out of the request lets the same
/// request be stamped with whichever generation the granted restart carries.
#[allow(clippy::module_name_repetitions)] // `RelaunchRequest` is the documented public type re-exported at the crate root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchRequest {
    /// Absolute path to the freshly built binary to exec.
    pub new_binary_path: PathBuf,
    /// Absolute path to the binary currently running (kept for rollback).
    pub previous_binary_path: PathBuf,
}

impl RelaunchRequest {
    /// Record a relaunch from `previous_binary_path` to `new_binary_path`.
    #[must_use]
    pub fn new(
        new_binary_path: impl Into<PathBuf>,
        previous_binary_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            new_binary_path: new_binary_path.into(),
            previous_binary_path: previous_binary_path.into(),
        }
    }

    /// Stamp this request with `generation` (the granted restart's generation,
    /// mirroring `ReloadContext::generation`) to produce the manifest. Pure.
    #[must_use]
    pub fn into_manifest(self, generation: u64) -> RelaunchManifest {
        RelaunchManifest {
            new_binary_path: self.new_binary_path,
            previous_binary_path: self.previous_binary_path,
            generation,
        }
    }

    /// Stamp this request with `generation` and persist it via the injected
    /// `store`, returning the manifest that was written. IO is injectable so the
    /// daemon supplies a [`FileRelaunchStore`] while tests use a fake.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the underlying store save fails.
    pub fn record(
        self,
        store: &dyn RelaunchStore,
        generation: u64,
    ) -> Result<RelaunchManifest, StoreError> {
        let manifest = self.into_manifest(generation);
        store.save(&manifest)?;
        Ok(manifest)
    }
}

/// Persistence boundary for a [`RelaunchManifest`].
///
/// Injected so the daemon can persist the manifest across a restart while tests
/// use an in-memory fake. Implementors own all I/O. Mirrors
/// [`crate::ReloadStore`] but is intentionally a *separate* slot from
/// `reload.json` — the manifest is the binary-swap contract, the reload context
/// is the orchestration-resume state.
#[allow(clippy::module_name_repetitions)] // `RelaunchStore` is the documented public type re-exported at the crate root.
pub trait RelaunchStore {
    /// Persist `manifest`, replacing any previously stored manifest.
    ///
    /// # Errors
    /// Returns [`StoreError`] on I/O or serialization failure.
    fn save(&self, manifest: &RelaunchManifest) -> Result<(), StoreError>;

    /// Load the stored manifest, or `None` if none has been saved.
    ///
    /// # Errors
    /// Returns [`StoreError`] on I/O or deserialization failure.
    fn load(&self) -> Result<Option<RelaunchManifest>, StoreError>;

    /// Remove any stored manifest (once the supervisor has consumed it).
    ///
    /// # Errors
    /// Returns [`StoreError`] on I/O failure other than "already absent".
    fn clear(&self) -> Result<(), StoreError>;
}

/// A JSON-file-backed [`RelaunchStore`].
///
/// The default location is `data_local_dir()/origin/selfdev/relaunch.json`, but
/// the path is injectable (the daemon, which owns the `dirs` dependency, supplies
/// the resolved path; tests use a tempdir). This file is the contract the
/// supervisor reads WITHOUT depending on this crate, so the field names in
/// [`RelaunchManifest`] are stable by design.
#[allow(clippy::module_name_repetitions)] // `FileRelaunchStore` is the documented public type re-exported at the crate root.
pub struct FileRelaunchStore {
    path: PathBuf,
}

impl FileRelaunchStore {
    /// Store the manifest at `path` (a single JSON file).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The conventional manifest file under a caller-supplied state directory:
    /// `<state_dir>/origin/selfdev/relaunch.json`.
    ///
    /// The crate does not depend on `dirs`; the daemon passes
    /// `dirs::data_local_dir()` (or its fallback) as `state_dir` so the final
    /// path is `data_local_dir()/origin/selfdev/relaunch.json`.
    #[must_use]
    pub fn under_state_dir(state_dir: impl AsRef<Path>) -> Self {
        let path = state_dir
            .as_ref()
            .join("origin")
            .join("selfdev")
            .join("relaunch.json");
        Self::new(path)
    }

    /// The file this store reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl RelaunchStore for FileRelaunchStore {
    fn save(&self, manifest: &RelaunchManifest) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(manifest)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    fn load(&self) -> Result<Option<RelaunchManifest>, StoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let manifest = serde_json::from_slice(&bytes)?;
                Ok(Some(manifest))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    fn clear(&self) -> Result<(), StoreError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn manifest_json_has_exact_snake_case_field_names() {
        let m = RelaunchManifest::new("/new/origin", "/old/origin", 7);
        let json = serde_json::to_string(&m).unwrap();
        assert!(
            json.contains("\"new_binary_path\":\"/new/origin\""),
            "got: {json}"
        );
        assert!(
            json.contains("\"previous_binary_path\":\"/old/origin\""),
            "got: {json}"
        );
        assert!(json.contains("\"generation\":7"), "got: {json}");
    }

    #[test]
    fn manifest_round_trips_through_serde_json() {
        let m = RelaunchManifest::new("/usr/local/bin/origin.new", "/usr/local/bin/origin", 42);
        let json = serde_json::to_string(&m).unwrap();
        let back: RelaunchManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_deserializes_from_the_exact_contract_json() {
        // The supervisor writes/reads this shape WITHOUT depending on this crate,
        // so deserialization from the literal field names is the contract.
        let json = r#"{"new_binary_path":"/n","previous_binary_path":"/p","generation":3}"#;
        let m: RelaunchManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.new_binary_path, PathBuf::from("/n"));
        assert_eq!(m.previous_binary_path, PathBuf::from("/p"));
        assert_eq!(m.generation, 3);
    }

    #[test]
    fn file_store_save_load_clear_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            FileRelaunchStore::new(tmp.path().join("nested").join("relaunch.json"));
        assert!(store.load().unwrap().is_none());

        let m = RelaunchManifest::new("/n", "/p", 9);
        store.save(&m).unwrap();
        assert_eq!(store.load().unwrap().unwrap(), m);

        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
        // Clearing an absent file is a no-op, not an error.
        store.clear().unwrap();
    }

    #[test]
    fn request_stamps_generation_into_manifest() {
        // The daemon constructs a request at restart-grant time carrying the new
        // binary; stamping it with the ReloadContext generation yields the
        // manifest the supervisor reads. Pure, no I/O.
        let req = RelaunchRequest::new("/build/origin.new", "/run/origin");
        assert_eq!(req.new_binary_path, PathBuf::from("/build/origin.new"));
        let manifest = req.into_manifest(11);
        assert_eq!(manifest.new_binary_path, PathBuf::from("/build/origin.new"));
        assert_eq!(manifest.previous_binary_path, PathBuf::from("/run/origin"));
        assert_eq!(manifest.generation, 11);
    }

    #[test]
    fn request_records_through_injected_store() {
        // IO stays injectable: a fake store captures the save without touching disk.
        #[derive(Default)]
        struct FakeStore {
            slot: std::cell::RefCell<Option<RelaunchManifest>>,
        }
        impl RelaunchStore for FakeStore {
            fn save(&self, m: &RelaunchManifest) -> Result<(), StoreError> {
                *self.slot.borrow_mut() = Some(m.clone());
                Ok(())
            }
            fn load(&self) -> Result<Option<RelaunchManifest>, StoreError> {
                Ok(self.slot.borrow().clone())
            }
            fn clear(&self) -> Result<(), StoreError> {
                *self.slot.borrow_mut() = None;
                Ok(())
            }
        }

        let store = FakeStore::default();
        let req = RelaunchRequest::new("/n", "/p");
        let written = req.record(&store, 5).unwrap();
        assert_eq!(written.generation, 5);
        assert_eq!(store.load().unwrap().unwrap(), written);
    }

    #[test]
    fn under_state_dir_uses_conventional_layout() {
        let store = FileRelaunchStore::under_state_dir("/state");
        let p = store.path();
        // The contract layout the daemon mirrors: <state>/origin/selfdev/relaunch.json
        assert!(p.ends_with(Path::new("origin/selfdev/relaunch.json")), "got: {p:?}");
        assert!(p.starts_with("/state"), "got: {p:?}");
    }
}
