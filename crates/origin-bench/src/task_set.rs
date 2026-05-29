// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub expected_tools_min: Vec<String>,
    pub expected_tool_calls_max: u32,
    pub max_turn_latency_ms: u64,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
}

/// Load `manifest.json` + every task JSON it references.
///
/// # Errors
/// Returns any error from filesystem I/O or JSON parsing.
pub fn load(root: &Path) -> anyhow::Result<Vec<Task>> {
    let manifest_path = root.join("manifest.json");
    let body = std::fs::read(&manifest_path)?;
    let m: Manifest = serde_json::from_slice(&body)?;
    let mut out = Vec::with_capacity(m.tasks.len());
    for rel in &m.tasks {
        let p: PathBuf = root.join(rel);
        let body = std::fs::read(&p)?;
        let t: Task = serde_json::from_slice(&body)?;
        out.push(t);
    }
    Ok(out)
}
