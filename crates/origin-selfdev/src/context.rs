// SPDX-License-Identifier: Apache-2.0
//! Resume context for supervised self-dev restarts.
//!
//! When a self-dev job reaches a granted restart, the daemon will exec the
//! freshly built binary. Everything the *next* process needs to pick up where
//! the old one left off lives in a [`ReloadContext`]: which sessions were open,
//! what goal was being pursued, which self-dev job was in flight, and a
//! monotonically increasing [`ReloadContext::generation`] counter so the new
//! process can tell it is a successor (and detect restart storms even across
//! `exec`).
//!
//! This is intentionally separate from `origin-resume-token`'s per-session
//! `ResumeToken` (which carries CAS roots + in-flight tool calls): a
//! `ReloadContext` is the *self-dev orchestration* state spanning the restart,
//! and references the affected session ids so the daemon can pair it with the
//! per-session resume tokens it already writes.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Everything needed to resume self-dev orchestration after a supervised
/// restart into a newly built binary.
///
/// Persisted via a [`ReloadStore`] before the restart is granted and loaded by
/// the successor process on boot.
#[allow(clippy::module_name_repetitions)] // `ReloadContext` is the documented public type re-exported at the crate root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadContext {
    /// Sessions that were open at restart time. The daemon pairs these with the
    /// per-session `origin-resume-token` entries it already checkpoints.
    pub session_ids: Vec<String>,
    /// The user-facing goal the self-dev job was pursuing, if any.
    #[serde(default)]
    pub pending_goal: Option<String>,
    /// Id of the self-dev [`crate::BuildJob`] that triggered this restart.
    pub in_flight_job_id: String,
    /// Monotonically increasing successor counter. Each granted restart bumps
    /// this; the successor uses it to detect it is a reload and to bound
    /// restart storms across `exec` boundaries.
    pub generation: u64,
}

impl ReloadContext {
    /// Construct a context for the first restart of `job_id` (generation 1).
    #[must_use]
    pub fn new(job_id: impl Into<String>) -> Self {
        Self {
            session_ids: Vec::new(),
            pending_goal: None,
            in_flight_job_id: job_id.into(),
            generation: 1,
        }
    }

    /// Attach the open session ids to resume.
    #[must_use]
    pub fn with_sessions(mut self, sessions: Vec<String>) -> Self {
        self.session_ids = sessions;
        self
    }

    /// Attach the pending goal text.
    #[must_use]
    pub fn with_goal(mut self, goal: impl Into<String>) -> Self {
        self.pending_goal = Some(goal.into());
        self
    }

    /// Return a successor context: same job/sessions/goal, generation bumped by
    /// one (saturating). Used when a reload itself triggers another reload so
    /// the generation counter keeps climbing across `exec`.
    #[must_use]
    pub fn successor(&self) -> Self {
        Self {
            session_ids: self.session_ids.clone(),
            pending_goal: self.pending_goal.clone(),
            in_flight_job_id: self.in_flight_job_id.clone(),
            generation: self.generation.saturating_add(1),
        }
    }
}

/// Error raised by a [`ReloadStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An underlying I/O failure (file read/write/create).
    #[error("reload-store io: {0}")]
    Io(#[from] std::io::Error),
    /// Serialization or deserialization of the context failed.
    #[error("reload-store codec: {0}")]
    Codec(#[from] serde_json::Error),
}

/// Persistence boundary for a [`ReloadContext`].
///
/// Injected so the driver/daemon can persist across a restart while tests use
/// an in-memory fake. Implementors own all I/O.
pub trait ReloadStore {
    /// Persist `ctx`, replacing any previously stored context.
    ///
    /// # Errors
    /// Returns [`StoreError`] on I/O or serialization failure.
    fn save(&self, ctx: &ReloadContext) -> Result<(), StoreError>;

    /// Load the stored context, or `None` if none has been saved.
    ///
    /// # Errors
    /// Returns [`StoreError`] on I/O or deserialization failure.
    fn load(&self) -> Result<Option<ReloadContext>, StoreError>;

    /// Remove any stored context (e.g. once the successor has fully resumed).
    ///
    /// # Errors
    /// Returns [`StoreError`] on I/O failure other than "already absent".
    fn clear(&self) -> Result<(), StoreError>;
}

/// A simple JSON-file-backed [`ReloadStore`].
///
/// The context is small and tooling-friendly as JSON. Unlike a resume token it
/// carries no CAS handles, so there is no MAC requirement here — the daemon is
/// expected to place it under its already-private state directory. This impl is
/// provided for Phase 2 convenience; the state machine never depends on it.
pub struct FileReloadStore {
    path: std::path::PathBuf,
}

impl FileReloadStore {
    /// Store the context at `path` (a single JSON file).
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file this store reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ReloadStore for FileReloadStore {
    fn save(&self, ctx: &ReloadContext) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(ctx)?;
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    fn load(&self) -> Result<Option<ReloadContext>, StoreError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let ctx = serde_json::from_slice(&bytes)?;
                Ok(Some(ctx))
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
    use std::cell::RefCell;

    /// In-memory fake used by these store tests.
    #[derive(Default)]
    struct FakeStore {
        slot: RefCell<Option<ReloadContext>>,
    }

    impl ReloadStore for FakeStore {
        fn save(&self, ctx: &ReloadContext) -> Result<(), StoreError> {
            *self.slot.borrow_mut() = Some(ctx.clone());
            Ok(())
        }
        fn load(&self) -> Result<Option<ReloadContext>, StoreError> {
            Ok(self.slot.borrow().clone())
        }
        fn clear(&self) -> Result<(), StoreError> {
            *self.slot.borrow_mut() = None;
            Ok(())
        }
    }

    fn sample() -> ReloadContext {
        ReloadContext::new("job-7")
            .with_sessions(vec!["s-a".into(), "s-b".into()])
            .with_goal("make the parser faster")
    }

    #[test]
    fn builder_sets_fields() {
        let ctx = sample();
        assert_eq!(ctx.in_flight_job_id, "job-7");
        assert_eq!(ctx.session_ids, vec!["s-a".to_string(), "s-b".to_string()]);
        assert_eq!(ctx.pending_goal.as_deref(), Some("make the parser faster"));
        assert_eq!(ctx.generation, 1);
    }

    #[test]
    fn successor_bumps_generation_and_keeps_payload() {
        let ctx = sample();
        let next = ctx.successor();
        assert_eq!(next.generation, 2);
        assert_eq!(next.in_flight_job_id, ctx.in_flight_job_id);
        assert_eq!(next.session_ids, ctx.session_ids);
        assert_eq!(next.pending_goal, ctx.pending_goal);
        // And it keeps climbing.
        assert_eq!(next.successor().generation, 3);
    }

    #[test]
    fn successor_saturates_at_u64_max() {
        let mut ctx = ReloadContext::new("j");
        ctx.generation = u64::MAX;
        assert_eq!(ctx.successor().generation, u64::MAX);
    }

    #[test]
    fn round_trips_through_fake_store() {
        let store = FakeStore::default();
        assert!(store.load().unwrap().is_none());

        let ctx = sample();
        store.save(&ctx).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded, ctx);

        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn save_overwrites_previous() {
        let store = FakeStore::default();
        store.save(&ReloadContext::new("first")).unwrap();
        store.save(&ReloadContext::new("second")).unwrap();
        assert_eq!(store.load().unwrap().unwrap().in_flight_job_id, "second");
    }

    #[test]
    fn json_round_trip_is_stable() {
        let ctx = sample();
        let json = serde_json::to_string(&ctx).unwrap();
        let back: ReloadContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
    }

    #[test]
    fn missing_optional_goal_defaults_to_none() {
        // A payload written before / without the goal still deserializes.
        let json = r#"{"session_ids":["x"],"in_flight_job_id":"j","generation":4}"#;
        let ctx: ReloadContext = serde_json::from_str(json).unwrap();
        assert_eq!(ctx.pending_goal, None);
        assert_eq!(ctx.generation, 4);
    }

    #[test]
    fn file_store_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileReloadStore::new(tmp.path().join("nested").join("reload.json"));
        assert!(store.load().unwrap().is_none());

        let ctx = sample();
        store.save(&ctx).unwrap();
        assert_eq!(store.load().unwrap().unwrap(), ctx);

        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
        // Clearing an absent file is a no-op, not an error.
        store.clear().unwrap();
    }
}
