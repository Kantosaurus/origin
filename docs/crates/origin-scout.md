# origin-scout

> Read-only dependency-source research: shallow-clone planning and repo overview extraction for origin.

## Purpose

`origin-scout` helps the agent research a dependency's source by planning a
shallow clone into a deterministic, url-derived cache location and extracting a
compact overview (README excerpt, manifest summary, top-level directories,
likely entry points) from a file listing. Git access is injected through the
`CloneRunner` trait and the overview extraction is pure, so the crate is fully
unit-testable offline and performs no network or filesystem access on its own
planning/summary paths. It is `#![forbid(unsafe_code)]`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `CloneRunner` (trait) | trait | `shallow_clone(url, dest)` — the only side-effecting hook. |
| `cache_path(cache_root, repo_url)` | fn → `String` | Deterministic `…/scout-<hash>` directory for a url. |
| `clone_plan(cache_root, repo_url)` | fn → `(String, bool)` | Destination + cached hint (always `false`; no FS access). |
| `Overview` | struct | `{ readme_excerpt, manifest_summary, top_dirs, entry_points }`. |
| `build_overview(file_list, readme, manifest)` | fn → `Overview` | Pure repository summary. |
| `ScoutError` | enum | `Git(String)` / `Io(String)`. |

## Key types

```rust
pub trait CloneRunner {
    fn shallow_clone(&self, url: &str, dest: &str) -> Result<(), ScoutError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub readme_excerpt: String,
    pub manifest_summary: String,
    pub top_dirs: Vec<String>,
    pub entry_points: Vec<String>,
}
```

## How it works

`cache_path` hashes the repo url with a hand-rolled FNV-1a (keeping the crate
dependency-light) and trims trailing separators from the cache root, so the same
url always maps to the same `…/scout-<16-hex>` directory and distinct urls
differ. `clone_plan` returns that destination plus a `cached = false` hint — it
does no filesystem access, leaving the existence check (and the actual
`git clone --depth 1` via `CloneRunner`) to the caller.

```
repo_url ─► fnv1a_64 ─► cache_path → "<root>/scout-<hash>"
file_list ─► top_dir (first nested component, sorted+deduped) ─┐
          ─► is_entry_point (src/main.rs, index.ts, main.py, …)├─► Overview
readme    ─► excerpt (UTF-8-boundary truncate to 800 bytes, … )│
manifest  ─► summarize_manifest (Cargo.toml / package.json / pyproject.toml)┘
```

`build_overview` infers top-level directories (first path component of each
nested entry, normalized to forward slashes, sorted and de-duplicated), detects
likely entry points against a fixed conventional list, truncates the README to
800 bytes on a UTF-8 boundary (appending `…`), and identifies the package
manifest kind with its byte size. Empty inputs yield an empty excerpt and
`"no recognized manifest"`.

## Dependencies & features

- `serde` (with `derive`) — `Overview` serialization.
- `thiserror` — `ScoutError`.
- Dev: `serde_json` (round-trip tests).
- `#![forbid(unsafe_code)]`; no cargo features; no git/network crates (the clone
  is the caller's injected `CloneRunner`).

## Used by

`Grep "origin-scout" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-scout/Cargo.toml` (self)

## Testing

Inline tests cover: `cache_path` stability + url-derivation + trailing-separator
trimming; `clone_plan` reporting the destination without a cached hint;
`build_overview` detecting entry points and top dirs; the manifest summary
distinguishing Cargo / package.json / pyproject (and the no-manifest case);
README excerpt truncation (ending in `…`) and leading-whitespace trimming; empty
inputs; `Overview` serde round-trip; and a mock `CloneRunner` returning Ok / a
`ScoutError::Git` for an empty url.

## See also

- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
