// SPDX-License-Identifier: Apache-2.0
//! User-level config at `~/.origin/config.toml`.
//!
//! Persists the role → (provider, account, model) mapping captured by the
//! onboarding flow. Secrets stay in the OS keychain via `origin-keyvault`;
//! this file only holds non-sensitive selection metadata, so it is safe to
//! check the file into a dotfiles repo if a user wants to.
//!
//! Schema is versioned so a future format bump can refuse stale files
//! instead of silently misinterpreting them.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Current schema version written by `save`.
pub const SCHEMA_VERSION: u32 = 1;

/// One provider/model selection slot. The `account` is the keyvault account
/// the secret was filed under during onboarding (typically `"default"`).
#[allow(clippy::module_name_repetitions)] // `RoleConfig` is part of the public config API
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleConfig {
    pub provider: String,
    #[serde(default = "default_account")]
    pub account: String,
    pub model: String,
}

fn default_account() -> String {
    "default".to_string()
}

/// Top-level on-disk shape of `~/.origin/config.toml`.
#[allow(clippy::module_name_repetitions)] // `OriginConfig` is the documented public config type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginConfig {
    /// Schema version. Files with a higher version than [`SCHEMA_VERSION`]
    /// are rejected at load time.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Required: the main provider/model the agent loop talks to.
    pub primary: RoleConfig,
    /// Optional: a fallback provider used when the primary errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<RoleConfig>,
    /// Optional: a separate provider/model dedicated to subagent and swarm
    /// workers, so heavy parallel work can flow to a cheaper or faster model
    /// without disturbing the main turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<RoleConfig>,
    /// Optional `[aliases]` table mapping a short alias name to a model target
    /// (`"provider/model"` or a bare model id). When a requested model string
    /// equals a defined alias name, [`resolve_alias`] substitutes the target
    /// before the model is sent to the daemon. Empty (the default) ⇒ no
    /// substitution, behaviour byte-identical. *Closes: aider `--alias`.*
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
}

const fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[allow(clippy::module_name_repetitions)] // `ConfigError` is the public error name
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("home directory not found (set $ORIGIN_HOME or $HOME)")]
    NoHome,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("toml serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("config schema_version {found} > supported {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("invalid --alias `{0}` (expected `name=provider/model` or `name=model`)")]
    AliasSpec(String),
    #[error("--thinking-tokens must be greater than 0")]
    ThinkingTokensZero,
}

/// Resolve `~/.origin/config.toml`. Honors `$ORIGIN_HOME` for tests and
/// alternate-root installs, matching the convention used elsewhere in the
/// CLI (see `crates/origin-cli/src/providers.rs`).
///
/// # Errors
/// Returns [`ConfigError::NoHome`] if neither `$ORIGIN_HOME` nor a home
/// directory can be resolved.
pub fn path() -> Result<PathBuf, ConfigError> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or(ConfigError::NoHome)?;
    Ok(home.join(".origin").join("config.toml"))
}

/// `true` when `~/.origin/config.toml` is present. Used by `main.rs` to
/// auto-trigger onboarding on first run.
#[must_use]
pub fn exists() -> bool {
    path().map(|p| p.exists()).unwrap_or(false)
}

/// Load the config from disk. Returns `Ok(None)` if the file does not exist,
/// `Err` only for genuine read/parse failures so callers can distinguish
/// first-run from corruption.
///
/// # Errors
/// Forwards [`ConfigError`] from [`path`] or [`load_from`] (io, parse,
/// unsupported schema version).
pub fn load() -> Result<Option<OriginConfig>, ConfigError> {
    load_from(&path()?)
}

/// Persist `cfg` atomically to the default location.
///
/// # Errors
/// Forwards [`ConfigError`] from [`path`] or [`save_to`].
pub fn save(cfg: &OriginConfig) -> Result<(), ConfigError> {
    save_to(&path()?, cfg)
}

/// Load from an explicit path.
///
/// Exposed so tests (and a future `--config <path>` flag) can avoid the
/// process-wide `$ORIGIN_HOME` env var, which Rust 1.83 flags `set_var` as
/// `unsafe` and can race other threads.
///
/// # Errors
/// Returns [`ConfigError::Io`] on read failure, [`ConfigError::Parse`] on
/// malformed TOML, or [`ConfigError::UnsupportedSchemaVersion`] if the file
/// declares a `schema_version` newer than [`SCHEMA_VERSION`].
pub fn load_from(p: &Path) -> Result<Option<OriginConfig>, ConfigError> {
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(p)?;
    let cfg: OriginConfig = toml::from_str(&raw)?;
    if cfg.schema_version > SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchemaVersion {
            found: cfg.schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(Some(cfg))
}

/// Persist to an explicit path. Creates the parent directory if missing,
/// writes to a `.tmp` sibling, then renames — so a crash mid-write can't
/// leave a half-written `config.toml`.
///
/// # Errors
/// Returns [`ConfigError::Io`] on directory create / write / rename failure
/// or [`ConfigError::Serialize`] if `cfg` fails to serialise.
pub fn save_to(p: &Path, cfg: &OriginConfig) -> Result<(), ConfigError> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(cfg)?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// Validate an extended-thinking budget supplied via `--thinking-tokens`.
///
/// `None` passes through (the flag was not given ⇒ wire unchanged). `Some(0)` is
/// rejected with a clear error, since a zero budget is meaningless and the
/// Anthropic API rejects it. Any positive value passes through unchanged.
///
/// Centralising the check here gives both the global flag (startup seed) and the
/// `run`-level flag a single, unit-testable validation point.
///
/// # Errors
/// Returns [`ConfigError::ThinkingTokensZero`] when `value` is `Some(0)`.
pub const fn validate_thinking_tokens(value: Option<u32>) -> Result<Option<u32>, ConfigError> {
    match value {
        Some(0) => Err(ConfigError::ThinkingTokensZero),
        other => Ok(other),
    }
}

/// Resolve a requested model string against an alias map.
///
/// When `model` exactly matches a key in `aliases`, the mapped target (a
/// `"provider/model"` pair or a bare model id) is returned. Otherwise `model`
/// is returned unchanged. This is the single CLI-side resolution point: callers
/// run it on the model string just before building the `PromptRequest`, so an
/// undefined alias — or any literal model id — passes through byte-identically.
///
/// The lookup is exact-match on the whole string; a `provider/model` target is
/// returned verbatim (the daemon already understands the `provider/model` form),
/// and a bare-model-id target is likewise returned as-is. Resolution is **not**
/// transitive: an alias whose target is itself another alias name is returned as
/// the literal target, avoiding any cycle risk.
#[must_use]
pub fn resolve_alias(aliases: &BTreeMap<String, String>, model: &str) -> String {
    aliases
        .get(model)
        .map_or_else(|| model.to_string(), Clone::clone)
}

/// Parse repeated ad-hoc `name=target` alias definitions (from `--alias`).
///
/// Returns a map merged on top of an optional base map (typically the config
/// `[aliases]`); ad-hoc entries override config entries with the same name.
///
/// Each `spec` must contain a single `=`; the part before is the alias name and
/// the part after is the target (`provider/model` or a bare model id). Both
/// sides are trimmed. Entries with an empty name or empty target, or missing the
/// `=`, are reported as errors rather than silently dropped, so a typo is loud.
///
/// # Errors
/// Returns [`ConfigError::AliasSpec`] describing the first malformed `spec`.
pub fn merge_alias_specs(
    base: &BTreeMap<String, String>,
    specs: &[String],
) -> Result<BTreeMap<String, String>, ConfigError> {
    let mut out = base.clone();
    for spec in specs {
        let (name, target) = spec
            .split_once('=')
            .ok_or_else(|| ConfigError::AliasSpec(spec.clone()))?;
        let (name, target) = (name.trim(), target.trim());
        if name.is_empty() || target.is_empty() {
            return Err(ConfigError::AliasSpec(spec.clone()));
        }
        out.insert(name.to_string(), target.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(provider: &str) -> RoleConfig {
        RoleConfig {
            provider: provider.into(),
            account: "default".into(),
            model: format!("{provider}-model"),
        }
    }

    #[test]
    fn round_trip_primary_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("config.toml");
        let cfg = OriginConfig {
            schema_version: SCHEMA_VERSION,
            primary: sample("anthropic"),
            backup: None,
            subagent: None,
            aliases: BTreeMap::new(),
        };
        save_to(&p, &cfg).expect("save");
        let loaded = load_from(&p).expect("load").expect("present");
        assert_eq!(cfg, loaded);
    }

    #[test]
    fn round_trip_all_roles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("config.toml");
        let cfg = OriginConfig {
            schema_version: SCHEMA_VERSION,
            primary: sample("anthropic"),
            backup: Some(sample("openai")),
            subagent: Some(sample("ollama")),
            aliases: BTreeMap::new(),
        };
        save_to(&p, &cfg).expect("save");
        let loaded = load_from(&p).expect("load").expect("present");
        assert_eq!(cfg, loaded);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("does-not-exist.toml");
        assert!(load_from(&p).expect("load").is_none());
    }

    #[test]
    fn rejects_future_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("config.toml");
        std::fs::write(
            &p,
            "schema_version = 999\n\n[primary]\nprovider = \"a\"\naccount = \"b\"\nmodel = \"m\"\n",
        )
        .expect("write");
        let err = load_from(&p).expect_err("must reject");
        assert!(matches!(err, ConfigError::UnsupportedSchemaVersion { .. }));
    }

    #[test]
    fn save_is_atomic_via_tmp_sibling() {
        // Ensure no `.tmp` artifact is left behind after a successful save.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("config.toml");
        let cfg = OriginConfig {
            schema_version: SCHEMA_VERSION,
            primary: sample("anthropic"),
            backup: None,
            subagent: None,
            aliases: BTreeMap::new(),
        };
        save_to(&p, &cfg).expect("save");
        assert!(p.exists());
        assert!(!p.with_extension("toml.tmp").exists());
    }

    #[test]
    fn aliases_round_trip_and_serialize() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("config.toml");
        let mut aliases = BTreeMap::new();
        aliases.insert("fast".to_string(), "anthropic/claude-haiku-4".to_string());
        aliases.insert("o".to_string(), "gpt-4o".to_string());
        let cfg = OriginConfig {
            schema_version: SCHEMA_VERSION,
            primary: sample("anthropic"),
            backup: None,
            subagent: None,
            aliases,
        };
        save_to(&p, &cfg).expect("save");
        let loaded = load_from(&p).expect("load").expect("present");
        assert_eq!(cfg, loaded);
    }

    #[test]
    fn empty_aliases_omitted_from_toml() {
        // Default (empty) aliases must not emit an `[aliases]` table, keeping
        // the on-disk file byte-identical to the pre-alias schema.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("config.toml");
        let cfg = OriginConfig {
            schema_version: SCHEMA_VERSION,
            primary: sample("anthropic"),
            backup: None,
            subagent: None,
            aliases: BTreeMap::new(),
        };
        save_to(&p, &cfg).expect("save");
        let raw = std::fs::read_to_string(&p).expect("read");
        assert!(!raw.contains("[aliases]"), "empty aliases must be omitted");
    }

    #[test]
    fn resolve_alias_substitutes_defined() {
        let mut aliases = BTreeMap::new();
        aliases.insert("fast".to_string(), "anthropic/claude-haiku-4".to_string());
        aliases.insert("bare".to_string(), "gpt-4o".to_string());
        // Defined alias → provider/model target.
        assert_eq!(
            resolve_alias(&aliases, "fast"),
            "anthropic/claude-haiku-4"
        );
        // Defined alias → bare model id target.
        assert_eq!(resolve_alias(&aliases, "bare"), "gpt-4o");
    }

    #[test]
    fn resolve_alias_passes_through_undefined() {
        let mut aliases = BTreeMap::new();
        aliases.insert("fast".to_string(), "anthropic/claude-haiku-4".to_string());
        // Undefined alias → returned unchanged.
        assert_eq!(resolve_alias(&aliases, "claude-opus-4-7"), "claude-opus-4-7");
        // A provider/model literal that is not an alias key passes through.
        assert_eq!(
            resolve_alias(&aliases, "anthropic/claude-opus-4-7"),
            "anthropic/claude-opus-4-7"
        );
        // Empty alias map → always pass-through.
        let empty = BTreeMap::new();
        assert_eq!(resolve_alias(&empty, "fast"), "fast");
    }

    #[test]
    fn resolve_alias_is_not_transitive() {
        // An alias whose target is itself another alias name resolves to the
        // literal target (one hop), never chasing the chain.
        let mut aliases = BTreeMap::new();
        aliases.insert("a".to_string(), "b".to_string());
        aliases.insert("b".to_string(), "provider/real-model".to_string());
        assert_eq!(resolve_alias(&aliases, "a"), "b");
    }

    #[test]
    fn merge_alias_specs_parses_and_overrides() {
        let mut base = BTreeMap::new();
        base.insert("fast".to_string(), "config/model".to_string());
        let specs = vec![
            "fast=anthropic/claude-haiku-4".to_string(), // overrides base
            "o = gpt-4o".to_string(),                    // trims whitespace
        ];
        let merged = merge_alias_specs(&base, &specs).expect("parse");
        assert_eq!(merged.get("fast").map(String::as_str), Some("anthropic/claude-haiku-4"));
        assert_eq!(merged.get("o").map(String::as_str), Some("gpt-4o"));
    }

    #[test]
    fn merge_alias_specs_rejects_malformed() {
        let base = BTreeMap::new();
        for bad in ["noequals", "=missingname", "missingtarget="] {
            let err = merge_alias_specs(&base, &[bad.to_string()]).expect_err("must reject");
            assert!(matches!(err, ConfigError::AliasSpec(_)), "spec `{bad}`");
        }
    }

    #[test]
    fn validate_thinking_tokens_passes_none_and_positive() {
        assert_eq!(validate_thinking_tokens(None).expect("ok"), None);
        assert_eq!(validate_thinking_tokens(Some(1)).expect("ok"), Some(1));
        assert_eq!(validate_thinking_tokens(Some(4_096)).expect("ok"), Some(4_096));
    }

    #[test]
    fn validate_thinking_tokens_rejects_zero() {
        let err = validate_thinking_tokens(Some(0)).expect_err("zero must be rejected");
        assert!(matches!(err, ConfigError::ThinkingTokensZero));
    }
}
