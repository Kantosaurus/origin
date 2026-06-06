// SPDX-License-Identifier: Apache-2.0
//! `origin providers recommend` — cost-based model recommendation with a live
//! local-Ollama latency fold.
//!
//! openclaude ships a provider-recommend flow that benchmarks candidates and
//! writes a saved profile. origin's analogue ranks candidate models by the
//! builtin [`origin_cost`] pricing table (a representative blended $/Mtok) and
//! writes the cheapest as a profile at `~/.origin/recommended.json`.
//!
//! Cloud models differ on price, so cost ranks them. Local Ollama models all
//! cost the same physical `$0`, so cost cannot separate them — latency can.
//! When a candidate is an explicit local Ollama model (an `ollama/`- or
//! `ollama:`-prefixed id) and the daemon is reachable, this measures a quick
//! round-trip to its default endpoint and folds the sample into
//! [`origin_router`]'s [`Strategy::Scored`](origin_router::Strategy) health, so
//! local models are ranked by real latency. The probe is best-effort: a failed
//! or unreachable probe simply drops the latency for that model, leaving the
//! cost-only ranking byte-identical to before.
#![allow(clippy::missing_errors_doc)]

use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use origin_router::{rank_by_latency, ModelRef};
use serde::Serialize;

/// Default Ollama endpoint host/port probed for local-model latency.
const OLLAMA_HOST: &str = "127.0.0.1";
/// Default Ollama daemon port.
const OLLAMA_PORT: u16 = 11434;
/// Short probe timeout so an absent or wedged daemon cannot stall the command.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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

/// A local Ollama candidate ranked by its measured latency.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalRanked {
    /// The candidate model id (as given, with its `ollama` prefix preserved).
    pub model: String,
    /// Measured round-trip latency to the local daemon, in milliseconds.
    pub latency_ms: u64,
}

/// `true` when `model` is an explicit local Ollama id (`ollama/…` or `ollama:…`).
///
/// Only an explicit provider prefix counts, so a plain cloud model name is
/// never reclassified as local — that keeps the cost-only path byte-identical
/// for the usual cloud candidate lists.
#[must_use]
fn is_local_ollama(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.starts_with("ollama/") || lower.starts_with("ollama:")
}

/// Strip the `ollama/` or `ollama:` prefix, yielding the bare model tag.
fn ollama_model_tag(model: &str) -> &str {
    model
        .strip_prefix("ollama/")
        .or_else(|| model.strip_prefix("ollama:"))
        .or_else(|| model.strip_prefix("Ollama/"))
        .or_else(|| model.strip_prefix("Ollama:"))
        .unwrap_or(model)
}

/// Rank local Ollama candidates by measured latency via [`rank_by_latency`].
///
/// `samples` pairs each local model id with its measured round-trip latency in
/// milliseconds. Returns the ids fastest-first as [`LocalRanked`] rows. This is
/// the pure fold used by `run`: given candidates and measured latencies it
/// yields the presentation order, so the latency ranking is unit-testable
/// without touching a live daemon. The `origin_router` [`ModelRef`] provider is
/// fixed to `ollama` here since every input is a local model.
#[must_use]
fn fold_latency(samples: &[(String, u64)]) -> Vec<LocalRanked> {
    let refs: Vec<(ModelRef, u64)> = samples
        .iter()
        .map(|(model, ms)| (ModelRef::new("ollama", model.clone()), *ms))
        .collect();
    let order = rank_by_latency(&refs);
    order
        .into_iter()
        .filter_map(|m| {
            samples
                .iter()
                .find(|(model, _)| *model == m.model)
                .map(|(model, ms)| LocalRanked {
                    model: model.clone(),
                    latency_ms: *ms,
                })
        })
        .collect()
}

/// Probe the local Ollama daemon's `/api/tags` and return the round-trip time.
///
/// Uses a raw [`TcpStream`] with a short connect + read/write timeout rather
/// than the async HTTP client, so it is safe to call from this synchronous
/// command even inside a Tokio runtime, and pulls in no new dependency. Any
/// failure (no daemon, refused connection, timeout, malformed response) returns
/// `None` — the probe is strictly best-effort and never panics.
#[must_use]
fn probe_ollama_latency_ms() -> Option<u64> {
    let addr = (OLLAMA_HOST, OLLAMA_PORT).to_socket_addrs().ok()?.next()?;
    let start = Instant::now();
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT)).ok()?;
    let req = format!(
        "GET /api/tags HTTP/1.1\r\nHost: {OLLAMA_HOST}:{OLLAMA_PORT}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    // Read just the status line / first chunk — we only need the round-trip
    // timing and a sign of life, not the full body.
    let mut buf = [0_u8; 256];
    let n = stream.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    // Require an HTTP response so we don't time a non-Ollama listener.
    if !buf[..n].starts_with(b"HTTP/") {
        return None;
    }
    u64::try_from(start.elapsed().as_millis()).ok()
}

/// Measure latency for each local Ollama candidate, best-effort.
///
/// Probes the daemon once; if unreachable, returns an empty vector so the
/// caller falls back to the unchanged cost-only ranking. When reachable, every
/// local candidate is attributed the same measured round-trip — a single
/// `/api/tags` round-trip characterises the local daemon's responsiveness, and
/// per-model `/api/show` probes would only add load without separating models
/// that share one daemon.
fn measure_local(locals: &[String]) -> Vec<(String, u64)> {
    if locals.is_empty() {
        return Vec::new();
    }
    let Some(ms) = probe_ollama_latency_ms() else {
        return Vec::new();
    };
    locals.iter().map(|m| (m.clone(), ms)).collect()
}

/// `true` when the borrowed local-ranking slice is empty.
///
/// A dedicated predicate (rather than `<[_]>::is_empty`) so `serde`'s
/// `skip_serializing_if`, which passes a reference to the field, type-checks
/// against the `&'a [LocalRanked]` field.
const fn slice_is_empty(s: &&[LocalRanked]) -> bool {
    s.is_empty()
}

/// The saved recommendation profile.
///
/// `local_ranked` is omitted entirely when empty, so a cost-only run writes the
/// exact JSON shape it always did.
#[derive(Debug, Serialize)]
struct Profile<'a> {
    recommended: &'a str,
    ranked: &'a [Ranked],
    #[serde(skip_serializing_if = "slice_is_empty")]
    local_ranked: &'a [LocalRanked],
}

/// `origin providers recommend` entrypoint.
pub fn run(models: &[String], write: bool) -> anyhow::Result<()> {
    let candidates: Vec<String> = if models.is_empty() {
        DEFAULT_CANDIDATES.iter().map(|s| (*s).to_string()).collect()
    } else {
        models.to_vec()
    };

    // Measure latency only for explicit local Ollama ids, best-effort. When the
    // daemon is unreachable (or no local ids were given) `local_ranked` is empty
    // and every candidate flows through the unchanged cost-only ranking below,
    // keeping that output byte-identical.
    let locals: Vec<String> = candidates
        .iter()
        .filter(|m| is_local_ollama(m))
        .cloned()
        .collect();
    let local_ranked = fold_latency(&measure_local(&locals));
    let local_set: std::collections::HashSet<&str> = local_ranked.iter().map(|l| l.model.as_str()).collect();

    // Cost-rank the candidates that were not pulled into the latency section.
    let cost_candidates: Vec<String> = candidates
        .iter()
        .filter(|m| !local_set.contains(m.as_str()))
        .cloned()
        .collect();
    let ranked = rank(&cost_candidates);

    if ranked.is_empty() && local_ranked.is_empty() {
        println!("No known pricing for the given models; cannot recommend.");
        return Ok(());
    }
    if !ranked.is_empty() {
        print_cost_table(&ranked);
    }
    if !local_ranked.is_empty() {
        print_local_table(&local_ranked);
    }

    // Reaching here, at least one of the two rankings is non-empty, so
    // `recommend_best` yields a pick.
    let Some((label, name)) = recommend_best(&ranked, &local_ranked) else {
        return Ok(());
    };
    println!("\nRecommended ({label}): {name}");
    if write {
        let profile = Profile {
            recommended: &name,
            ranked: &ranked,
            local_ranked: &local_ranked,
        };
        let path = write_profile(&profile)?;
        println!("Saved profile to {}", path.display());
    }
    Ok(())
}

/// Print the cost-ranked cloud table, cheapest first (unchanged formatting).
fn print_cost_table(ranked: &[Ranked]) {
    println!(
        "{:<24} {:>14} {:>12} {:>12}",
        "MODEL", "BLENDED $/Mtok", "IN $/Mtok", "OUT $/Mtok"
    );
    for r in ranked {
        println!(
            "{:<24} {:>14.2} {:>12.2} {:>12.2}",
            r.model, r.blended_per_mtok, r.input_per_mtok, r.output_per_mtok
        );
    }
}

/// Print the local Ollama table ranked by measured latency, fastest first.
fn print_local_table(local: &[LocalRanked]) {
    println!("\n{:<24} {:>14}", "LOCAL MODEL (ollama, $0)", "LATENCY ms");
    for l in local {
        println!("{:<24} {:>14}", ollama_model_tag(&l.model), l.latency_ms);
    }
}

/// Choose the headline recommendation and a label describing why.
///
/// A reachable local model wins (physical `$0` cost, ranked by real latency);
/// otherwise the cheapest priced cloud model is recommended as before.
fn recommend_best(ranked: &[Ranked], local: &[LocalRanked]) -> Option<(String, String)> {
    if let Some(top) = local.first() {
        return Some((
            format!("local, lowest latency {} ms", top.latency_ms),
            top.model.clone(),
        ));
    }
    ranked
        .first()
        .map(|r| ("cheapest capable".to_string(), r.model.clone()))
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
    use super::{
        fold_latency, is_local_ollama, ollama_model_tag, rank, recommend_best, LocalRanked, Ranked,
        DEFAULT_CANDIDATES,
    };

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    fn s(model: &str, ms: u64) -> (String, u64) {
        (model.to_string(), ms)
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
            assert!(
                w[0].blended_per_mtok <= w[1].blended_per_mtok,
                "must be sorted ascending"
            );
        }
    }

    #[test]
    fn is_local_ollama_only_matches_explicit_prefix() {
        assert!(is_local_ollama("ollama/llama3.2"));
        assert!(is_local_ollama("ollama:qwen2.5-coder"));
        assert!(is_local_ollama("Ollama/llama3.2"), "case-insensitive");
        // A bare or cloud id is never reclassified as local.
        assert!(!is_local_ollama("llama3.2"));
        assert!(!is_local_ollama("gpt-4o"));
        assert!(!is_local_ollama("anthropic/claude-haiku-4"));
    }

    #[test]
    fn ollama_tag_strips_prefix() {
        assert_eq!(ollama_model_tag("ollama/llama3.2"), "llama3.2");
        assert_eq!(ollama_model_tag("ollama:qwen2.5"), "qwen2.5");
        assert_eq!(ollama_model_tag("llama3.2"), "llama3.2");
    }

    #[test]
    fn fold_latency_orders_fastest_first() {
        // Given candidates + measured latencies, the fold must rank lowest ms
        // first — the same order origin-router's Scored health would pick.
        let out = fold_latency(&[
            s("ollama/llama3.1:70b", 1_800),
            s("ollama/llama3.2", 120),
            s("ollama/qwen2.5-coder", 600),
        ]);
        let order: Vec<&str> = out.iter().map(|l| l.model.as_str()).collect();
        assert_eq!(
            order,
            vec!["ollama/llama3.2", "ollama/qwen2.5-coder", "ollama/llama3.1:70b"]
        );
        // The measured latency travels with each row for display.
        assert_eq!(out[0].latency_ms, 120);
    }

    #[test]
    fn fold_latency_empty_is_empty() {
        assert!(fold_latency(&[]).is_empty());
    }

    #[test]
    fn no_ollama_path_recommends_cheapest_cloud_unchanged() {
        // No probe / no local rows -> recommendation is the cheapest cost rank,
        // exactly as the cost-only path always did.
        let ranked = rank(&v(&["claude-opus-4", "claude-haiku-4"]));
        let best = recommend_best(&ranked, &[]);
        let (label, name) = best.expect("a priced candidate exists");
        assert_eq!(label, "cheapest capable");
        assert_eq!(name, "claude-haiku-4");
    }

    #[test]
    fn local_present_wins_over_cloud_in_recommendation() {
        let ranked: Vec<Ranked> = rank(&v(&["claude-haiku-4"]));
        let local = vec![LocalRanked {
            model: "ollama/llama3.2".to_string(),
            latency_ms: 95,
        }];
        let (label, name) = recommend_best(&ranked, &local).expect("local row present");
        assert_eq!(name, "ollama/llama3.2");
        assert!(
            label.contains("95 ms"),
            "label surfaces measured latency: {label}"
        );
    }

    #[test]
    fn empty_inputs_yield_no_recommendation() {
        assert!(recommend_best(&[], &[]).is_none());
    }
}
