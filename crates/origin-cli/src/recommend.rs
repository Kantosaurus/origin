// SPDX-License-Identifier: Apache-2.0
//! `origin providers recommend` — cost-based model recommendation.
//!
//! openclaude ships a provider-recommend flow that benchmarks candidates and
//! writes a saved profile. origin's analogue ranks candidate models by the
//! builtin [`origin_cost`] pricing table (a representative blended $/Mtok) and
//! writes the cheapest as a profile at `~/.origin/recommended.json`. A live
//! latency / local-Ollama benchmark layered on `origin-router`'s `Scored` health
//! is a follow-up; this is the pure, offline half.
#![allow(clippy::missing_errors_doc)]

use std::path::PathBuf;

use serde::Serialize;

/// Default candidates spanning the major families when the user names none.
const DEFAULT_CANDIDATES: &[&str] = &[
    "claude-opus-4",
    "claude-sonnet-4",
    "claude-haiku-4",
    "gpt-4o",
    "gpt-4o-mini",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
];

/// A priced, ranked candidate.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Ranked {
    /// The candidate model id (as given).
    pub model: String,
    /// Representative blended cost used for ranking (lower is cheaper).
    pub blended_per_mtok: f64,
    /// List input rate, USD per million tokens.
    pub input_per_mtok: f64,
    /// List output rate, USD per million tokens.
    pub output_per_mtok: f64,
}

/// Rank `models` cheapest-first by a representative blended cost.
///
/// An agentic turn is input-heavy (the full context is re-sent each turn), so we
/// weight input 3:1 over output. Models with no known price are dropped (a model
/// we cannot price cannot be honestly recommended).
#[must_use]
pub fn rank(models: &[String]) -> Vec<Ranked> {
    let mut out: Vec<Ranked> = models
        .iter()
        .filter_map(|m| {
            origin_cost::price_for(m).map(|p| Ranked {
                model: m.clone(),
                blended_per_mtok: p.input_per_mtok.mul_add(3.0, p.output_per_mtok),
                input_per_mtok: p.input_per_mtok,
                output_per_mtok: p.output_per_mtok,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.blended_per_mtok
            .partial_cmp(&b.blended_per_mtok)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// The saved recommendation profile.
#[derive(Debug, Serialize)]
struct Profile<'a> {
    recommended: &'a str,
    ranked: &'a [Ranked],
}

/// `origin providers recommend` entrypoint.
pub fn run(models: &[String], write: bool) -> anyhow::Result<()> {
    let candidates: Vec<String> = if models.is_empty() {
        DEFAULT_CANDIDATES.iter().map(|s| (*s).to_string()).collect()
    } else {
        models.to_vec()
    };
    let ranked = rank(&candidates);
    if ranked.is_empty() {
        println!("No known pricing for the given models; cannot recommend.");
        return Ok(());
    }
    println!(
        "{:<24} {:>14} {:>12} {:>12}",
        "MODEL", "BLENDED $/Mtok", "IN $/Mtok", "OUT $/Mtok"
    );
    for r in &ranked {
        println!(
            "{:<24} {:>14.2} {:>12.2} {:>12.2}",
            r.model, r.blended_per_mtok, r.input_per_mtok, r.output_per_mtok
        );
    }
    let best = ranked[0].model.clone();
    println!("\nRecommended (cheapest capable): {best}");
    if write {
        let profile = Profile {
            recommended: &best,
            ranked: &ranked,
        };
        let path = write_profile(&profile)?;
        println!("Saved profile to {}", path.display());
    }
    Ok(())
}

fn write_profile(profile: &Profile) -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
    let dir = home.join(".origin");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("recommended.json");
    std::fs::write(&path, serde_json::to_string_pretty(profile)?)?;
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::{rank, DEFAULT_CANDIDATES};

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn ranks_cheapest_first() {
        let r = rank(&v(&["claude-opus-4", "claude-haiku-4", "claude-sonnet-4"]));
        assert_eq!(r.len(), 3);
        // Haiku is cheapest, Opus dearest.
        assert_eq!(r[0].model, "claude-haiku-4");
        assert_eq!(r[2].model, "claude-opus-4");
        assert!(r[0].blended_per_mtok < r[1].blended_per_mtok);
        assert!(r[1].blended_per_mtok < r[2].blended_per_mtok);
    }

    #[test]
    fn unknown_models_are_dropped() {
        let r = rank(&v(&["claude-haiku-4", "totally-made-up-model-xyz"]));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].model, "claude-haiku-4");
    }

    #[test]
    fn provider_prefix_is_accepted() {
        // price_for ignores a provider prefix; ranking should still price it.
        let r = rank(&v(&["anthropic/claude-haiku-4"]));
        assert_eq!(r.len(), 1);
        assert!(r[0].blended_per_mtok > 0.0);
    }

    #[test]
    fn default_candidates_are_all_priced_and_ordered() {
        let r = rank(&v(DEFAULT_CANDIDATES));
        assert_eq!(r.len(), DEFAULT_CANDIDATES.len(), "every default must be priced");
        for w in r.windows(2) {
            assert!(w[0].blended_per_mtok <= w[1].blended_per_mtok, "must be sorted ascending");
        }
    }
}
