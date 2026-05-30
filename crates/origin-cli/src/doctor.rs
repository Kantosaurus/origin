// SPDX-License-Identifier: Apache-2.0
//! `origin doctor` — environment & runtime diagnostics plus a privacy
//! disclosure.
//!
//! The CLI gathers real environment facts (toolchain, config, daemon socket,
//! configured providers, home writability) and hands them to the pure
//! [`origin_doctor`] crate, which derives the health verdicts and the
//! phone-home disclosure (openclaude `doctor:runtime` / `verify:privacy`).

use std::time::Duration;

use anyhow::Result;
use origin_doctor::{diagnose, phone_home_disclosures, DoctorInputs, Health};

/// Run diagnostics. With `privacy`, prints only the phone-home disclosure.
///
/// # Errors
/// Returns if JSON rendering fails. Probe failures are reported as checks, not
/// hard errors.
pub async fn run(json: bool, privacy: bool) -> Result<()> {
    if privacy {
        if json {
            let body = serde_json::to_string_pretty(&phone_home_disclosures())?;
            println!("{body}");
        } else {
            println!("Privacy disclosure — origin's outbound behaviour:");
            for line in phone_home_disclosures() {
                println!("  • {line}");
            }
        }
        return Ok(());
    }

    let inputs = gather().await;
    let report = diagnose(&inputs);
    if json {
        println!("{}", report.to_json()?);
    } else {
        println!("{}", report.to_text());
    }
    Ok(())
}

/// Collect environment facts for the diagnosis. Never panics; missing data maps
/// to the conservative value (e.g. `None`/`false`).
async fn gather() -> DoctorInputs {
    let rust_version = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let config_present = crate::config::path().is_ok_and(|p| p.exists());

    let providers_configured = crate::config::load()
        .ok()
        .flatten()
        .map(|cfg| {
            let mut ps = vec![cfg.primary.provider];
            if let Some(b) = cfg.backup {
                ps.push(b.provider);
            }
            if let Some(s) = cfg.subagent {
                ps.push(s.provider);
            }
            ps.sort();
            ps.dedup();
            ps
        })
        .unwrap_or_default();

    let writable_home = probe_writable_home();
    let daemon_running = probe_daemon().await;

    DoctorInputs {
        rust_version,
        config_present,
        daemon_running,
        providers_configured,
        writable_home,
        // origin's doctor does NOT make network calls by default (privacy);
        // network reachability is left unknown.
        network_ok: None,
    }
}

/// Attempt a probe-write into `~/.origin` to confirm the home is writable.
fn probe_writable_home() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let dir = home.join(".origin");
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let probe = dir.join(".doctor-probe");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Try to connect to the daemon socket with a short timeout.
async fn probe_daemon() -> bool {
    let path = crate::admin::socket_path();
    matches!(
        tokio::time::timeout(
            Duration::from_millis(400),
            origin_ipc::transport::Connector::connect(&path),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Process exit code derived from the worst check: 0 = ok/warn, 2 = fail.
#[must_use]
pub const fn exit_code(worst: Health) -> i32 {
    match worst {
        Health::Ok | Health::Warn => 0,
        Health::Fail => 2,
    }
}
