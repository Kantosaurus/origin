# origin-websearch

> Pluggable web search with offline-testable result parsers and an injected fetcher.

## Purpose

`origin-websearch` turns raw search-engine responses into a clean, ranked list
of hits and an optional cited summary. It supplies HTML/JSON parsers for three
engines, an endpoint/header builder, a term-overlap reranker, and a deterministic
"grounded answer" synthesizer. The live network fetch is *injected* by the
caller, so the whole crate is unit-testable offline with no I/O and no language
model.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Engine` | enum | `DuckDuckGo` / `Brave` / `Tavily`. |
| `SearchHit` | struct | `{ title, url, snippet }`. |
| `SearchError` | enum | `Parse(String)`. |
| `parse_duckduckgo_html` | fn | Scrapes the DDG HTML endpoint without an HTML lib. |
| `parse_brave_json` / `parse_tavily_json` | fn | Parse the respective JSON APIs. |
| `endpoint_for` | fn | Builds `(url, headers)` for an engine + query (+ optional API key). |
| `rank` | fn | Reranks by query-term overlap and dedups by URL (stable). |
| `ground` | fn | Synthesizes a `GroundedAnswer` from top hits with inline `[n]` markers. |
| `GroundedAnswer` / `Citation` | struct | Cited summary + provenance; `render()` adds a `Sources:` footer. |

## Key types

```rust
pub enum Engine { DuckDuckGo, Brave, Tavily }

pub struct SearchHit { pub title: String, pub url: String, pub snippet: String }

pub struct GroundedAnswer {
    pub answer: String,        // contains inline [n] markers
    pub sources: Vec<Citation>,
}
pub struct Citation { pub number: usize, pub title: String, pub url: String }
```

## How it works

`endpoint_for` returns the fully-formed URL plus header pairs: DuckDuckGo needs
no key (it gets a `User-Agent`), Brave attaches `X-Subscription-Token`, Tavily
attaches an `Authorization: Bearer`. The caller's fetcher performs the GET/POST;
the matching parser converts the body to `Vec<SearchHit>` (the DDG parser even
unwraps `uddg=` redirect links and decodes HTML entities by hand). `rank` scores
each hit by how many distinct lowercase query terms appear in its title/snippet,
sorts descending (stable), and dedups by URL. `ground` then folds the top
`GROUND_TOP_N` (5) hits into one sentence each with contiguous `[n]` markers,
purely by string assembly — no network, no LLM, fully deterministic.

```
endpoint_for(engine,q,key) ─▶ (url, headers) ─▶ [injected fetcher] ─▶ body
body ─▶ parse_*  ─▶ Vec<SearchHit> ─▶ rank(query) ─▶ ground(query) ─▶ GroundedAnswer
```

## Design notes

The fetcher seam is the whole point: every function here is `body in → values
out`, so the crate never opens a socket. `endpoint_for` even returns the headers
the caller must attach (key placement differs per engine), and parsing is split
from fetching so a recorded fixture exercises the exact code path a live request
would. The DDG path is deliberately library-free — it splits on the
`result__a` class marker and decodes `uddg=` redirects and a small HTML-entity
set by hand — which keeps the dependency surface minimal and the scrape robust
to partial markup (malformed input yields an empty `Vec`, never an error).

`ground` is the other novelty: it attaches provenance to bare search output so a
model receives a cited summary instead of a raw link list, and it does so by
deterministic string assembly only. Empty titles fall back to the URL host, and
empty snippets fall back to the title, so every selected hit yields a non-empty,
period-terminated sentence with a matching `[n]` source.

## Dependencies & features

Tiny and `#![forbid(unsafe_code)]`. Only `serde`/`serde_json` (for `SearchHit`,
`Citation`, `GroundedAnswer` serialization) and `thiserror`. No HTTP client, no
async runtime — the fetcher is the caller's responsibility.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-websearch/Cargo.toml
```

## Testing

The crate is exhaustively unit-tested in-file: a DDG HTML fixture (redirect
unwrap + entity decode), Brave/Tavily JSON fixtures, missing-results and
invalid-JSON cases, endpoint/header assertions per engine, `rank` ordering and
dedup (including the empty-query path), and `ground` properties — contiguous
numbering from 1, every marker backed by a source, top-N capping, title/host
fallback, and determinism.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
