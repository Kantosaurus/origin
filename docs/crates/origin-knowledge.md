# origin-knowledge

> Local knowledge index: full-text inverted index + cosine vector search, JSON-persistable.

## Purpose

`origin-knowledge` is a dependency-light in-process store that does both lexical
and semantic retrieval over documents: a TF-IDF inverted index for full-text
search and cosine similarity over caller-supplied embeddings for semantic search.
The whole store is `serde`-serializable to JSON, so the daemon can persist it and
reload it next session. There is no I/O, async, or platform concern — embeddings
are produced elsewhere and handed in, keeping the layer pure and testable.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Doc` | struct | `{ id, text, embedding }` document; `new` / `text` ctors. |
| `Hit` | struct | `{ id, score }` search result (higher ranks first). |
| `Knowledge` | struct | The store: add/remove/search/persist. |
| `Knowledge::add` / `remove` | fn | Insert (replace-by-id) / delete a document. |
| `Knowledge::search_text` | fn | TF-IDF full-text search, top `k`. |
| `Knowledge::search_vec` | fn | Cosine vector search, top `k`. |
| `Knowledge::to_json` / `from_json` | fn | Persist / restore (index rebuilt on load). |
| `Knowledge::len` / `is_empty` | fn | Document count helpers. |
| `KnowledgeError` | enum | `Serde(String)`. |

## Key types

```rust
pub struct Doc {
    pub id: String,
    pub text: String,
    pub embedding: Vec<f32>, // empty = text-only, skipped by search_vec
}
impl Doc {
    pub fn new(id: impl Into<String>, text: impl Into<String>, embedding: Vec<f32>) -> Self;
    pub fn text(id: impl Into<String>, text: impl Into<String>) -> Self;
}

pub struct Hit { pub id: String, pub score: f32 }

impl Knowledge {
    pub fn add(&mut self, doc: Doc);
    pub fn remove(&mut self, id: &str) -> bool;
    pub fn search_text(&self, query: &str, k: usize) -> Vec<Hit>;
    pub fn search_vec(&self, query: &[f32], k: usize) -> Vec<Hit>;
    pub fn to_json(&self) -> Result<String, KnowledgeError>;
    pub fn from_json(s: &str) -> Result<Self, KnowledgeError>;
}
```

## How it works

`Knowledge` holds documents in insertion order plus an inverted index
`HashMap<token, Vec<doc_index>>`, where multiplicity is captured as repeated
postings so term frequency falls out of a count. `add` is replace-by-id (it drops
the old document and its postings first); `remove` shifts later docs down and
rebuilds the index to keep postings consistent.

```text
search_text(query, k)
   tokenize(query) → for each term: tf per doc × idf = ln(1 + N/df)
   sum per doc, drop non-matches, top-k (ties by ascending id)

search_vec(query, k)
   cosine(query, doc.embedding) for docs with a non-empty embedding
   top-k by similarity in [-1, 1]
```

IDF down-weights terms that appear in many documents, so a match on a rare,
discriminative query term outranks a common word. Persistence uses a
`#[serde(from/into = "KnowledgeData")]` shim: only the documents are written to
JSON; the inverted index is reconstructed by replaying `add` on load.

## Dependencies & features

- `serde` + `serde_json` (persistence) and `thiserror`. No async, no I/O,
  `#![forbid(unsafe_code)]`. No optional cargo features.

## Used by

`crates/*/Cargo.toml` matches for `origin-knowledge`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-knowledge/Cargo.toml`

## Testing

All tests are in-file in `lib.rs` (e.g. the tokenizer drops short tokens and
lowercases). They cover TF-IDF ranking, vector cosine search, replace-by-id and
remove semantics, and JSON round-trip with index rebuild.

## See also

- [Memory & code graph subsystem](../subsystems/memory-and-codegraph.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
