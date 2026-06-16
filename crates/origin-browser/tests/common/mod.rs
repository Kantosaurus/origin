// SPDX-License-Identifier: Apache-2.0
//! Shared test helpers for the node-backed browser integration tests.

/// Probe whether the `node` binary is runnable on this runner.
///
/// CI runners usually have node, but version/PATH can differ across the
/// Windows/macOS/Linux images. Tests that hard-spawn `node` should call this
/// and skip cleanly (return early) when it reports `false`, so the suite never
/// hard-fails on a node-less or mismatched runner.
#[must_use]
pub fn node_available() -> bool {
    let status = std::process::Command::new("node")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(status, Ok(s) if s.success())
}
