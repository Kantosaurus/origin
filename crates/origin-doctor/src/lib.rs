// SPDX-License-Identifier: Apache-2.0
//! Environment/runtime diagnostics and privacy disclosure for `origin`.
//!
//! This crate mirrors openclaude's `doctor:runtime` (a health checklist over the
//! toolchain, config, daemon, and providers) and `verify:privacy` (an explicit
//! list of every outbound network behaviour the tool can perform). Unlike those
//! tools it performs **no real I/O**: every fact about the environment arrives
//! through an injected [`DoctorInputs`] value, so the daemon/CLI does the probing
//! and this crate does the pure verdict logic. That keeps it fully deterministic
//! and trivially testable.
//!
//! ```
//! use origin_doctor::{diagnose, DoctorInputs, Health};
//!
//! let inputs = DoctorInputs {
//!     rust_version: Some("1.83.0".to_string()),
//!     config_present: true,
//!     daemon_running: true,
//!     providers_configured: vec!["anthropic".to_string()],
//!     writable_home: true,
//!     network_ok: Some(true),
//! };
//! let report = diagnose(&inputs);
//! assert_eq!(report.worst(), Health::Ok);
//! assert!(report.phone_home.iter().any(|p| p.contains("auto-update")));
//! ```

#![forbid(unsafe_code)]

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Minimum Rust toolchain `origin` builds against (workspace MSRV).
///
/// A toolchain older than this fails the toolchain check; an unknown version
/// only warns.
pub const MIN_RUST_VERSION: (u64, u64) = (1, 83);

/// Health verdict for a single check or for the report as a whole.
///
/// Ordered from healthiest to most severe so [`DoctorReport::worst`] can take a
/// straightforward maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Health {
    /// Everything looks correct.
    Ok,
    /// Degraded or unverifiable, but `origin` can still run.
    Warn,
    /// Broken in a way that blocks normal operation.
    Fail,
}

impl Health {
    /// Short uppercase label for plain-text rendering (`OK` / `WARN` / `FAIL`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

/// A single diagnostic line: what was checked, the verdict, and a human detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Stable, short identifier for the check (e.g. `toolchain`, `config`).
    pub name: String,
    /// Verdict for this check.
    pub health: Health,
    /// One-line, user-facing explanation of the verdict.
    pub detail: String,
}

impl Check {
    /// Construct a check from its parts.
    #[must_use]
    pub fn new(name: impl Into<String>, health: Health, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            health,
            detail: detail.into(),
        }
    }
}

/// Facts about the environment, gathered by the caller and injected here.
///
/// The daemon/CLI is responsible for the actual probing (reading the toolchain
/// version, stat-ing the config file, pinging the daemon socket, etc.); this
/// struct is the boundary so the verdict logic stays pure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorInputs {
    /// Detected Rust toolchain version (e.g. `"1.83.0"`), or `None` if it could
    /// not be determined.
    pub rust_version: Option<String>,
    /// Whether a usable `origin` config file was found.
    pub config_present: bool,
    /// Whether the background daemon is currently reachable.
    pub daemon_running: bool,
    /// Names of providers that have credentials/config (e.g. `["anthropic"]`).
    pub providers_configured: Vec<String>,
    /// Whether the home/config directory is writable.
    pub writable_home: bool,
    /// Outcome of an optional outbound connectivity probe: `Some(true)` reachable,
    /// `Some(false)` failed, `None` not attempted (e.g. offline by policy).
    pub network_ok: Option<bool>,
}

/// Result of running diagnostics: the per-check list plus a privacy disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Individual checks, in a stable display order.
    pub checks: Vec<Check>,
    /// Every outbound ("phone-home") behaviour the tool can perform, disclosed
    /// up front (`verify:privacy` parity). Always lists the auto-update check.
    pub phone_home: Vec<String>,
}

impl DoctorReport {
    /// Most severe verdict across all checks, or [`Health::Ok`] when empty.
    #[must_use]
    pub fn worst(&self) -> Health {
        self.checks
            .iter()
            .map(|c| c.health)
            .max()
            .unwrap_or(Health::Ok)
    }

    /// Render the report as aligned plain text suitable for a terminal.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "doctor: {}", self.worst().label());
        for c in &self.checks {
            let _ = writeln!(out, "  [{}] {}: {}", c.health.label(), c.name, c.detail);
        }
        out.push_str("\nprivacy — outbound behaviours:\n");
        if self.phone_home.is_empty() {
            out.push_str("  (none)\n");
        } else {
            for p in &self.phone_home {
                let _ = writeln!(out, "  - {p}");
            }
        }
        out
    }

    /// Serialize the report to a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`DoctorError::Serialize`] if serialization fails (e.g. the
    /// underlying writer/encoder errors).
    pub fn to_json(&self) -> Result<String, DoctorError> {
        serde_json::to_string_pretty(self).map_err(|e| DoctorError::Serialize(e.to_string()))
    }
}

/// Errors produced by this crate.
#[derive(Debug, thiserror::Error)]
pub enum DoctorError {
    /// JSON serialization of a [`DoctorReport`] failed.
    #[error("failed to serialize doctor report: {0}")]
    Serialize(String),
}

/// The outbound behaviours `origin` can perform, disclosed for `verify:privacy`.
///
/// This is intentionally a constant list so the disclosure cannot silently drift
/// from the actual behaviour set.
#[must_use]
pub fn phone_home_disclosures() -> Vec<String> {
    vec![
        "npm auto-update check (disable with ORIGINX_NO_UPDATE=1)".to_string(),
        "model/provider API requests to the endpoints you configure".to_string(),
        "optional telemetry (opt-in; off unless you enable it)".to_string(),
    ]
}

/// Run diagnostics over `inputs`, deriving a verdict per check.
///
/// The returned [`DoctorReport::worst`] reflects the most severe single check.
/// The privacy disclosure is always populated (see [`phone_home_disclosures`]).
#[must_use]
pub fn diagnose(inputs: &DoctorInputs) -> DoctorReport {
    let checks = vec![
        check_toolchain(inputs.rust_version.as_deref()),
        check_config(inputs.config_present),
        check_daemon(inputs.daemon_running),
        check_providers(&inputs.providers_configured),
        check_home(inputs.writable_home),
        check_network(inputs.network_ok),
    ];

    DoctorReport {
        checks,
        phone_home: phone_home_disclosures(),
    }
}

fn check_toolchain(version: Option<&str>) -> Check {
    version.map_or_else(
        || {
            Check::new(
                "toolchain",
                Health::Warn,
                "could not detect a Rust toolchain version",
            )
        },
        |v| match parse_major_minor(v) {
            None => Check::new(
                "toolchain",
                Health::Warn,
                format!("unrecognized Rust version string: {v}"),
            ),
            Some(parsed) if parsed >= MIN_RUST_VERSION => Check::new(
                "toolchain",
                Health::Ok,
                format!(
                    "Rust {v} (>= MSRV {}.{})",
                    MIN_RUST_VERSION.0, MIN_RUST_VERSION.1
                ),
            ),
            Some(_) => Check::new(
                "toolchain",
                Health::Fail,
                format!(
                    "Rust {v} is older than the required MSRV {}.{}",
                    MIN_RUST_VERSION.0, MIN_RUST_VERSION.1
                ),
            ),
        },
    )
}

fn check_config(present: bool) -> Check {
    if present {
        Check::new("config", Health::Ok, "config file found")
    } else {
        Check::new(
            "config",
            Health::Warn,
            "no config file found; running with defaults",
        )
    }
}

fn check_daemon(running: bool) -> Check {
    if running {
        Check::new("daemon", Health::Ok, "daemon is reachable")
    } else {
        Check::new(
            "daemon",
            Health::Warn,
            "daemon not running; it will be started on demand",
        )
    }
}

fn check_providers(providers: &[String]) -> Check {
    if providers.is_empty() {
        Check::new(
            "providers",
            Health::Fail,
            "no providers configured; configure at least one to send requests",
        )
    } else {
        Check::new(
            "providers",
            Health::Ok,
            format!("{} provider(s) configured: {}", providers.len(), providers.join(", ")),
        )
    }
}

fn check_home(writable: bool) -> Check {
    if writable {
        Check::new("home", Health::Ok, "home/config directory is writable")
    } else {
        Check::new(
            "home",
            Health::Fail,
            "home/config directory is not writable; sessions cannot persist",
        )
    }
}

fn check_network(network_ok: Option<bool>) -> Check {
    match network_ok {
        Some(true) => Check::new("network", Health::Ok, "outbound connectivity verified"),
        Some(false) => Check::new(
            "network",
            Health::Fail,
            "outbound connectivity probe failed",
        ),
        None => Check::new(
            "network",
            Health::Warn,
            "connectivity not checked (offline or skipped)",
        ),
    }
}

/// Parse the leading `major.minor` of a version string like `1.83.0` or
/// `1.85.0-nightly`. Returns `None` if the first two components are not numeric.
fn parse_major_minor(v: &str) -> Option<(u64, u64)> {
    let core = v.trim().split(['-', '+', ' ']).next().unwrap_or(v);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    Some((major, minor))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn all_ok() -> DoctorInputs {
        DoctorInputs {
            rust_version: Some("1.83.0".to_string()),
            config_present: true,
            daemon_running: true,
            providers_configured: vec!["anthropic".to_string()],
            writable_home: true,
            network_ok: Some(true),
        }
    }

    #[test]
    fn all_ok_inputs_yield_ok() {
        let report = diagnose(&all_ok());
        assert_eq!(report.worst(), Health::Ok);
        assert!(report.checks.iter().all(|c| c.health == Health::Ok));
        assert_eq!(report.checks.len(), 6);
    }

    #[test]
    fn missing_config_warns_but_does_not_fail() {
        let mut inputs = all_ok();
        inputs.config_present = false;
        let report = diagnose(&inputs);
        assert_eq!(report.worst(), Health::Warn);
        let config = report.checks.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(config.health, Health::Warn);
    }

    #[test]
    fn no_providers_fails() {
        let mut inputs = all_ok();
        inputs.providers_configured.clear();
        let report = diagnose(&inputs);
        assert_eq!(report.worst(), Health::Fail);
        let providers = report.checks.iter().find(|c| c.name == "providers").unwrap();
        assert_eq!(providers.health, Health::Fail);
    }

    #[test]
    fn old_toolchain_fails_unknown_warns() {
        let mut inputs = all_ok();
        inputs.rust_version = Some("1.70.0".to_string());
        assert_eq!(diagnose(&inputs).worst(), Health::Fail);

        inputs.rust_version = Some("not-a-version".to_string());
        let report = diagnose(&inputs);
        let tc = report.checks.iter().find(|c| c.name == "toolchain").unwrap();
        assert_eq!(tc.health, Health::Warn);

        inputs.rust_version = None;
        let tc_none = diagnose(&inputs);
        let tc = tc_none.checks.iter().find(|c| c.name == "toolchain").unwrap();
        assert_eq!(tc.health, Health::Warn);

        inputs.rust_version = Some("1.85.0-nightly".to_string());
        let report = diagnose(&inputs);
        let tc = report.checks.iter().find(|c| c.name == "toolchain").unwrap();
        assert_eq!(tc.health, Health::Ok);
    }

    #[test]
    fn unwritable_home_and_failed_network_fail() {
        let mut inputs = all_ok();
        inputs.writable_home = false;
        assert_eq!(diagnose(&inputs).worst(), Health::Fail);

        let mut inputs = all_ok();
        inputs.network_ok = Some(false);
        assert_eq!(diagnose(&inputs).worst(), Health::Fail);

        let mut inputs = all_ok();
        inputs.network_ok = None;
        let report = diagnose(&inputs);
        let net = report.checks.iter().find(|c| c.name == "network").unwrap();
        assert_eq!(net.health, Health::Warn);
    }

    #[test]
    fn phone_home_always_lists_auto_update() {
        // Even with everything failing, the disclosure is populated.
        let inputs = DoctorInputs::default();
        let report = diagnose(&inputs);
        assert!(!report.phone_home.is_empty());
        assert!(report
            .phone_home
            .iter()
            .any(|p| p.contains("auto-update") && p.contains("ORIGINX_NO_UPDATE=1")));
    }

    #[test]
    fn json_round_trips() {
        let report = diagnose(&all_ok());
        let json = report.to_json().unwrap();
        let parsed: DoctorReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, report);
    }

    #[test]
    fn text_output_includes_verdict_and_privacy_section() {
        let report = diagnose(&all_ok());
        let text = report.to_text();
        assert!(text.contains("doctor: OK"));
        assert!(text.contains("[OK] config:"));
        assert!(text.contains("privacy — outbound behaviours:"));
        assert!(text.contains("auto-update"));
    }

    #[test]
    fn worst_is_ordered_and_empty_is_ok() {
        assert!(Health::Fail > Health::Warn);
        assert!(Health::Warn > Health::Ok);
        let empty = DoctorReport {
            checks: Vec::new(),
            phone_home: Vec::new(),
        };
        assert_eq!(empty.worst(), Health::Ok);
    }

    #[test]
    fn parse_major_minor_handles_suffixes() {
        assert_eq!(parse_major_minor("1.83.0"), Some((1, 83)));
        assert_eq!(parse_major_minor("1.85.0-nightly"), Some((1, 85)));
        assert_eq!(parse_major_minor("1.83"), Some((1, 83)));
        assert_eq!(parse_major_minor("garbage"), None);
    }
}
