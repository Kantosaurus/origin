// SPDX-License-Identifier: Apache-2.0
//! `Monitor` tool — tail a supervisor process's output by byte offset.

use std::time::Duration;

use crate::error::ToolError;
use crate::proc_supervisor::{ProcStatus, Supervisor};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[allow(clippy::module_name_repetitions)] // MonitorArgs in module monitor — name kept for API clarity
#[derive(Debug, Clone)]
pub struct MonitorArgs {
    pub pid: u32,
    pub since_byte: u64,
    pub max_bytes: u32,
    pub wait: bool,
}

/// Tail `pid`'s output from `since_byte`, returning up to `max_bytes`.
///
/// If `wait` is `true`, polls for up to 2 s until new bytes arrive or the
/// process terminates.
///
/// # Errors
/// Returns `validation.unknown_pid` if pid is unknown.
pub async fn monitor(args: MonitorArgs, sup: &Supervisor) -> Result<Value, ToolError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let chunk = sup.read_since(args.pid, args.since_byte, args.max_bytes as usize)?;
        let bytes_avail = !chunk.bytes.is_empty();
        if !args.wait || bytes_avail || chunk.status.is_terminal() {
            let status_str = match chunk.status {
                ProcStatus::Running => "running",
                ProcStatus::Exited(_) => "exited",
                ProcStatus::TimedOut => "timed_out",
                ProcStatus::Killed => "killed",
            };
            let exit_code = if let ProcStatus::Exited(c) = chunk.status {
                Some(c)
            } else {
                None
            };
            let mut out = json!({
                "bytes": chunk.bytes,
                "next_offset": chunk.next_offset,
                "status": status_str,
            });
            if let Some(c) = exit_code {
                out["exit_code"] = json!(c);
            }
            return Ok(out);
        }
        if std::time::Instant::now() > deadline {
            return Ok(json!({
                "bytes": "",
                "next_offset": args.since_byte,
                "status": "running"
            }));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

crate::origin_tool! {
    name: "Monitor",
    description: "Tail output from a background process started by Bash{run_in_background:true}. Pass since_byte=N to skip already-seen bytes.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pid":        { "type": "integer", "minimum": 1, "description": "Process ID returned by Bash with run_in_background:true" },
            "since_byte": { "type": "integer", "minimum": 0, "default": 0, "description": "Byte offset to read from (use next_offset from previous call)" },
            "max_bytes":  { "type": "integer", "minimum": 1, "default": 4096, "description": "Maximum bytes to return" },
            "wait":       { "type": "boolean", "default": false, "description": "If true, poll up to 2s for new output before returning" }
        },
        "required": ["pid"]
    }"#,
}
