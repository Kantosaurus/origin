// SPDX-License-Identifier: Apache-2.0
//! Append-only ledger of ponytail advisories/overrides (`~/.origin/ponytail-debt.jsonl`).

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::origin_home;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtAction {
    Advisory,
    OverrideOnce,
    Remembered,
    HeadlessAllow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebtEvent {
    pub action: DebtAction,
    pub dep: String,
    pub native: String,
    /// Unix seconds; 0 when unknown (the daemon stamps the real time).
    #[serde(default)]
    pub ts: u64,
}

#[must_use]
pub fn ledger_path() -> PathBuf {
    origin_home().join("ponytail-debt.jsonl")
}

/// Append one event. Best-effort: never panics, never blocks a write on failure.
pub fn log(action: DebtAction, dep: &str, native: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ev = DebtEvent { action, dep: dep.to_string(), native: native.to_string(), ts };
    let Ok(line) = serde_json::to_string(&ev) else { return };
    let path = ledger_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

#[must_use]
pub fn read() -> Vec<DebtEvent> {
    std::fs::read_to_string(ledger_path())
        .map(|c| c.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_jsonl() {
        let line = serde_json::to_string(&DebtEvent {
            action: DebtAction::OverrideOnce,
            dep: "lodash".into(),
            native: "Object.groupBy".into(),
            ts: 0,
        })
        .unwrap();
        let back: DebtEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.dep, "lodash");
        assert!(matches!(back.action, DebtAction::OverrideOnce));
    }
}
