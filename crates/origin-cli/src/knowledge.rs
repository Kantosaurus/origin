// SPDX-License-Identifier: Apache-2.0
//! `origin knowledge` — a local full-text knowledge index.
//!
//! Persisted to `~/.origin/knowledge.json`, backed by [`origin_knowledge`]
//! (openclaude `/knowledge`; kilocode/opencode `semantic_search` parity for the
//! text path). Vector search needs an embedder (the daemon owns those); the CLI
//! exposes the lexical path, which works fully offline.

use std::path::PathBuf;

use anyhow::{Context, Result};
use origin_knowledge::{Doc, Knowledge};

use crate::cli_def::KnowledgeSub;

/// Dispatch a `knowledge` subcommand.
///
/// # Errors
/// Returns on filesystem or JSON (de)serialization failure.
pub fn run(sub: KnowledgeSub) -> Result<()> {
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

fn add(id: &str, text: &str) -> Result<()> {
    let mut kb = load()?;
    kb.add(Doc::text(id, text));
    save(&kb)?;
    println!("indexed `{id}` ({} docs total)", kb.len());
    Ok(())
}

fn search(query: &str, k: usize) -> Result<()> {
    let kb = load()?;
    let hits = kb.search_text(query, k);
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(());
    }
    for h in hits {
        println!("{:>6.3}  {}", h.score, h.id);
    }
    Ok(())
}

fn remove(id: &str) -> Result<()> {
    let mut kb = load()?;
    if kb.remove(id) {
        save(&kb)?;
        println!("removed `{id}`");
    } else {
        println!("no such document: `{id}`");
    }
    Ok(())
}

fn list() -> Result<()> {
    let kb = load()?;
    if kb.is_empty() {
        println!("knowledge index is empty");
        return Ok(());
    }
    // Surface ids by searching with an empty query falls short; instead list via
    // a wildcard-ish scan: re-serialize and read ids back.
    let json = kb.to_json().map_err(|e| anyhow::anyhow!("{e}"))?;
    let parsed: serde_json::Value = serde_json::from_str(&json)?;
    if let Some(docs) = parsed.get("docs").and_then(|d| d.as_array()) {
        for d in docs {
            if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                println!("{id}");
            }
        }
    }
    Ok(())
}
