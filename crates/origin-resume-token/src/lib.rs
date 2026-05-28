//! Leaf crate carrying the cross-process [`ResumeToken`] shape.
//!
//! Both `origin-daemon` (writer) and `origin-supervisor` (replayer) depend
//! on this crate. Keeping the type in a leaf avoids a daemon ↔ supervisor
//! cycle: the daemon checkpoints a token at each assistant-turn boundary,
//! the supervisor reads any tokens on restart and replays them to the next
//! daemon over IPC.
//!
//! # Persistence + authentication
//!
//! Tokens live at `<state_dir>/resume/<session_id>.json`. Without a MAC,
//! anyone who can write that directory can swap `cas_handle_root` and
//! steer the resumed daemon into arbitrary CAS content — effectively a
//! code-execution gadget given how CAS handles flow into the agent loop.
//!
//! Each token is therefore wrapped:
//! ```text
//! { "payload": "<inner ResumeToken JSON, compact, as a STRING>",
//!   "mac_hex": "<64-char hex of blake3::keyed_hash(key, payload.as_bytes())>" }
//! ```
//! The MAC input is *literally* `payload.as_bytes()` — no canonicalization
//! round-trip, no formatter sensitivity. The key is a 32-byte file at
//! `<dir>/.mac-key`, generated on first save via `getrandom`, chmod 0600
//! on unix. On windows we cannot tighten the ACL via the stdlib alone
//! (see save() for the gap note) so the directory itself must already be
//! user-private.
//!
//! No back-compat for the pre-MAC bare-JSON format: a load of an
//! unwrapped file errors out.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

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

/// On-disk wrapper. `payload` is the inner `ResumeToken` JSON serialized
/// as a compact string — i.e. a JSON string field whose content is itself
/// JSON. This makes the MAC input unambiguous: `payload.as_bytes()`, no
/// canonicalization. `mac_hex` is the lowercase hex encoding of
/// `blake3::keyed_hash(key, payload.as_bytes())`.
#[derive(Debug, Serialize, Deserialize)]
struct OnDisk {
    payload: String,
    mac_hex: String,
}

const KEY_FILE: &str = ".mac-key";
const KEY_LEN: usize = 32;

fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

/// Load the sidecar MAC key, or generate-and-persist it if missing.
/// Called from `save`. Never called from `load_all` — load only reads.
fn load_or_create_key(dir: &Path) -> std::io::Result<[u8; KEY_LEN]> {
    let path = key_path(dir);
    match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() != KEY_LEN {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "MAC key at {} is {} bytes, expected {}",
                        path.display(),
                        bytes.len(),
                        KEY_LEN
                    ),
                ));
            }
            let mut k = [0u8; KEY_LEN];
            k.copy_from_slice(&bytes);
            Ok(k)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut k = [0u8; KEY_LEN];
            getrandom::getrandom(&mut k).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("getrandom failed: {e}"))
            })?;
            std::fs::write(&path, k)?;
            // Tighten perms on unix. On windows std has no portable chmod —
            // we rely on the enclosing state dir already being user-private
            // (created by the supervisor under %LOCALAPPDATA%). Documented
            // gap; if it becomes a concern, swap in `windows-acl` later.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path)?.permissions();
                perms.set_mode(0o600);
                std::fs::set_permissions(&path, perms)?;
            }
            Ok(k)
        }
        Err(e) => Err(e),
    }
}

/// Read the sidecar MAC key. Fails (NotFound) if missing — load paths
/// must NOT auto-generate, because that would silently let a tampered
/// token slide through after an attacker deletes the key.
fn load_key_strict(dir: &Path) -> std::io::Result<[u8; KEY_LEN]> {
    let path = key_path(dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "MAC key missing; cannot verify tokens",
            ));
        }
        Err(e) => return Err(e),
    };
    if bytes.len() != KEY_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "MAC key at {} is {} bytes, expected {}",
                path.display(),
                bytes.len(),
                KEY_LEN
            ),
        ));
    }
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(&bytes);
    Ok(k)
}

fn compute_mac_hex(key: &[u8; KEY_LEN], payload: &[u8]) -> String {
    let hash = blake3::keyed_hash(key, payload);
    hex::encode(hash.as_bytes())
}

impl ResumeToken {
    /// Write to `<dir>/<session_id>.json` as a MAC-wrapped envelope.
    /// On first call into a fresh `dir`, a `.mac-key` sidecar is generated.
    ///
    /// # Errors
    /// Propagates I/O errors, serde failures, and `getrandom` failures.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let key = load_or_create_key(dir)?;

        // Inner payload: compact JSON of the ResumeToken.
        let inner = serde_json::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mac_hex = compute_mac_hex(&key, inner.as_bytes());
        let wrapper = OnDisk {
            payload: inner,
            mac_hex,
        };
        // Outer wrapper: pretty is fine, only `payload.as_bytes()` is MAC'd
        // and we never re-parse the inner payload for MAC verification.
        let json = serde_json::to_vec_pretty(&wrapper)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let path = dir.join(format!("{}.json", self.session_id));
        std::fs::write(path, json)
    }

    /// Load the single MAC-wrapped token at `<dir>/<session_id>.json`,
    /// verifying its MAC. Returns `Ok(None)` if the file does not exist.
    ///
    /// # Errors
    /// Propagates I/O errors, serde decode failures, missing-key, and
    /// MAC mismatches — never silently skips a present-but-bad token.
    pub fn load_one(dir: &Path, session_id: &str) -> std::io::Result<Option<Self>> {
        let path = dir.join(format!("{session_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        let wrapper: OnDisk = serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "resume token {} is not a valid MAC-wrapped envelope: {e}",
                    path.display()
                ),
            )
        })?;
        let key = load_key_strict(dir)?;
        let expected_hex = compute_mac_hex(&key, wrapper.payload.as_bytes());
        let got_hex = wrapper.mac_hex.as_bytes();
        let exp_hex = expected_hex.as_bytes();
        let ok: bool = got_hex.len() == exp_hex.len() && bool::from(got_hex.ct_eq(exp_hex));
        if !ok {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("MAC mismatch for {session_id}.json"),
            ));
        }
        let token: Self = serde_json::from_str(&wrapper.payload)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(token))
    }

    /// Read every `*.json` token under `dir`, verifying each MAC.
    /// Missing dir → empty vec. Missing `.mac-key` while token files
    /// exist → Err(NotFound). Tampered or unwrapped file → Err(InvalidData).
    ///
    /// # Errors
    /// Propagates I/O errors, serde decode failures, missing-key, and
    /// MAC mismatches. The caller (supervisor) needs to see these — a
    /// silent skip would mask a tamper attempt.
    pub fn load_all(dir: &Path) -> std::io::Result<Vec<Self>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        // Pre-scan for token files. If none, the key file's absence is
        // irrelevant — return empty cleanly.
        let entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let key = load_key_strict(dir)?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let path = entry.path();
            let bytes = std::fs::read(&path)?;
            let wrapper: OnDisk = serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "resume token {} is not a valid MAC-wrapped envelope: {e}",
                        path.display()
                    ),
                )
            })?;
            let expected_hex = compute_mac_hex(&key, wrapper.payload.as_bytes());
            // Constant-time compare via subtle. Compare the hex bytes —
            // both sides are 64 lowercase ASCII chars by construction.
            let got_hex = wrapper.mac_hex.as_bytes();
            let exp_hex = expected_hex.as_bytes();
            let ok: bool = got_hex.len() == exp_hex.len() && bool::from(got_hex.ct_eq(exp_hex));
            if !ok {
                let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("<unknown>");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("MAC mismatch for {filename}"),
                ));
            }
            let token: Self = serde_json::from_str(&wrapper.payload)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            out.push(token);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> ResumeToken {
        ResumeToken {
            session_id: id.to_string(),
            last_turn: 3,
            cas_handle_root: [7u8; 32],
            pending_tool_calls: vec!["tool-1".into()],
            plan_seq: 42,
        }
    }

    #[test]
    fn save_then_load_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let token = sample("S");
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

    #[test]
    fn mac_mismatch_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmp");
        let token = sample("S");
        token.save(tmp.path()).expect("save");

        // Tamper with the payload field of the on-disk JSON: parse the
        // wrapper, mutate the inner payload (flip a byte of cas_handle_root),
        // write it back. mac_hex is left intact -> MAC over new payload
        // bytes will not match.
        let file = tmp.path().join("S.json");
        let raw = std::fs::read(&file).expect("read");
        let mut wrapper: OnDisk = serde_json::from_slice(&raw).expect("parse wrapper");
        // wrapper.payload is the inner JSON; mutate cas_handle_root[0].
        let mut inner: serde_json::Value = serde_json::from_str(&wrapper.payload).expect("parse inner");
        let arr = inner["cas_handle_root"].as_array_mut().expect("arr");
        let old = arr[0].as_u64().unwrap_or(0);
        arr[0] = serde_json::Value::from(old ^ 0xFF);
        wrapper.payload = serde_json::to_string(&inner).expect("reser");
        std::fs::write(&file, serde_json::to_vec_pretty(&wrapper).expect("ser")).expect("write");

        let err = ResumeToken::load_all(tmp.path()).expect_err("must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("MAC"),
            "expected MAC in error, got: {err}"
        );
    }

    #[test]
    fn missing_key_file_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmp");
        let token = sample("S");
        token.save(tmp.path()).expect("save");

        // Delete .mac-key while leaving the token file intact.
        std::fs::remove_file(tmp.path().join(KEY_FILE)).expect("remove key");

        let err = ResumeToken::load_all(tmp.path()).expect_err("must reject");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn key_file_persists_across_saves() {
        let tmp = tempfile::tempdir().expect("tmp");
        let a = sample("A");
        let mut b = sample("B");
        b.session_id = "B".to_string();
        a.save(tmp.path()).expect("save a");
        let key_after_a = std::fs::read(tmp.path().join(KEY_FILE)).expect("read key");
        assert_eq!(key_after_a.len(), KEY_LEN);
        b.save(tmp.path()).expect("save b");
        let key_after_b = std::fs::read(tmp.path().join(KEY_FILE)).expect("read key");
        assert_eq!(key_after_a, key_after_b, "key must persist across saves");
    }

    #[test]
    fn load_rejects_unwrapped_legacy_format() {
        let tmp = tempfile::tempdir().expect("tmp");
        // Make sure a key file exists so the rejection is about shape, not
        // missing key — that's a separately-tested failure mode.
        std::fs::write(tmp.path().join(KEY_FILE), [0u8; KEY_LEN]).expect("write key");
        // Write a bare ResumeToken JSON (old format).
        let bare = serde_json::to_vec_pretty(&sample("legacy")).expect("ser");
        std::fs::write(tmp.path().join("legacy.json"), bare).expect("write");

        let err = ResumeToken::load_all(tmp.path()).expect_err("must reject legacy");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
