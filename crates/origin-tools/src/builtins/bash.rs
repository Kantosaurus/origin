//! `Bash` v2 — timeout, cwd, env, `run_in_background`. Backed by `proc_supervisor`.

use std::time::Duration;

use crate::error::ToolError;
use crate::proc_supervisor::{ProcStatus, SpawnOpts, Supervisor};
use crate::{SideEffects, Tier, Urgency};
use serde_json::{json, Value};

#[allow(clippy::module_name_repetitions)] // BashArgs in module bash — name kept for API clarity
#[derive(Debug, Clone)]
pub struct BashArgs {
    pub command: String,
    /// Timeout in seconds; default 120, max 600 (enforced here).
    pub timeout: Option<u32>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub run_in_background: bool,
}

/// Execute a shell command via the `Supervisor`.
///
/// Foreground (default): polls until the process terminates and returns the
/// accumulated output with `{status, exit_code, stdout}`.
///
/// Background: spawns and immediately returns `{status: "started", pid}`.
///
/// # Errors
/// Returns `bash.spawn_failed` if the shell child cannot be spawned.
#[allow(clippy::module_name_repetitions)] // bash_v2 in module bash — disambiguates from v1
pub async fn bash_v2(args: BashArgs, sup: &Supervisor) -> Result<Value, ToolError> {
    let timeout_secs = args.timeout.unwrap_or(120).min(600);
    let opts = SpawnOpts {
        timeout: Some(Duration::from_secs(u64::from(timeout_secs))),
        cwd: args.cwd,
        env: args.env,
        buffer_cap_bytes: None,
    };
    let pid = sup.spawn(&args.command, &opts)?;
    if args.run_in_background {
        return Ok(json!({"status": "started", "pid": pid}));
    }
    // Foreground: poll until status terminal, then return final body.
    let mut next = 0u64;
    let deadline = std::time::Instant::now() + Duration::from_secs(u64::from(timeout_secs) + 5);
    let mut acc = String::new();
    loop {
        let chunk = sup.read_since(pid, next, 64 * 1024)?;
        acc.push_str(&chunk.bytes);
        next = chunk.next_offset;
        if chunk.status.is_terminal() {
            let (status_str, exit_code) = match chunk.status {
                ProcStatus::Exited(c) => ("exited", c),
                ProcStatus::TimedOut => ("timed_out", -1),
                ProcStatus::Killed => ("killed", -1),
                ProcStatus::Running => unreachable!(),
            };
            return Ok(json!({
                "status": status_str,
                "exit_code": exit_code,
                "stdout": acc,
            }));
        }
        if std::time::Instant::now() > deadline {
            return Ok(json!({"status": "timed_out", "exit_code": -1, "stdout": acc}));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

crate::origin_tool! {
    name: "Bash",
    description: "Execute a shell command. Foreground (default) waits for completion and returns stdout; run_in_background returns a pid for Monitor to tail.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "command":           { "type": "string", "description": "Shell command line" },
            "timeout":           { "type": "integer", "minimum": 1, "maximum": 600, "description": "Timeout in seconds (default 120, max 600)" },
            "cwd":               { "type": "string", "description": "Working directory" },
            "env":               { "type": "array", "items": { "type": "array", "items": [{"type": "string"}, {"type": "string"}], "minItems": 2, "maxItems": 2 }, "description": "Environment variables as [[key, value], ...]" },
            "run_in_background": { "type": "boolean", "default": false, "description": "Return immediately with pid; use Monitor to tail output" }
        },
        "required": ["command"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Shell,
    token_budget: 25_000,
}
