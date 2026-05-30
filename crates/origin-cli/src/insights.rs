// SPDX-License-Identifier: Apache-2.0
//! `origin insights` subcommand: a per-session cost / usage report.
//!
//! Reuses the existing daemon `GetUsage` path by delegating to
//! [`crate::admin::usage`], then appends a session insights footer.
//!
//! This is an additive subcommand — `origin usage` itself is unchanged. The
//! delegated table already routes through the daemon `GetUsage` IPC path and the
//! `origin-cost` pricing/COST column, so the only new output is the footer.

use anyhow::Result;

/// Run `origin insights`.
///
/// Prints the per-provider/model usage + cost table (reusing the `origin usage`
/// rendering, which already routes through the daemon `GetUsage` IPC path and
/// the `origin-cost` pricing table), followed by a one-line session insights
/// footer.
///
/// # Errors
/// Propagates any IPC / decode error from the underlying usage path.
pub async fn run() -> Result<()> {
    // Reuse the verified GetUsage path + COST-column rendering.
    crate::admin::usage().await?;
    println!();
    println!("Session insights");
    println!("================");
    println!(
        "Tip: keep turns close together so Anthropic's 5-minute prompt cache stays warm - \
         a cold cache means recent input is billed at the full (uncached) input rate. \
         The COST column above already reflects cache-read vs full-input pricing."
    );
    Ok(())
}
