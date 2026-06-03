// SPDX-License-Identifier: Apache-2.0
//! Real, opt-in `cargo` build/test runners.
//!
//! This is the ONLY part of the crate that performs real process I/O. It is
//! deliberately small, clearly named, and never used by the state machine's
//! tests — Phase 2 chooses it explicitly when wiring the daemon, while tests
//! inject fakes. The runners shell out to `cargo build` / `cargo test` in a
//! given workspace directory and report success/failure (with captured stderr
//! as the failure reason).
//!
//! `cargo` is invoked by name and resolved from `PATH`; override it with the
//! `cargo_bin` field if the daemon needs a pinned toolchain.

use std::process::Command;

use crate::driver::{BuildRunner, TestRunner};
use crate::queue::BuildJob;

/// A real [`BuildRunner`]/[`TestRunner`] that shells out to `cargo`.
///
/// Construct with [`CargoRunner::new`] pointing at the workspace whose source
/// the self-dev job edited. Both `build` and `test` run with `--quiet` and
/// capture output; on a non-zero exit, the trimmed stderr (falling back to
/// stdout) becomes the error reason.
#[allow(clippy::module_name_repetitions)] // `CargoRunner` is the documented public type re-exported at the crate root.
#[derive(Debug, Clone)]
pub struct CargoRunner {
    /// Working directory the `cargo` invocation runs in (the workspace root).
    workspace_dir: std::path::PathBuf,
    /// The `cargo` executable to invoke (defaults to `"cargo"`).
    cargo_bin: String,
    /// Extra arguments appended to every invocation (e.g. `--locked`,
    /// `-p origin-cli`). Empty by default.
    extra_args: Vec<String>,
}

impl CargoRunner {
    /// Create a runner rooted at `workspace_dir`, invoking `cargo` from `PATH`.
    #[must_use]
    pub fn new(workspace_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            workspace_dir: workspace_dir.into(),
            cargo_bin: "cargo".to_string(),
            extra_args: Vec::new(),
        }
    }

    /// Override the `cargo` executable (e.g. a `rustup`-proxied pinned
    /// toolchain path).
    #[must_use]
    pub fn with_cargo_bin(mut self, bin: impl Into<String>) -> Self {
        self.cargo_bin = bin.into();
        self
    }

    /// Append extra arguments to every `cargo` invocation (e.g. `--locked`).
    #[must_use]
    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Run `cargo <subcommand> --quiet <extra_args...>` in the workspace and map
    /// the outcome to `Ok(())` / `Err(reason)`.
    fn run(&self, subcommand: &str) -> Result<(), String> {
        let mut cmd = Command::new(&self.cargo_bin);
        cmd.arg(subcommand)
            .arg("--quiet")
            .args(&self.extra_args)
            .current_dir(&self.workspace_dir);
        let output = cmd
            .output()
            .map_err(|e| format!("failed to spawn `{} {subcommand}`: {e}", self.cargo_bin))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = if stderr.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        let code = output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string());
        Err(format!("cargo {subcommand} exited {code}: {reason}"))
    }
}

impl BuildRunner for CargoRunner {
    fn build(&self, _job: &BuildJob) -> Result<(), String> {
        self.run("build")
    }
}

impl TestRunner for CargoRunner {
    fn test(&self, _job: &BuildJob) -> Result<(), String> {
        self.run("test")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn builders_set_fields() {
        let r = CargoRunner::new("/work")
            .with_cargo_bin("/opt/cargo")
            .with_extra_args(vec!["--locked".into()]);
        assert_eq!(r.cargo_bin, "/opt/cargo");
        assert_eq!(r.extra_args, vec!["--locked".to_string()]);
        assert_eq!(r.workspace_dir, std::path::PathBuf::from("/work"));
    }

    #[test]
    fn spawn_failure_is_reported_not_panicked() {
        // A non-existent cargo binary must yield an Err (a clean failure
        // reason), never a panic — proving the runner degrades gracefully.
        let r = CargoRunner::new(std::env::temp_dir())
            .with_cargo_bin("definitely-not-a-real-cargo-binary-xyz");
        let job = BuildJob::new("j", "x");
        let err = r.build(&job).unwrap_err();
        assert!(err.contains("failed to spawn"), "got: {err}");
    }
}
