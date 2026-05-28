//! `Bash` tool — run a shell command, capture stdout/stderr/exit-code.
//!
//! Uses `sh -c` on Unix. On Windows tries `pwsh -NoProfile -Command` first
//! (`PowerShell` 7+); falls back to `powershell.exe -NoProfile -Command` if pwsh
//! is not on PATH.

use std::process::Stdio;

use crate::{SideEffects, Tier, Urgency};
use origin_sandbox::SandboxProfile;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

#[allow(clippy::module_name_repetitions)] // `BashOutput` in module `bash` — name kept for API clarity
#[derive(Debug)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Run `command` through the platform shell, capturing all output.
///
/// # Errors
/// Returns a `String` describing process-spawn failure (e.g. the shell missing).
/// Non-zero exit codes do NOT error — they're returned in `BashOutput.exit_code`.
#[allow(clippy::module_name_repetitions)] // `bash_tool` in module `bash` — name kept for API clarity
pub async fn bash_tool(command: &str) -> Result<BashOutput, String> {
    #[cfg(unix)]
    let output = {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        apply_shell_profile(&mut cmd).map_err(|e| format!("sandbox: {e}"))?;
        cmd.output().await.map_err(|e| format!("spawn sh: {e}"))?
    };

    #[cfg(windows)]
    let output = {
        let pwsh_args = ["-NoProfile", "-Command", command];
        let mut cmd = Command::new("pwsh");
        cmd.args(pwsh_args);
        apply_shell_profile(&mut cmd).map_err(|e| format!("sandbox: {e}"))?;
        if let Ok(o) = cmd.output().await {
            o
        } else {
            let mut fallback = Command::new("powershell");
            fallback.args(pwsh_args);
            apply_shell_profile(&mut fallback).map_err(|e| format!("sandbox: {e}"))?;
            fallback
                .output()
                .await
                .map_err(|e| format!("spawn powershell: {e}"))?
        }
    };

    Ok(BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Streaming variant of [`bash_tool`].
///
/// Spawns the shell child with piped stdout/stderr, reads each stream
/// line-by-line, forwards every line to `chunk_tx` as it arrives, and
/// still returns the fully accumulated `BashOutput` for the LLM's
/// `tool_result`. Stderr lines are prefixed with `"stderr: "` so the
/// consumer can distinguish them in the live view; the returned
/// `BashOutput.stderr` keeps the unprefixed text.
///
/// If the receiver of `chunk_tx` is gone (CLI disconnected), forwarding
/// silently fails — accumulation still completes so the LLM gets the
/// full body.
///
/// # Errors
/// Returns a `String` describing process-spawn failure (e.g. the shell
/// missing). Non-zero exit codes do NOT error.
#[allow(clippy::module_name_repetitions)]
pub async fn bash_tool_streaming(
    command: &str,
    chunk_tx: UnboundedSender<String>,
) -> Result<BashOutput, String> {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        apply_shell_profile(&mut c).map_err(|e| format!("sandbox: {e}"))?;
        c
    };

    #[cfg(windows)]
    let mut cmd = {
        let pwsh_args = ["-NoProfile", "-Command", command];
        let mut c = Command::new("pwsh");
        c.args(pwsh_args);
        apply_shell_profile(&mut c).map_err(|e| format!("sandbox: {e}"))?;
        c
    };

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        #[cfg(windows)]
        Err(_) => {
            // pwsh missing — fall back to the legacy `powershell` shim.
            let pwsh_args = ["-NoProfile", "-Command", command];
            let mut fallback = Command::new("powershell");
            fallback.args(pwsh_args);
            apply_shell_profile(&mut fallback).map_err(|e| format!("sandbox: {e}"))?;
            fallback.stdout(Stdio::piped());
            fallback.stderr(Stdio::piped());
            fallback.stdin(Stdio::null());
            fallback.spawn().map_err(|e| format!("spawn powershell: {e}"))?
        }
        #[cfg(unix)]
        Err(e) => return Err(format!("spawn sh: {e}")),
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "child stderr unavailable".to_string())?;

    let tx_out = chunk_tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut acc = String::new();
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            // Forward live and accumulate the full body for the LLM.
            let _ = tx_out.send(line.clone());
            acc.push_str(&line);
            acc.push('\n');
        }
        acc
    });

    let tx_err = chunk_tx;
    let stderr_task = tokio::spawn(async move {
        let mut acc = String::new();
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let _ = tx_err.send(format!("stderr: {line}"));
            acc.push_str(&line);
            acc.push('\n');
        }
        acc
    });

    let status = child.wait().await.map_err(|e| format!("wait child: {e}"))?;

    let stdout_text = stdout_task.await.map_err(|e| format!("stdout task: {e}"))?;
    let stderr_text = stderr_task.await.map_err(|e| format!("stderr task: {e}"))?;

    Ok(BashOutput {
        stdout: stdout_text,
        stderr: stderr_text,
        exit_code: status.code().unwrap_or(-1),
    })
}

/// Apply the [`SandboxProfile::Shell`] profile that this tool declares in its
/// `ToolMeta`. On Windows the profile sets `CREATE_SUSPENDED`; the daemon's
/// later `attach_job_object_if_needed` step is responsible for the post-spawn
/// `JobObject` wiring (P11.4). For [`bash_tool`]'s `.output().await` path we
/// short-circuit by not calling `apply()` on Windows, since `.output()`
/// drains stdout to completion and won't resume a suspended child without
/// the `JobObject` helper. On unix the seccomp+landlock layer fires inside
/// `pre_exec` so `.output().await` works as expected.
//
// On Windows this function never mutates `cmd` and is a trivial `Ok(())`;
// the mutable signature is required by the unix arm. The allow attributes
// suppress clippy's per-arm linting accordingly.
#[allow(clippy::needless_pass_by_ref_mut, clippy::unnecessary_wraps)]
fn apply_shell_profile(cmd: &mut Command) -> Result<(), origin_sandbox::SandboxError> {
    let _ = SandboxProfile::Shell; // referenced for `cargo doc` cross-links
    #[cfg(unix)]
    {
        origin_sandbox::apply(SandboxProfile::Shell, cmd.as_std_mut())
    }
    #[cfg(windows)]
    {
        // Deliberately a no-op until the daemon-level `spawn_sandboxed`
        // helper (P12) drives `CREATE_SUSPENDED` + `attach_job_object_if_needed`
        // around the spawn. P11 ships the profile on the meta + the Linux/macOS
        // enforcement path; the Windows `JobObject` is wired but requires the
        // post-spawn handshake that the in-process tool can't perform from
        // `.output().await`. See `docs/superpowers/plans/2026-05-20-origin-phase-11.md`
        // §"out of scope" — the Windows wire-up is the daemon's job.
        let _ = cmd;
        Ok(())
    }
}

crate::origin_tool! {
    name: "Bash",
    description: "Execute a shell command. Returns stdout, stderr, and exit code.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "Shell command line" }
        },
        "required": ["command"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::Shell,
}
