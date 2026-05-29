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
        };
        save_to(&p, &cfg).expect("save");
        assert!(p.exists());
        assert!(!p.with_extension("toml.tmp").exists());
    }
}
