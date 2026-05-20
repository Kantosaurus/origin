//! `Bash` tool — run a shell command, capture stdout/stderr/exit-code.
//!
//! Uses `sh -c` on Unix. On Windows tries `pwsh -NoProfile -Command` first
//! (`PowerShell` 7+); falls back to `powershell.exe -NoProfile -Command` if pwsh
//! is not on PATH.

use crate::{SideEffects, Tier, Urgency};
use origin_sandbox::SandboxProfile;
use tokio::process::Command;

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
        match cmd.output().await {
            Ok(o) => o,
            Err(_) => {
                let mut fallback = Command::new("powershell");
                fallback.args(pwsh_args);
                apply_shell_profile(&mut fallback).map_err(|e| format!("sandbox: {e}"))?;
                fallback
                    .output()
                    .await
                    .map_err(|e| format!("spawn powershell: {e}"))?
            }
        }
    };

    Ok(BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Apply the `Shell` sandbox profile that this tool declares in its
/// `ToolMeta`. On Windows the profile sets `CREATE_SUSPENDED`; the daemon's
/// later `attach_job_object_if_needed` step is responsible for the post-spawn
/// JobObject wiring (P11.4) — for `bash_tool`'s `.output().await` path we
/// short-circuit by not calling `apply()` on Windows, since `.output()`
/// drains stdout to completion and won't resume a suspended child without
/// the JobObject helper. On unix the seccomp+landlock layer fires inside
/// `pre_exec` so `output()` works as expected.
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
        // enforcement path; the Windows JobObject is wired but requires the
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
