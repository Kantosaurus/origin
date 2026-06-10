// SPDX-License-Identifier: Apache-2.0
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
            // The per-read cap (64 KiB) may leave buffered output behind; the
            // process has terminated and its readers have been drained, so the
            // ring will not grow further — read until it is empty.
            loop {
                let more = sup.read_since(pid, next, 64 * 1024)?;
                if more.bytes.is_empty() {
                    break;
                }
                acc.push_str(&more.bytes);
                next = more.next_offset;
            }
            let (status_str, exit_code) = match chunk.status {
                ProcStatus::Exited(c) => ("exited", c),
                ProcStatus::TimedOut => ("timed_out", -1),
                ProcStatus::Killed => ("killed", -1),
                ProcStatus::Running => unreachable!(),
            };
            return Ok(json!({
                "status": status_str,
                "exit_code": exit_code,
                "stdout": cap_stdout(acc),
            }));
        }
        if std::time::Instant::now() > deadline {
            return Ok(json!({"status": "timed_out", "exit_code": -1, "stdout": cap_stdout(acc)}));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Byte budget for returned stdout (~24k tokens at ~4 bytes/token, just under
/// the tool's declared `token_budget` of 25k). A chatty command (a build log,
/// `find /`, a stack dump) would otherwise flood the model's context with its
/// entire output every turn it stays in history.
const STDOUT_BYTE_BUDGET: usize = 96_000;

/// Cap `out` to [`STDOUT_BYTE_BUDGET`], preserving the head and the tail
/// (command errors and exit summaries usually surface at the *end*) and noting
/// the elided middle. Truncates on UTF-8 char boundaries so the JSON stays valid.
fn cap_stdout(out: String) -> String {
    if out.len() <= STDOUT_BYTE_BUDGET {
        return out;
    }
    let head_budget = STDOUT_BYTE_BUDGET * 2 / 3;
    let tail_budget = STDOUT_BYTE_BUDGET - head_budget;
    let head_end = floor_char_boundary(&out, head_budget);
    let tail_start = ceil_char_boundary(&out, out.len() - tail_budget);
    let elided = tail_start.saturating_sub(head_end);
    format!(
        "{}\n... [{elided} bytes elided to fit the tool output budget] ...\n{}",
        &out[..head_end],
        &out[tail_start..]
    )
}

/// Largest char boundary `<= i` (stable-Rust stand-in for the nightly
/// `str::floor_char_boundary`).
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i`.
fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod cap_tests {
    use super::cap_stdout;

    #[test]
    fn short_output_is_unchanged() {
        let s = "hello world".to_string();
        assert_eq!(cap_stdout(s.clone()), s);
    }

    #[test]
    fn long_output_keeps_head_and_tail_and_notes_elision() {
        let head = "H".repeat(80_000);
        let tail = "TAIL_MARKER_AT_END";
        let s = format!("{head}{}{tail}", "M".repeat(80_000));
        let capped = cap_stdout(s);
        assert!(capped.len() < 100_000, "must be capped: {}", capped.len());
        assert!(capped.starts_with("HHHH"), "head preserved");
        assert!(capped.ends_with(tail), "tail preserved (errors surface at end)");
        assert!(capped.contains("bytes elided"), "elision noted");
    }

    #[test]
    fn cap_respects_utf8_boundaries() {
        // A multibyte char straddling the budget must not be split.
        let s = format!("{}é{}", "a".repeat(95_999), "z".repeat(5_000));
        let capped = cap_stdout(s);
        // Round-trips as valid UTF-8 (String is always valid; the slices must be too).
        assert!(capped.is_char_boundary(0));
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
            "env":               { "type": "array", "items": { "type": "array", "prefixItems": [{"type": "string"}, {"type": "string"}], "items": false, "minItems": 2, "maxItems": 2 }, "description": "Environment variables as [[key, value], ...]" },
            "run_in_background": { "type": "boolean", "default": false, "description": "Return immediately with pid; use Monitor to tail output" }
        },
        "required": ["command"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Shell,
    token_budget: 25_000,
}
