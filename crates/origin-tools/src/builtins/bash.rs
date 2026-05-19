//! `Bash` tool — run a shell command, capture stdout/stderr/exit-code.
//!
//! Uses `sh -c` on Unix. On Windows tries `pwsh -NoProfile -Command` first
//! (`PowerShell` 7+); falls back to `powershell.exe -NoProfile -Command` if pwsh
//! is not on PATH.

use crate::{SideEffects, Tier, Urgency};
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
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .map_err(|e| format!("spawn sh: {e}"))?;

    #[cfg(windows)]
    let output = {
        match Command::new("pwsh")
            .args(["-NoProfile", "-Command", command])
            .output()
            .await
        {
            Ok(o) => o,
            Err(_) => Command::new("powershell")
                .args(["-NoProfile", "-Command", command])
                .output()
                .await
                .map_err(|e| format!("spawn powershell: {e}"))?,
        }
    };

    Ok(BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
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
}
