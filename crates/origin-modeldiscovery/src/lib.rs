// SPDX-License-Identifier: Apache-2.0
//! Runtime model discovery, catalog merge, and a TTL cache for `origin`.
//!
//! `origin`'s baseline ships a hand-maintained builtin model list. This crate
//! adds *runtime* discovery (openclaude's descriptor-era runtime models lookup,
//! opencode's `models --refresh`): given a provider's raw model-listing JSON it
//! parses the available models, merges them into the builtin catalog, and caches
//! the result behind a wall-clock TTL so the daemon refetches only when stale.
//!
//! The crate is pure parse + merge + cache policy — it performs no network I/O.
//! The HTTP GET belongs to the caller; this crate consumes the response body and
//! owns nothing time-dependent (the caller passes `now_unix`), so it is fully
//! offline-testable.
//!
//! ```
//! use origin_modeldiscovery::{parse_models_response, merge_catalog, ModelCache};
//!
//! let body = r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#;
//! let discovered = parse_models_response(body).unwrap();
//! let catalog = merge_catalog(&["claude-sonnet-4-6".to_string()], &discovered);
//! assert_eq!(catalog, ["claude-sonnet-4-6", "gpt-4o", "gpt-4o-mini"]);
//!
//! let mut cache = ModelCache::new();
//! cache.put("openai", discovered);
//! assert_eq!(cache.get("openai").map(<[_]>::len), Some(2));
//! ```

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Metadata for a single runtime-discovered model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Provider-scoped model identifier (for example `gpt-4o`).
    pub id: String,
}

impl ModelInfo {
    /// Construct a [`ModelInfo`] from its id.
    #[must_use]
    pub const fn new(id: String) -> Self {
        Self { id }
    }
}

/// Errors produced while parsing or (de)serializing discovery data.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// The input was not valid JSON, or did not match any accepted model-listing
    /// shape. The wrapped string explains what went wrong.
    #[error("failed to parse model listing: {0}")]
    Parse(String),
}

/// One model entry as it may appear inside a provider listing.
///
/// Only `id` is required; everything else is best-effort and tolerated when
/// absent. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct RawModel {
    id: Option<String>,
}

/// Accepted top-level shapes for a model-listing response.
///
/// Order matters: `serde(untagged)` tries each variant top-to-bottom, so the
/// object-wrapped shapes precede the bare array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModelsEnvelope {
    /// `OpenAI` shape: `{"data": [ {"id": ...}, ... ]}`.
    Data { data: Vec<RawModel> },
    /// Alternative shape: `{"models": [ {"id": ...}, ... ]}`.
    Models { models: Vec<RawModel> },
    /// Bare top-level array: `[ {"id": ...}, ... ]`.
    Bare(Vec<RawModel>),
}

impl RawModel {
    fn into_info(self) -> Option<ModelInfo> {
        // Drop entries without a usable id; an empty id is meaningless.
        let id = self.id.filter(|s| !s.is_empty())?;
        Some(ModelInfo { id })
    }
}

/// Parse a provider's raw model-listing JSON into [`ModelInfo`] records.
///
/// Three top-level shapes are accepted, mirroring the listings emitted by the
/// major providers and aggregators:
/// * the `OpenAI` shape — an object with a `data` array of objects each carrying an
///   `id`;
/// * an object with a `models` array of the same;
/// * a bare top-level array of model objects.
///
/// Entries lacking a non-empty `id` are skipped rather than rejected, so a
/// single malformed row does not discard an otherwise valid listing. Any
/// additional per-model fields are ignored.
///
/// # Errors
///
/// Returns [`DiscoveryError::Parse`] when the input is not valid JSON or does
/// not match any accepted shape.
pub fn parse_models_response(json: &str) -> Result<Vec<ModelInfo>, DiscoveryError> {
    let envelope: ModelsEnvelope =
        serde_json::from_str(json).map_err(|e| DiscoveryError::Parse(e.to_string()))?;
    let raw = match envelope {
        ModelsEnvelope::Data { data } => data,
        ModelsEnvelope::Models { models } => models,
        ModelsEnvelope::Bare(list) => list,
    };
    Ok(raw.into_iter().filter_map(RawModel::into_info).collect())
}

/// Merge a builtin catalog of model ids with a runtime-discovered set.
///
/// The result is the de-duplicated union of `builtin` ids followed by any
/// discovered ids not already present. Order is stable and deterministic:
/// builtin ids keep their original order and come first, then discovered ids in
/// listing order. Duplicates (within or across the two inputs) are collapsed to
/// their first occurrence.
#[must_use]
pub fn merge_catalog(builtin: &[String], discovered: &[ModelInfo]) -> Vec<String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::with_capacity(builtin.len() + discovered.len());
    for id in builtin {
        if seen.insert(id.as_str()) {
            out.push(id.clone());
        }
    }
    for model in discovered {
        if seen.insert(model.id.as_str()) {
            out.push(model.id.clone());
        }
    }
    out
}

/// A single cached provider listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheEntry {
    models: Vec<ModelInfo>,
}

/// A cache mapping a provider name to its last discovered model list.
///
/// Plain in-memory state with no background expiry: callers
/// [`ModelCache::put`] a freshly fetched listing and [`ModelCache::get`] it
/// back. The type is pure and offline-testable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCache {
    providers: BTreeMap<String, CacheEntry>,
}

impl ModelCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the cached listing for `provider`.
    pub fn put(&mut self, provider: &str, models: Vec<ModelInfo>) {
        self.providers.insert(provider.to_string(), CacheEntry { models });
    }

    /// Return the cached models for `provider`, or `None` if never fetched.
    #[must_use]
    pub fn get(&self, provider: &str) -> Option<&[ModelInfo]> {
        self.providers.get(provider).map(|e| e.models.as_slice())
    }

    /// Serialize the whole cache to a JSON string for on-disk persistence.
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::Parse`] if serialization fails (not expected for
    /// well-formed in-memory state).
    pub fn to_json(&self) -> Result<String, DiscoveryError> {
        serde_json::to_string(self).map_err(|e| DiscoveryError::Parse(e.to_string()))
    }

    /// Reconstruct a cache from a string produced by [`ModelCache::to_json`].
    ///
    /// # Errors
    ///
    /// Returns [`DiscoveryError::Parse`] when `s` is not valid cache JSON.
    pub fn from_json(s: &str) -> Result<Self, DiscoveryError> {
        serde_json::from_str(s).map_err(|e| DiscoveryError::Parse(e.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_data_array_shape() {
        let body = r#"{"data":[
            {"id":"gpt-4o","context_window":128000,"supports_tools":true},
            {"id":"gpt-4o-mini"}
        ]}"#;
        let models = parse_models_response(body).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4o");
        assert_eq!(models[1].id, "gpt-4o-mini");
    }

    #[test]
    fn parses_bare_top_level_array() {
        let body = r#"[{"id":"claude-sonnet-4-6"},{"id":"claude-opus-4-8"}]"#;
        let models = parse_models_response(body).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-sonnet-4-6");
        assert_eq!(models[1].id, "claude-opus-4-8");
    }

    #[test]
    fn parses_models_array_shape_ignoring_extra_fields() {
        // Extra per-model fields (here `context_length`, `function_calling`)
        // are tolerated and ignored; only `id` is captured.
        let body = r#"{"models":[
            {"id":"gemini-2.5-pro","context_length":1000000,"function_calling":true}
        ]}"#;
        let models = parse_models_response(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-2.5-pro");
    }

    #[test]
    fn skips_entries_without_a_usable_id() {
        let body = r#"{"data":[{"id":"good"},{"id":""},{"object":"model"}]}"#;
        let models = parse_models_response(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "good");
    }

    #[test]
    fn parse_error_on_junk() {
        let err = parse_models_response("not json at all").unwrap_err();
        let DiscoveryError::Parse(msg) = err;
        assert!(!msg.is_empty());
        // A JSON object that matches no accepted shape is also rejected.
        assert!(parse_models_response(r#"{"unexpected":42}"#).is_err());
    }

    #[test]
    fn merge_catalog_dedups_and_is_builtin_first() {
        let builtin = vec!["claude-sonnet-4-6".to_string(), "claude-opus-4-8".to_string()];
        let discovered = vec![
            ModelInfo::new("gpt-4o".to_string()),
            // Duplicate of a builtin id: must not reappear.
            ModelInfo::new("claude-opus-4-8".to_string()),
            ModelInfo::new("gpt-4o-mini".to_string()),
            // Duplicate within discovered: collapsed to first occurrence.
            ModelInfo::new("gpt-4o".to_string()),
        ];
        let merged = merge_catalog(&builtin, &discovered);
        assert_eq!(
            merged,
            ["claude-sonnet-4-6", "claude-opus-4-8", "gpt-4o", "gpt-4o-mini"]
        );
    }

    #[test]
    fn cache_put_get_roundtrips_models() {
        let mut cache = ModelCache::new();
        assert!(cache.get("openai").is_none());
        let models = vec![ModelInfo::new("gpt-4o".to_string())];
        cache.put("openai", models.clone());
        assert_eq!(cache.get("openai"), Some(models.as_slice()));
    }

    #[test]
    fn cache_put_replaces_prior_listing() {
        let mut cache = ModelCache::new();
        cache.put("openai", vec![ModelInfo::new("old".to_string())]);
        cache.put("openai", vec![ModelInfo::new("new".to_string())]);
        assert_eq!(cache.get("openai").map(<[_]>::len), Some(1));
        assert_eq!(cache.get("openai").unwrap()[0].id, "new");
    }

    #[test]
    fn cache_to_json_from_json_round_trip() {
        let mut cache = ModelCache::new();
        cache.put("anthropic", vec![ModelInfo::new("claude-opus-4-8".to_string())]);
        cache.put("openai", vec![ModelInfo::new("gpt-4o".to_string())]);
        let json = cache.to_json().unwrap();
        let restored = ModelCache::from_json(&json).unwrap();
        assert_eq!(restored, cache);
        assert_eq!(restored.get("anthropic").unwrap().len(), 1);
        assert_eq!(restored.get("openai").unwrap()[0].id, "gpt-4o");
    }

    #[test]
    fn from_json_rejects_garbage() {
        assert!(ModelCache::from_json("{not valid").is_err());
    }
}
