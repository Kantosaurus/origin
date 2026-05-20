//! Leaf crate carrying the cross-process [`ResumeToken`] shape.
//!
//! Both `origin-daemon` (writer) and `origin-supervisor` (replayer) depend
//! on this crate. Keeping the type in a leaf avoids a daemon ↔ supervisor
//! cycle: the daemon checkpoints a token at each assistant-turn boundary,
//! the supervisor reads any tokens on restart and replays them to the next
//! daemon over IPC.
//!
//! Persistence is plain JSON files at `<state_dir>/resume/<session_id>.json`.
//! The wire surface is tiny so we trade rkyv perf for tooling simplicity.

#![forbid(unsafe_code)]

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Snapshot of an open session sufficient to resume it after the daemon
/// restarts.
///
/// `cas_handle_root` is the CAS root for the session's message log so the
/// next daemon can re-hydrate the transcript without re-walking `SQLite`.
/// `pending_tool_calls` holds the ids of any tool calls that were in-flight
/// when the daemon last checkpointed — the resumed daemon re-spawns them
/// under `TaskClass::Critical`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeToken {
    pub session_id: String,
    pub last_turn: u32,
    /// CAS root handle for the session's message log.
    pub cas_handle_root: [u8; 32],
    /// Tool calls that were in-flight when the daemon last checkpointed.
    pub pending_tool_calls: Vec<String>,
    /// Plan CRDT sequence number at the checkpoint.
    pub plan_seq: u64,
}

impl ResumeToken {
    /// Write to `<dir>/<session_id>.json`. JSON is fine — the surface is tiny.
    ///
    /// # Errors
    /// Propagates I/O errors and serde failures.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", self.session_id));
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Read every `*.json` token under `dir`. Missing dir → empty vec.
    ///
    /// # Errors
    /// Propagates I/O errors and serde decode failures.
    pub fn load_all(dir: &Path) -> std::io::Result<Vec<Self>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(entry.path())?;
            let token: Self = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            out.push(token);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let token = ResumeToken {
            session_id: "S".to_string(),
            last_turn: 3,
            cas_handle_root: [7u8; 32],
            pending_tool_calls: vec!["tool-1".into()],
            plan_seq: 42,
        };
        token.save(tmp.path()).expect("save");
        let mut loaded = ResumeToken::load_all(tmp.path()).expect("load");
        assert_eq!(loaded.len(), 1);
        let got = loaded.pop().expect("one");
        assert_eq!(got.session_id, "S");
        assert_eq!(got.last_turn, 3);
        assert_eq!(got.plan_seq, 42);
        assert_eq!(got.pending_tool_calls, vec!["tool-1".to_string()]);
        assert_eq!(got.cas_handle_root, [7u8; 32]);
    }

    #[test]
    fn load_all_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().expect("tmp");
        let nonexistent = tmp.path().join("nope");
        let loaded = ResumeToken::load_all(&nonexistent).expect("missing -> empty");
        assert!(loaded.is_empty());
    }
}
