// SPDX-License-Identifier: Apache-2.0
use crate::metrics::TaskResult;
use crate::task_set::Task;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Generic subprocess driver for comparison contestants.
///
/// Each contestant exposes a `--json` mode that emits one line per turn-end
/// with `{ input_tokens, output_tokens, tool_calls }`. Contestants that don't
/// natively emit JSON are wrapped by a small shim in `packaging/bench-shims/`
/// (out of scope here).
///
/// # Errors
/// Returns any I/O error from spawning the subprocess.
pub fn run_one(
    contestant: &str,
    bin: &Path,
    extra_args: &[String],
    task: &Task,
) -> anyhow::Result<TaskResult> {
    let start = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.args(extra_args);
    cmd.args(["--prompt", &task.prompt, "--json"]);
    let out = cmd.output()?;
    let wall = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut tool_calls: u32 = 0;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
                match t {
                    "turn_end" => {
                        input_tokens = input_tokens.saturating_add(
                            v.get("input_tokens")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                        );
                        output_tokens = output_tokens.saturating_add(
                            v.get("output_tokens")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                        );
                    }
                    "tool_call" => tool_calls = tool_calls.saturating_add(1),
                    _ => {}
                }
            }
        }
    }

    let passed = out.status.success()
        && wall <= task.max_turn_latency_ms
        && input_tokens <= task.max_input_tokens
        && output_tokens <= task.max_output_tokens
        && tool_calls <= task.expected_tool_calls_max;

    Ok(TaskResult {
        contestant: contestant.to_string(),
        task_id: task.id.clone(),
        input_tokens,
        output_tokens,
        wall_ms: wall,
        tool_calls,
        passed,
    })
}
