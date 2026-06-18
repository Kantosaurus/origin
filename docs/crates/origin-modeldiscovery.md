# origin-modeldiscovery

> Runtime model discovery: parse provider model listings, merge with builtin catalog, and cache with a TTL

## Purpose

`origin-modeldiscovery` adds *runtime* model discovery on top of origin's
hand-maintained builtin model list. Given a provider's raw model-listing JSON, it
parses the available models, merges them into the builtin catalog, and caches the
result. The crate is pure parse + merge + cache policy: it performs no network
I/O (the HTTP GET is the caller's job) and owns nothing time-dependent, so it is
fully offline-testable.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ModelInfo` | struct | One discovered model; carries its `id`. |
| `ModelInfo::new` | fn | Construct from an id. |
| `parse_models_response` | fn | Parse a listing JSON into `Vec<ModelInfo>` across three shapes. |
| `merge_catalog` | fn | De-duplicated union of builtin ids + discovered ids (builtin first). |
| `ModelCache` | struct | In-memory provider → listing cache with JSON persistence. |
| `ModelCache::{new, put, get, to_json, from_json}` | fn | Cache lifecycle + serialization. |
| `DiscoveryError` | enum | `Parse(String)` for bad JSON / unknown shapes. |

## Key types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo { pub id: String }

pub fn parse_models_response(json: &str) -> Result<Vec<ModelInfo>, DiscoveryError>;
pub fn merge_catalog(builtin: &[String], discovered: &[ModelInfo]) -> Vec<String>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCache { /* providers: BTreeMap<String, CacheEntry> */ }

impl ModelCache {
    pub fn put(&mut self, provider: &str, models: Vec<ModelInfo>);
    pub fn get(&self, provider: &str) -> Option<&[ModelInfo]>;
    pub fn to_json(&self) -> Result<String, DiscoveryError>;
    pub fn from_json(s: &str) -> Result<Self, DiscoveryError>;
}
```

## How it works

**Parsing.** `parse_models_response` accepts three top-level shapes via a
`#[serde(untagged)]` envelope (object-wrapped shapes precede the bare array):

- OpenAI shape: `{"data": [{"id": …}, …]}`
- `{"models": [{"id": …}, …]}`
- a bare array `[{"id": …}, …]`

Only `id` is required; extra per-model fields are ignored, and entries lacking a
non-empty `id` are skipped (so one malformed row does not discard the listing).
Invalid JSON or an unrecognised shape yields `DiscoveryError::Parse`.

**Merging.** `merge_catalog` returns the de-duplicated union of `builtin` ids
(keeping their original order, first) followed by discovered ids not already
present, in listing order. Duplicates within or across inputs collapse to their
first occurrence — the result is stable and deterministic.

**Caching.** `ModelCache` is plain in-memory state (a `BTreeMap`) with no
background expiry: callers `put` a freshly fetched listing keyed by provider name
and `get` it back. `to_json`/`from_json` round-trip the whole cache for on-disk
persistence; the TTL refresh policy (when to refetch) is enforced by the caller
around this store.

## Dependencies & features

Only `serde` (derive), `serde_json`, and `thiserror`. No async, no network, no
other origin crates. No cargo features.

## Used by

`Grep "origin-modeldiscovery"` over `crates/*/Cargo.toml`:

```
crates/origin-cli/Cargo.toml
crates/origin-modeldiscovery/Cargo.toml
```

## Testing

All coverage is in-file (`#[cfg(test)] mod tests`): parsing each of the three
listing shapes, ignoring extra fields, skipping entries without a usable id,
rejecting junk/unknown-shape JSON, the builtin-first de-dup contract of
`merge_catalog`, and `ModelCache` put/get/replace plus `to_json`/`from_json`
round-trips.

## See also

- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
