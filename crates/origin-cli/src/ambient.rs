// SPDX-License-Identifier: Apache-2.0
//! `origin ambient …` subcommand handlers.
//!
//! Surfaces the [`origin_ambient`] morning-report + overnight-plan foundation
//! (jcode Ambient/OpenClaw + Overnight parity). The autonomous daemon-side
//! ambient/overnight loop is gated behind `ORIGIN_AMBIENT=1` and is deferred;
//! this subcommand is read-only and additive — rendering a report schedules no
//! work and consumes no budget.
#![allow(clippy::missing_errors_doc, clippy::unnecessary_wraps)]

use anyhow::Result;

use crate::cli_def::AmbientSub;

/// Dispatch an `origin ambient …` subcommand.
pub fn run(sub: &AmbientSub) -> Result<()> {
    match sub {
        AmbientSub::Report => {
            report();
            Ok(())
        }
    }
}

/// Print the morning report for the most recent overnight session plus the
/// standing overnight plan.
///
/// When the daemon's overnight driver (`ORIGIN_OVERNIGHT=1`) has run, it persists
/// a [`MorningReport`](origin_ambient::MorningReport) to
/// `~/.origin/overnight/latest.json`; this command loads and renders it. With no
/// persisted session it renders the "nothing ran overnight" digest. Either way
/// the standing task rotation is shown so the surface is informative.
fn report() {
    let morning = load_persisted_report()
        .unwrap_or_else(|| origin_ambient::MorningReport::new(Vec::new(), 0, Vec::new()));
    print!("{}", morning.to_markdown());

    // Show the standing overnight plan the daemon loop would run (the task
    // rotation), so the surface is informative even before a session has run.
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let next = origin_ambient::next_task(&tasks);
        tasks.push(next);
    }
    println!("\n## Standing overnight plan (run with ORIGIN_OVERNIGHT=1)\n");
    for t in &tasks {
        println!("- {}", t.slug());
    }
    println!(
        "\n(The autonomous ambient loop is `ORIGIN_AMBIENT=1`; the windowed \
         overnight driver is `ORIGIN_OVERNIGHT=1`. This report is read-only.)"
    );
}

/// Load the most recent persisted overnight morning report, if any. Honors
/// `$ORIGIN_HOME` (used by tests) and falls back to the user home directory.
fn load_persisted_report() -> Option<origin_ambient::MorningReport> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)?;
    let path = home.join(".origin").join("overnight").join("latest.json");
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}
