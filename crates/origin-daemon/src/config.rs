// SPDX-License-Identifier: Apache-2.0
//! Daemon-wide configuration knobs sourced from env vars.
//!
//! Each accessor here is a small free function so it can be unit-tested
//! without spinning up the rest of the daemon. The binary in `main.rs`
//! re-exports them where it needs them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use origin_conseca::SecurityPolicy;
use origin_policy::{PolicyEngine, PolicyLayer, Tier};
use serde::Deserialize;

/// Resolve the bearer TTL (seconds) surfaced in
/// [`StreamEvent::PairIssued`](crate::protocol::StreamEvent::PairIssued).
///
/// Default: one day ([`origin_mem::SECS_PER_DAY`]). Overridable via the
/// `ORIGIN_BEARER_TTL_SECS` env var. Saturates at `u32::MAX` — the wire
/// field is a `u32`. Non-numeric overrides are ignored.
#[must_use]
pub fn bearer_ttl_secs() -> u32 {
    if let Ok(raw) = std::env::var("ORIGIN_BEARER_TTL_SECS") {
        if let Ok(n) = raw.parse::<u32>() {
            return n;
        }
    }
    u32::try_from(origin_mem::SECS_PER_DAY).unwrap_or(u32::MAX)
}

/// Governance tier as named in `governance.toml`. Mirrors
/// [`origin_policy::Tier`] but is a config-surface enum so the on-disk spelling
/// is stable and case-insensitive, independent of the engine's internal repr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TierName {
    User,
    Project,
    Managed,
    Admin,
    System,
}

impl From<TierName> for Tier {
    fn from(t: TierName) -> Self {
        match t {
            TierName::User => Self::User,
            TierName::Project => Self::Project,
            TierName::Managed => Self::Managed,
            TierName::Admin => Self::Admin,
            TierName::System => Self::System,
        }
    }
}

/// One `[[policy_layers]]` entry: a tier tag plus the flattened
/// [`origin_policy::PolicyLayer`] data fields (`allowed_tools`, `denied_tools`,
/// `max_spend_usd`, …). The layer's own `tier` field is `#[serde(skip)]` in the
/// engine crate, so we carry the tier here and stamp it after parse.
#[derive(Debug, Clone, Deserialize)]
struct PolicyLayerConfig {
    tier: TierName,
    #[serde(flatten)]
    layer: PolicyLayer,
}

/// On-disk `governance.toml` schema. Both sections are optional: an empty or
/// absent section contributes `None`, preserving byte-identical default
/// behavior (no policy engine, no per-prompt security policy).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceConfig {
    /// Stacked policy layers across the five precedence tiers. When non-empty,
    /// they are fed to [`PolicyEngine::new`].
    #[serde(default)]
    policy_layers: Vec<PolicyLayerConfig>,
    /// Optional per-prompt `ConSeca` security policy applied to every loop. Its
    /// fields mirror [`origin_conseca::SecurityPolicy`].
    #[serde(default)]
    conseca: Option<SecurityPolicy>,
    /// Optional `[browser]` section carrying browser-security knobs. Absent ⇒
    /// `None` ⇒ no browser rate limit (byte-identical default).
    #[serde(default)]
    browser: Option<BrowserConfig>,
}

/// On-disk `[browser]` section. Currently a single optional knob: the enforced
/// per-session cap on browser-class actions. An absent section (or an omitted
/// field) contributes `None`, so the default daemon path is byte-identical.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserConfig {
    /// ENFORCED maximum number of browser-class actions
    /// (`Browser`/`WebFetch`/`WebSearch`) per `run_loop`. `None` ⇒ unlimited.
    #[serde(default)]
    max_actions_per_session: Option<u32>,
}

/// Errors raised while loading governance configuration.
#[derive(Debug, thiserror::Error)]
pub enum GovernanceError {
    /// The governance file exists but could not be read from disk.
    #[error("failed to read governance config at {path}: {source}")]
    Read {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The governance file was present but was not valid TOML / schema.
    #[error("failed to parse governance config at {path}: {source}")]
    Parse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Underlying TOML error (boxed: `toml::de::Error` is large, and
        /// boxing keeps [`GovernanceError`] small enough to satisfy clippy's
        /// `result_large_err` lint).
        source: Box<toml::de::Error>,
    },
    /// A `max_spend_usd` value was negative or non-finite.
    #[error("invalid max_spend_usd in governance config at {path}: {value}")]
    InvalidSpend {
        /// Path containing the bad value.
        path: PathBuf,
        /// The offending value.
        value: f64,
    },
}

/// The two governance handles threaded into `LoopOptions`. Both are `None` when
/// no configuration is present (byte-identical to the historical default).
#[derive(Debug, Clone, Default)]
pub struct Governance {
    /// Layered policy engine, or `None` when no `[[policy_layers]]` are set.
    pub policy: Option<Arc<PolicyEngine>>,
    /// Per-prompt security policy, or `None` when no `[conseca]` is set.
    pub conseca: Option<Arc<SecurityPolicy>>,
    /// ENFORCED per-session browser-action cap, or `None` when no
    /// `[browser] max_actions_per_session` is set (byte-identical default).
    pub browser_max_actions: Option<u32>,
}

/// Resolve the on-disk governance config path.
///
/// Uses `ORIGIN_GOVERNANCE_PATH` if set, otherwise
/// `<home>/.origin/governance.toml` where `<home>` honours `ORIGIN_HOME` then
/// the OS home dir (mirrors the skill/workflow loaders).
#[must_use]
pub fn governance_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("ORIGIN_GOVERNANCE_PATH") {
        return PathBuf::from(explicit);
    }
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".origin").join("governance.toml")
}

/// Load governance configuration from `path`.
///
/// When `path` does not exist, returns a default (`None`/`None`) [`Governance`]
/// so the daemon behaves byte-identically to today. When the file is present it
/// is parsed and validated; an empty file (or one with empty sections) also
/// yields `None`/`None`.
///
/// # Errors
///
/// Returns [`GovernanceError`] when the file exists but cannot be read, is not
/// valid TOML/schema, or carries an invalid `max_spend_usd`.
pub fn load_governance(path: &Path) -> Result<Governance, GovernanceError> {
    if !path.exists() {
        return Ok(Governance::default());
    }
    let raw = std::fs::read_to_string(path).map_err(|source| GovernanceError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: GovernanceConfig = toml::from_str(&raw).map_err(|source| GovernanceError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    governance_from_config(cfg, path)
}

/// Convert a parsed [`GovernanceConfig`] into the threadable [`Governance`].
/// Factored out so it can be unit-tested without touching the filesystem.
fn governance_from_config(cfg: GovernanceConfig, path: &Path) -> Result<Governance, GovernanceError> {
    let policy = if cfg.policy_layers.is_empty() {
        None
    } else {
        let mut layers = Vec::with_capacity(cfg.policy_layers.len());
        for entry in cfg.policy_layers {
            let mut layer = entry.layer;
            layer.tier = entry.tier.into();
            if let Some(spend) = layer.max_spend_usd {
                if !spend.is_finite() || spend < 0.0 {
                    return Err(GovernanceError::InvalidSpend {
                        path: path.to_path_buf(),
                        value: spend,
                    });
                }
            }
            layers.push(layer);
        }
        Some(Arc::new(PolicyEngine::new(layers)))
    };
    let conseca = cfg.conseca.map(Arc::new);
    let browser_max_actions = cfg.browser.and_then(|b| b.max_actions_per_session);
    Ok(Governance {
        policy,
        conseca,
        browser_max_actions,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        let gov = load_governance(&path).unwrap();
        assert!(gov.policy.is_none(), "absent config ⇒ no policy");
        assert!(gov.conseca.is_none(), "absent config ⇒ no conseca");
    }

    #[test]
    fn empty_file_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "").unwrap();
        let gov = load_governance(&path).unwrap();
        assert!(gov.policy.is_none());
        assert!(gov.conseca.is_none());
    }

    #[test]
    fn policy_layers_build_engine_and_deny_a_tool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[[policy_layers]]\n\
             tier = \"admin\"\n\
             denied_tools = [\"Bash\"]\n\
             max_spend_usd = 5.0\n\
             [[policy_layers]]\n\
             tier = \"user\"\n\
             allowed_tools = [\"Bash\", \"Read\"]\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        let engine = gov.policy.expect("policy engine present");
        assert!(!engine.is_tool_allowed("Bash"), "admin deny is final");
        assert!(engine.is_tool_allowed("Read"));
        assert_eq!(engine.spend_cap_usd(), Some(5.0));
        assert!(gov.conseca.is_none(), "no [conseca] ⇒ None");
    }

    #[test]
    fn conseca_section_builds_security_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[conseca]\n\
             allow_tools = [\"Read\"]\n\
             rationale = \"read-only session\"\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        let policy = gov.conseca.expect("conseca policy present");
        assert!(origin_conseca::check_tool(&policy, "Read").is_allow());
        assert!(!origin_conseca::check_tool(&policy, "Bash").is_allow());
        assert!(gov.policy.is_none(), "no policy_layers ⇒ None");
    }

    #[test]
    fn both_sections_build_both_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[[policy_layers]]\n\
             tier = \"system\"\n\
             denied_tools = [\"Write\"]\n\
             [conseca]\n\
             deny_tools = [\"Bash\"]\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        assert!(gov.policy.is_some());
        assert!(gov.conseca.is_some());
    }

    #[test]
    fn negative_spend_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[[policy_layers]]\n\
             tier = \"user\"\n\
             max_spend_usd = -1.0\n",
        )
        .unwrap();
        let err = load_governance(&path).unwrap_err();
        assert!(matches!(err, GovernanceError::InvalidSpend { value, .. } if value == -1.0));
    }

    #[test]
    fn malformed_toml_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        let err = load_governance(&path).unwrap_err();
        assert!(matches!(err, GovernanceError::Parse { .. }));
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        // `deny_unknown_fields` guards against typo'd section names silently
        // disabling governance.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "polcy_layers = []\n").unwrap();
        let err = load_governance(&path).unwrap_err();
        assert!(matches!(err, GovernanceError::Parse { .. }));
    }

    #[test]
    fn browser_section_parses_max_actions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "[browser]\nmax_actions_per_session = 5\n").unwrap();
        let gov = load_governance(&path).unwrap();
        assert_eq!(gov.browser_max_actions, Some(5));
        assert!(gov.policy.is_none());
        assert!(gov.conseca.is_none());
    }

    #[test]
    fn absent_browser_section_yields_no_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "[conseca]\nallow_tools = [\"Read\"]\n").unwrap();
        let gov = load_governance(&path).unwrap();
        assert_eq!(
            gov.browser_max_actions, None,
            "no [browser] section ⇒ no cap (byte-identical default)"
        );
    }

    #[test]
    fn all_five_tiers_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[[policy_layers]]\ntier = \"user\"\n\
             [[policy_layers]]\ntier = \"project\"\n\
             [[policy_layers]]\ntier = \"managed\"\n\
             [[policy_layers]]\ntier = \"admin\"\n\
             [[policy_layers]]\ntier = \"system\"\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        assert!(gov.policy.is_some(), "five empty layers still build an engine");
    }
}
