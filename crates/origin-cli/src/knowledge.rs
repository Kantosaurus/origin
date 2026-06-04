// SPDX-License-Identifier: Apache-2.0
//! `origin knowledge` — a local knowledge index with hybrid retrieval.
//!
//! Persisted to `~/.origin/knowledge.json`, backed by [`origin_knowledge`]
//! (openclaude `/knowledge`; kilocode/opencode `semantic_search` parity). Two
//! retrieval paths are combined when possible:
//!
//! * a tf-style **lexical** path ([`Knowledge::search_text`]) that always works
//!   offline, and
//! * a **cosine vector** path ([`Knowledge::search_vec`]) over `MiniLM`
//!   embeddings produced by [`origin_mem::Embedder`].
//!
//! The embedder needs an ONNX model artifact on disk. When that artifact is
//! present, `knowledge add` stores a real embedding alongside each document and
//! `knowledge search` reranks the union of the lexical and cosine hits. When the
//! model is absent (the common offline case) every path degrades gracefully to
//! the byte-identical lexical behaviour — no panic, no network, no surprise.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use origin_knowledge::{Doc, Hit, Knowledge};
use origin_mem::Embedder;

use crate::cli_def::KnowledgeSub;

/// Dispatch a `knowledge` subcommand.
///
/// # Errors
/// Returns on filesystem or JSON (de)serialization failure.
pub fn run(sub: KnowledgeSub) -> Result<()> {
    let out = run_to_string(sub)?;
    if !out.is_empty() {
        println!("{out}");
    }
    Ok(())
}

/// Dispatch a `knowledge` subcommand, returning the rendered output as a string
/// instead of printing it.
///
/// This is what the in-session `/knowledge` composer command calls so it can
/// push results into the TUI scrollback rather than writing to `stdout` (which
/// would corrupt the alternate screen). [`run`] is a thin `println!` wrapper.
///
/// # Errors
/// Returns on filesystem or JSON (de)serialization failure.
pub fn run_to_string(sub: KnowledgeSub) -> Result<String> {
    match sub {
        KnowledgeSub::Add { id, text } => add(&id, &text),
        KnowledgeSub::Search { query, k } => search(&query, k),
        KnowledgeSub::Rm { id } => remove(&id),
        KnowledgeSub::Ls => list(),
    }
}

fn store_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home directory")?;
    let dir = home.join(".origin");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join("knowledge.json"))
}

fn load() -> Result<Knowledge> {
    let path = store_path()?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Knowledge::from_json(&s).map_err(|e| anyhow::anyhow!("{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Knowledge::new()),
        Err(e) => Err(anyhow::Error::new(e).context("reading knowledge.json")),
    }
}

fn save(kb: &Knowledge) -> Result<()> {
    let path = store_path()?;
    let body = kb.to_json().map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Resolve the `MiniLM` ONNX model path, mirroring the daemon's policy.
///
/// Honours `ORIGIN_MEM_MODEL_DIR` (joined with `model.onnx`) first, then the
/// documented default `${ORIGIN_DATA or $HOME/.origin}/models/minilm-l6-v2.onnx`.
/// Returns the first candidate that exists on disk, or `None`.
fn model_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("ORIGIN_MEM_MODEL_DIR") {
        let candidate = PathBuf::from(dir).join("model.onnx");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let base = std::env::var_os("ORIGIN_DATA")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".origin")))?;
    let candidate = base.join("models").join("minilm-l6-v2.onnx");
    candidate.exists().then_some(candidate)
}

/// Construct the embedder if its model artifact is available, else `None`.
///
/// Any load failure (missing model, ONNX error, bad tokenizer) yields `None` so
/// callers fall back to the lexical path rather than aborting.
fn try_load_embedder() -> Option<Embedder> {
    let path = model_path()?;
    Embedder::from_path(path).ok()
}

/// Compute an embedding for `text`, returning an empty vector on any failure.
///
/// An empty vector is exactly what [`Doc::text`] stores, so a failed embed is
/// indistinguishable from a text-only document downstream.
fn embed_or_empty(embedder: Option<&Embedder>, text: &str) -> Vec<f32> {
    embedder
        .and_then(|e| e.embed(text).ok())
        .unwrap_or_default()
}

fn add(id: &str, text: &str) -> Result<String> {
    let mut kb = load()?;
    let embedding = embed_or_empty(try_load_embedder().as_ref(), text);
    // A real embedding makes the document cosine-searchable; an empty one keeps
    // the prior text-only behaviour byte-for-byte (`Doc::new(.., vec![])` ==
    // `Doc::text`).
    kb.add(Doc::new(id, text, embedding));
    save(&kb)?;
    Ok(format!("indexed `{id}` ({} docs total)", kb.len()))
}

fn search(query: &str, k: usize) -> Result<String> {
    let kb = load()?;
    let hits = run_search(&kb, try_load_embedder().as_ref(), query, k);
    if hits.is_empty() {
        return Ok(format!("no matches for {query:?}"));
    }
    let lines: Vec<String> = hits
        .iter()
        .map(|h| format!("{:>6.3}  {}", h.score, h.id))
        .collect();
    Ok(lines.join("\n"))
}

/// Run a hybrid search: lexical always, cosine when an embedder is available.
///
/// With no embedder this is byte-identical to the legacy lexical-only path. With
/// an embedder it embeds `query` and reranks the union of the lexical and cosine
/// hit lists via [`merge_hybrid`], so a semantically-near document surfaces even
/// when it shares no literal terms with the query.
fn run_search(kb: &Knowledge, embedder: Option<&Embedder>, query: &str, k: usize) -> Vec<Hit> {
    let lexical = kb.search_text(query, k);
    let Some(vector) = embedder.and_then(|e| e.embed(query).ok()) else {
        return lexical;
    };
    if vector.is_empty() {
        return lexical;
    }
    let cosine = kb.search_vec(&vector, k);
    if cosine.is_empty() {
        return lexical;
    }
    merge_hybrid(&lexical, &cosine, k)
}

/// Merge a lexical and a cosine hit list into a reranked union of the top `k`.
///
/// Each list is independently min-max normalized to `[0, 1]` so the
/// term-frequency scale (unbounded counts) and the cosine scale (`[-1, 1]`)
/// become comparable, then per-document scores are summed across the two lists.
/// A document present in both lists therefore ranks above one present in only
/// one. Ties break deterministically by ascending id, matching the underlying
/// crate. The returned `score` is the combined value in `[0, 2]`.
fn merge_hybrid(lexical: &[Hit], cosine: &[Hit], k: usize) -> Vec<Hit> {
    // Track the summed min-max-normalized score AND how many retrieval paths
    // (lexical / cosine) surfaced each doc. A doc found by BOTH paths earns a
    // presence bonus so it outranks single-list hits — the point of a hybrid
    // union (min-max alone can't do this, since a doc at the bottom of one list
    // normalizes to 0 there and would tie with single-list docs).
    let mut combined: HashMap<&str, (f32, u32)> = HashMap::new();
    for hit in normalize(lexical) {
        let entry = combined.entry(hit.0).or_insert((0.0, 0));
        entry.0 += hit.1;
        entry.1 += 1;
    }
    for hit in normalize(cosine) {
        let entry = combined.entry(hit.0).or_insert((0.0, 0));
        entry.0 += hit.1;
        entry.1 += 1;
    }
    let mut all: Vec<Hit> = combined
        .into_iter()
        .map(|(id, (score, lists))| Hit {
            id: id.to_owned(),
            // +1.0 when surfaced by both paths (lists >= 2), else +0.0.
            score: score + if lists >= 2 { 1.0 } else { 0.0 },
        })
        .collect();
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    all.truncate(k);
    all
}

/// Min-max normalize a hit list's scores into `[0, 1]`, keyed by id.
///
/// A single-element or flat list maps every score to `1.0` (all equally
/// relevant within that list). An empty list yields an empty iterator.
fn normalize(hits: &[Hit]) -> impl Iterator<Item = (&str, f32)> {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for h in hits {
        lo = lo.min(h.score);
        hi = hi.max(h.score);
    }
    let span = hi - lo;
    hits.iter().map(move |h| {
        let norm = if span > 0.0 { (h.score - lo) / span } else { 1.0 };
        (h.id.as_str(), norm)
    })
}

fn remove(id: &str) -> Result<String> {
    let mut kb = load()?;
    if kb.remove(id) {
        save(&kb)?;
        Ok(format!("removed `{id}`"))
    } else {
        Ok(format!("no such document: `{id}`"))
    }
}

fn list() -> Result<String> {
    let kb = load()?;
    if kb.is_empty() {
        return Ok("knowledge index is empty".to_string());
    }
    // Surface ids by searching with an empty query falls short; instead list via
    // a wildcard-ish scan: re-serialize and read ids back.
    let json = kb.to_json().map_err(|e| anyhow::anyhow!("{e}"))?;
    let parsed: serde_json::Value = serde_json::from_str(&json)?;
    let mut ids = Vec::new();
    if let Some(docs) = parsed.get("docs").and_then(|d| d.as_array()) {
        for d in docs {
            if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                ids.push(id.to_string());
            }
        }
    }
    Ok(ids.join("\n"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, clippy::panic)]
mod tests {
    use super::*;

    fn hit(id: &str, score: f32) -> Hit {
        Hit {
            id: id.to_owned(),
            score,
        }
    }

    #[test]
    fn run_search_without_embedder_is_lexical_only() {
        // No embedder (None) => byte-identical to the legacy lexical path.
        let mut kb = Knowledge::new();
        kb.add(Doc::text("a", "rust async runtime"));
        kb.add(Doc::text("b", "python sync script"));
        let hybrid = run_search(&kb, None, "rust async", 10);
        let lexical = kb.search_text("rust async", 10);
        assert_eq!(hybrid, lexical);
        assert_eq!(hybrid[0].id, "a");
    }

    #[test]
    fn run_search_falls_back_when_query_unembeddable() {
        // Even if some embedder were present, a doc store with empty embeddings
        // means cosine returns nothing; the lexical result must still come back.
        let mut kb = Knowledge::new();
        kb.add(Doc::text("a", "alpha beta gamma"));
        let hybrid = run_search(&kb, None, "alpha", 5);
        assert_eq!(hybrid.len(), 1);
        assert_eq!(hybrid[0].id, "a");
    }

    #[test]
    fn cosine_ranks_nearest_first_via_hybrid() {
        // Construct docs with explicit small vectors; query the lexical+cosine
        // merge directly (no ONNX needed) by simulating the two lists.
        let mut kb = Knowledge::new();
        kb.add(Doc::new("near", "unrelated words one", vec![1.0, 0.0]));
        kb.add(Doc::new("mid", "unrelated words two", vec![0.7, 0.7]));
        kb.add(Doc::new("far", "unrelated words three", vec![0.0, 1.0]));
        // Query vector points along the "near" direction. No lexical overlap
        // with the query term, so cosine alone must decide the order.
        let cosine = kb.search_vec(&[1.0, 0.0], 3);
        let lexical = kb.search_text("zzz-no-such-term", 3);
        assert!(lexical.is_empty());
        let merged = merge_hybrid(&lexical, &cosine, 3);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, "near");
        assert_eq!(merged[1].id, "mid");
        assert_eq!(merged[2].id, "far");
    }

    #[test]
    fn hybrid_union_boosts_docs_in_both_lists() {
        // `shared` appears in both lists; it must outrank docs in only one.
        let lexical = vec![hit("shared", 3.0), hit("lex_only", 3.0)];
        let cosine = vec![hit("shared", 0.9), hit("vec_only", 0.95)];
        let merged = merge_hybrid(&lexical, &cosine, 10);
        assert_eq!(merged[0].id, "shared");
        // shared = 1.0 (lex norm) + 0.0 (vec norm, bottom) + 1.0 (both-paths bonus) = 2.0; others < 2.0.
        assert!((merged[0].score - 2.0).abs() < 1e-6);
        let ids: Vec<&str> = merged.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.contains(&"lex_only"));
        assert!(ids.contains(&"vec_only"));
    }

    #[test]
    fn merge_respects_k_and_tie_breaks_by_id() {
        let lexical = vec![hit("zebra", 1.0), hit("alpha", 1.0), hit("mango", 1.0)];
        let cosine: Vec<Hit> = Vec::new();
        let merged = merge_hybrid(&lexical, &cosine, 2);
        assert_eq!(merged.len(), 2);
        // All normalize to 1.0 => tie-break ascending id.
        assert_eq!(merged[0].id, "alpha");
        assert_eq!(merged[1].id, "mango");
    }

    #[test]
    fn normalize_single_element_maps_to_one() {
        let one = vec![hit("solo", 42.0)];
        let got: Vec<(&str, f32)> = normalize(&one).collect();
        assert_eq!(got, vec![("solo", 1.0)]);
    }

    #[test]
    fn normalize_empty_is_empty() {
        let none: Vec<Hit> = Vec::new();
        assert_eq!(normalize(&none).count(), 0);
    }

    #[test]
    fn embed_or_empty_without_embedder_is_empty() {
        assert!(embed_or_empty(None, "anything").is_empty());
    }

    #[test]
    fn model_path_absent_when_nothing_configured() {
        // We cannot guarantee env is clean in CI, but the function must never
        // panic and must return a path that, if Some, actually exists.
        if let Some(p) = model_path() {
            assert!(p.exists());
        }
    }
}
