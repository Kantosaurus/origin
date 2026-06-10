// SPDX-License-Identifier: Apache-2.0
//! Local knowledge index over documents with text and optional vector search.
//!
//! `origin`'s baseline can run tools and skills but keeps no searchable memory of
//! prior notes, files, or embeddings. This crate closes that gap (openclaude's
//! Orama in-process index, kilo/oc `semantic_search`) with a single, dependency-
//! light store that does both lexical and semantic retrieval:
//!
//! * a tf-style **inverted index** over tokenized lowercase terms for full-text
//!   search, and
//! * **cosine similarity** over caller-supplied embeddings for semantic search.
//!
//! The whole store is `serde`-serializable, so the daemon can persist it to a
//! JSON file and reload it on the next session. There is no I/O, async, or
//! platform concern inside the crate — embeddings are produced elsewhere and
//! handed in, keeping this layer pure and trivially testable.
//!
//! ```
//! use origin_knowledge::{Doc, Knowledge};
//!
//! let mut k = Knowledge::new();
//! k.add(Doc::text("a", "the quick brown fox"));
//! k.add(Doc::text("b", "a lazy brown dog sleeps"));
//! let hits = k.search_text("brown fox", 2);
//! assert_eq!(hits[0].id, "a"); // more query-term hits ranks first
//! ```

#![forbid(unsafe_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A stored document: a stable id, its raw text, and an optional embedding.
///
/// The `embedding` is supplied by the caller (any external model) and may be
/// empty for text-only documents; such documents are simply skipped by
/// [`Knowledge::search_vec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    /// Caller-chosen unique identifier. Re-adding an existing id replaces it.
    pub id: String,
    /// Raw document text, tokenized lazily for the inverted index.
    pub text: String,
    /// Caller-supplied embedding; empty means "text-only, no vector".
    pub embedding: Vec<f32>,
}

impl Doc {
    /// Construct a document with an explicit embedding.
    #[must_use]
    pub fn new(id: impl Into<String>, text: impl Into<String>, embedding: Vec<f32>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            embedding,
        }
    }

    /// Construct a text-only document (empty embedding).
    #[must_use]
    pub fn text(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            embedding: Vec::new(),
        }
    }
}

/// A search result: the matching document id and its relevance score.
///
/// Higher scores rank first; for text search the score is a term-frequency sum,
/// for vector search it is cosine similarity in `[-1, 1]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// Id of the matching document.
    pub id: String,
    /// Relevance score (higher is better).
    pub score: f32,
}

/// Errors produced when (de)serializing a [`Knowledge`] store.
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    /// JSON serialization or deserialization failed.
    #[error("knowledge serde error: {0}")]
    Serde(String),
}

/// In-memory knowledge store with full-text and vector search.
///
/// Persist with [`Knowledge::to_json`] and restore with
/// [`Knowledge::from_json`]; the inverted index is rebuilt on load so only the
/// documents are stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(from = "KnowledgeData", into = "KnowledgeData")]
pub struct Knowledge {
    /// Documents in insertion order (stable for deterministic iteration).
    docs: Vec<Doc>,
    /// Token -> list of `docs` indices containing that token (with multiplicity
    /// captured as repeated entries so term frequency falls out of a count).
    index: HashMap<String, Vec<usize>>,
}

/// On-disk shape: only the documents are persisted; the index is rebuilt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KnowledgeData {
    docs: Vec<Doc>,
}

impl From<KnowledgeData> for Knowledge {
    fn from(data: KnowledgeData) -> Self {
        let mut k = Self {
            docs: Vec::with_capacity(data.docs.len()),
            index: HashMap::new(),
        };
        for doc in data.docs {
            k.add(doc);
        }
        k
    }
}

impl From<Knowledge> for KnowledgeData {
    fn from(k: Knowledge) -> Self {
        Self { docs: k.docs }
    }
}

impl Knowledge {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `doc`, replacing any existing document with the same id.
    ///
    /// The inverted index is updated in place so searches see the new content.
    pub fn add(&mut self, doc: Doc) {
        // Replace-by-id: drop the old entry (and its index postings) first.
        self.remove(&doc.id);
        let idx = self.docs.len();
        for token in tokenize(&doc.text) {
            self.index.entry(token).or_default().push(idx);
        }
        self.docs.push(doc);
    }

    /// Remove the document with `id`, returning whether one was present.
    ///
    /// Removal shifts later documents down by one, so the inverted index is
    /// rebuilt to keep its postings consistent.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(pos) = self.docs.iter().position(|d| d.id == id) else {
            return false;
        };
        self.docs.remove(pos);
        self.rebuild_index();
        true
    }

    /// Number of stored documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether the store holds no documents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Full-text search: rank documents by **TF-IDF** of the query's tokens,
    /// returning the top `k` hits.
    ///
    /// Each query term contributes `tf × idf`, where `tf` is the term's
    /// occurrence count in the document and `idf = ln(1 + N/df)` down-weights
    /// terms that appear in many documents. Plain summed term frequency (the
    /// previous scoring) let a common word dominate a rare, discriminative one;
    /// IDF fixes that so a match on an unusual query term ranks higher.
    /// Documents matching no query term are omitted. An empty query (or
    /// `k == 0`) yields an empty result. Ties are broken by ascending id.
    #[must_use]
    pub fn search_text(&self, query: &str, k: usize) -> Vec<Hit> {
        if k == 0 {
            return Vec::new();
        }
        let terms = tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let n_docs = as_f32(u32::try_from(self.docs.len().max(1)).unwrap_or(u32::MAX));
        let mut scores: HashMap<usize, f32> = HashMap::new();
        for term in terms {
            if let Some(postings) = self.index.get(&term) {
                // Term frequency per document for this term.
                let mut tf: HashMap<usize, u32> = HashMap::new();
                for &doc_idx in postings {
                    *tf.entry(doc_idx).or_insert(0) += 1;
                }
                // df = number of distinct documents containing the term; rarer
                // terms (low df) are more discriminative, so they weigh more.
                let df = as_f32(u32::try_from(tf.len().max(1)).unwrap_or(u32::MAX));
                let idf = (n_docs / df).ln_1p();
                for (doc_idx, count) in tf {
                    let slot = scores.entry(doc_idx).or_insert(0.0);
                    *slot = as_f32(count).mul_add(idf, *slot);
                }
            }
        }
        let hits = scores.into_iter().map(|(doc_idx, score)| Hit {
            id: self.docs[doc_idx].id.clone(),
            score,
        });
        top_k(hits, k)
    }

    /// Vector search: rank documents by cosine similarity to `query`, returning
    /// the top `k` hits.
    ///
    /// Documents with an empty embedding or a dimension that does not match
    /// `query`'s length are skipped. Zero-magnitude vectors score 0. An empty
    /// query (or `k == 0`) yields an empty result. Ties are broken
    /// deterministically by ascending id.
    #[must_use]
    pub fn search_vec(&self, query: &[f32], k: usize) -> Vec<Hit> {
        if k == 0 || query.is_empty() {
            return Vec::new();
        }
        let q_norm = norm(query);
        let hits = self.docs.iter().filter_map(|doc| {
            if doc.embedding.len() != query.len() {
                return None;
            }
            Some(Hit {
                id: doc.id.clone(),
                score: cosine(query, &doc.embedding, q_norm),
            })
        });
        top_k(hits, k)
    }

    /// Serialize the store (documents only) to a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`KnowledgeError::Serde`] if serialization fails.
    pub fn to_json(&self) -> Result<String, KnowledgeError> {
        let data = KnowledgeData {
            docs: self.docs.clone(),
        };
        serde_json::to_string(&data).map_err(|e| KnowledgeError::Serde(e.to_string()))
    }

    /// Deserialize a store from a JSON string produced by [`Knowledge::to_json`],
    /// rebuilding the inverted index.
    ///
    /// # Errors
    ///
    /// Returns [`KnowledgeError::Serde`] if the input is not valid JSON for the
    /// store shape.
    pub fn from_json(s: &str) -> Result<Self, KnowledgeError> {
        let data: KnowledgeData =
            serde_json::from_str(s).map_err(|e| KnowledgeError::Serde(e.to_string()))?;
        Ok(Self::from(data))
    }

    /// Rebuild the inverted index from scratch over the current documents.
    fn rebuild_index(&mut self) {
        self.index.clear();
        for (idx, doc) in self.docs.iter().enumerate() {
            for token in tokenize(&doc.text) {
                self.index.entry(token).or_default().push(idx);
            }
        }
    }
}

/// Take the top `k` hits from `hits`, ordered by descending score then ascending
/// id (a deterministic, stable tie-break).
fn top_k(hits: impl Iterator<Item = Hit>, k: usize) -> Vec<Hit> {
    let mut all: Vec<Hit> = hits.collect();
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    all.truncate(k);
    all
}

/// Tokenize `text`: split on non-alphanumeric runs, lowercase, and drop tokens
/// shorter than two characters.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// Euclidean norm (magnitude) of a vector.
fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Cosine similarity of `a` and `b`, given `a`'s precomputed norm `a_norm`.
///
/// Lengths are assumed equal (callers guarantee this). Returns 0 when either
/// vector has zero magnitude, avoiding a divide-by-zero.
fn cosine(a: &[f32], b: &[f32], a_norm: f32) -> f32 {
    let b_norm = norm(b);
    if a_norm == 0.0 || b_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    dot / (a_norm * b_norm)
}

#[inline]
#[allow(clippy::cast_precision_loss)] // term-frequency counts are tiny
const fn as_f32(v: u32) -> f32 {
    v as f32
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn text_search_ranks_more_query_term_hits_higher() {
        let mut k = Knowledge::new();
        k.add(Doc::text("a", "rust rust async rust runtime"));
        k.add(Doc::text("b", "rust async programming"));
        k.add(Doc::text("c", "completely unrelated text"));
        let hits = k.search_text("rust async", 10);
        // a: rust*3 + async*1 = 4; b: rust*1 + async*1 = 2; c: 0 (omitted).
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "a");
        assert_eq!(hits[1].id, "b");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn text_search_weights_rare_terms_higher_via_idf() {
        let mut k = Knowledge::new();
        // "common" appears in a and b (low idf); "quantum" only in c (high idf).
        k.add(Doc::text("a", "common alpha"));
        k.add(Doc::text("b", "common gamma"));
        k.add(Doc::text("c", "quantum delta"));
        let hits = k.search_text("common quantum", 5);
        // Each matching doc has exactly one query-term occurrence, so plain TF
        // would tie them; TF-IDF lifts c (the rare "quantum" match) above the
        // two docs that only matched the common term.
        assert_eq!(hits[0].id, "c", "rare-term match must outrank common-term matches");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn text_search_respects_k() {
        let mut k = Knowledge::new();
        k.add(Doc::text("a", "alpha beta"));
        k.add(Doc::text("b", "alpha gamma"));
        k.add(Doc::text("c", "alpha delta"));
        let hits = k.search_text("alpha", 2);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn empty_query_yields_empty() {
        let mut k = Knowledge::new();
        k.add(Doc::text("a", "anything at all"));
        assert!(k.search_text("", 5).is_empty());
        assert!(k.search_text("   !! ?", 5).is_empty());
        assert!(k.search_text("anything", 0).is_empty());
        assert!(k.search_vec(&[], 5).is_empty());
        assert!(k.search_vec(&[1.0], 0).is_empty());
    }

    #[test]
    fn vector_search_ranks_nearest() {
        let mut k = Knowledge::new();
        k.add(Doc::new("a", "doc a", vec![1.0, 0.0]));
        k.add(Doc::new("b", "doc b", vec![0.0, 1.0]));
        k.add(Doc::new("c", "doc c", vec![0.9, 0.1]));
        let hits = k.search_vec(&[1.0, 0.0], 3);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].id, "a"); // identical direction -> cosine 1
        assert_eq!(hits[1].id, "c"); // close direction
        assert_eq!(hits[2].id, "b"); // orthogonal -> cosine 0
        assert!((hits[0].score - 1.0).abs() < 1e-6);
        assert!(hits[2].score.abs() < 1e-6);
    }

    #[test]
    fn vector_search_skips_empty_and_dim_mismatch() {
        let mut k = Knowledge::new();
        k.add(Doc::new("a", "good", vec![1.0, 0.0, 0.0]));
        k.add(Doc::text("b", "text only, empty embedding"));
        k.add(Doc::new("c", "wrong dim", vec![1.0, 0.0])); // 2 != 3
        let hits = k.search_vec(&[1.0, 0.0, 0.0], 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }

    #[test]
    fn zero_vector_scores_zero() {
        let mut k = Knowledge::new();
        k.add(Doc::new("a", "zero", vec![0.0, 0.0]));
        let hits = k.search_vec(&[1.0, 1.0], 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].score, 0.0);
        // Zero query vector also scores 0 against a real doc.
        let mut k2 = Knowledge::new();
        k2.add(Doc::new("b", "real", vec![1.0, 1.0]));
        let hits2 = k2.search_vec(&[0.0, 0.0], 1);
        assert_eq!(hits2[0].score, 0.0);
    }

    #[test]
    fn remove_works_and_updates_search() {
        let mut k = Knowledge::new();
        k.add(Doc::text("a", "keep this around"));
        k.add(Doc::text("b", "remove this later"));
        assert_eq!(k.len(), 2);
        assert!(k.remove("b"));
        assert!(!k.remove("b")); // already gone
        assert_eq!(k.len(), 1);
        // "remove" term should no longer match anything.
        assert!(k.search_text("remove", 5).is_empty());
        // The surviving doc is still searchable.
        let hits = k.search_text("keep", 5);
        assert_eq!(hits[0].id, "a");
    }

    #[test]
    fn add_replaces_by_id() {
        let mut k = Knowledge::new();
        k.add(Doc::text("a", "original content"));
        k.add(Doc::text("a", "fresh replacement"));
        assert_eq!(k.len(), 1);
        assert!(k.search_text("original", 5).is_empty());
        assert_eq!(k.search_text("replacement", 5)[0].id, "a");
    }

    #[test]
    fn json_round_trip_preserves_search() {
        let mut k = Knowledge::new();
        k.add(Doc::new("a", "the quick brown fox", vec![1.0, 0.0]));
        k.add(Doc::new("b", "lazy brown dog", vec![0.0, 1.0]));
        let json = k.to_json().unwrap();
        let restored = Knowledge::from_json(&json).unwrap();
        assert_eq!(restored.len(), 2);
        // Inverted index was rebuilt: text search still works.
        let text_hits = restored.search_text("brown fox", 5);
        assert_eq!(text_hits[0].id, "a");
        // Vector search still works.
        let vec_hits = restored.search_vec(&[1.0, 0.0], 1);
        assert_eq!(vec_hits[0].id, "a");
    }

    #[test]
    fn from_json_rejects_garbage() {
        let err = Knowledge::from_json("not valid json").unwrap_err();
        match err {
            KnowledgeError::Serde(msg) => assert!(!msg.is_empty()),
        }
    }

    #[test]
    fn tie_break_is_deterministic_by_id() {
        let mut k = Knowledge::new();
        // Three docs each match the query exactly once -> equal scores.
        k.add(Doc::text("zebra", "match"));
        k.add(Doc::text("alpha", "match"));
        k.add(Doc::text("mango", "match"));
        let hits = k.search_text("match", 3);
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn tokenizer_drops_short_tokens_and_lowercases() {
        let toks = tokenize("Rust is A Great Language, OK?");
        // "is", "A", "OK", and punctuation handling: "a" is 1 char -> dropped.
        assert!(toks.contains(&"rust".to_string()));
        assert!(toks.contains(&"is".to_string()));
        assert!(toks.contains(&"great".to_string()));
        assert!(toks.contains(&"ok".to_string()));
        assert!(!toks.iter().any(|t| t == "a")); // single char dropped
    }

    #[test]
    fn len_and_is_empty() {
        let mut k = Knowledge::new();
        assert!(k.is_empty());
        assert_eq!(k.len(), 0);
        k.add(Doc::text("a", "hello world"));
        assert!(!k.is_empty());
        assert_eq!(k.len(), 1);
    }
}
