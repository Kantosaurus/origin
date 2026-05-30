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

/// Print the morning report for the most recent ambient session plus the
/// standing overnight plan.
///
/// With no daemon ambient loop active yet (it is gated behind `ORIGIN_AMBIENT=1`
/// and deferred), there is no persisted session, so the report renders its
/// "nothing ran overnight" digest. The standing plan that the loop *would* run
/// is derived from [`origin_ambient::next_task`] so users can see the rotation.
fn report() {
    // No persisted overnight session yet -> an empty morning report.
    let morning = origin_ambient::MorningReport::new(Vec::new(), 0, Vec::new());
    print!("{}", morning.to_markdown());

    // Show the standing overnight plan the daemon loop would run (the task
    // rotation), so the surface is informative even before the loop ships.
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let next = origin_ambient::next_task(&tasks);
        tasks.push(next);
    }
    println!("\n## Standing overnight plan (when ORIGIN_AMBIENT=1)\n");
    for t in &tasks {
        println!("- {}", t.slug());
    }
    println!(
        "\n(The autonomous ambient/overnight loop is gated behind ORIGIN_AMBIENT=1 \
         and is not yet wired; this report is read-only.)"
    );
}
