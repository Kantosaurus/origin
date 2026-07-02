// SPDX-License-Identifier: Apache-2.0
//! Daemon-wide configuration knobs sourced from env vars.
//!
//! Each accessor here is a small free function so it can be unit-tested
//! without spinning up the rest of the daemon. The binary in `main.rs`
//! re-exports them where it needs them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use origin_conseca::SecurityPolicy;
use origin_permission::bloom::BloomPreCheck;
use origin_permission::rules::Rule;
use origin_policy::{PolicyEngine, PolicyLayer, Tier};
use serde::Deserialize;

/// Canonical session scope for user-configured permission rules.
///
/// The daemon passes this single scope to the rule lookup; an author writes
/// `scope = "*"` (the default) to make a rule apply to every session. Kept as
/// one constant so the loader-side bloom keys and the dispatch-side check agree
/// exactly.
pub const PERMISSION_RULE_SCOPE: &str = "*";

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
    /// User-configured `[[permission_rules]]` (P10.12). Each entry pre-answers a
    /// `RequiresPermission` prompt for a named tool: an explicit `allow = true`
    /// auto-approves and `allow = false` blocks, before the interactive prompter
    /// is consulted. An empty list (the default) ⇒ no rules ⇒ byte-identical.
    #[serde(default)]
    permission_rules: Vec<PermissionRuleConfig>,
    /// Optional `[post_edit]` section: per-extension formatter overrides plus
    /// `auto_lint`/`auto_test` policy and a bounded repair budget. Absent ⇒
    /// `None` ⇒ the daemon's post-edit path consults only the builtin formatter
    /// table (byte-identical default).
    #[serde(default)]
    post_edit: Option<PostEditConfigToml>,
    /// Optional `[notify]` section: quiet-hours window and the delivery channel
    /// for completion notifications. Absent ⇒ `None` ⇒ `ORIGIN_NOTIFY=1` spawns
    /// the desktop toast unconditionally exactly as before (byte-identical).
    #[serde(default)]
    notify: Option<NotifyConfigToml>,
}

/// On-disk `[post_edit]` section.
///
/// A config-surface mirror of [`origin_postedit::PostEditConfig`]; the
/// `format_overrides` are expressed as a TOML table (`ext = "command"`) rather
/// than the crate's `Vec<(String, String)>` so the file stays readable. Every
/// field is optional and defaults to the crate's own default, so an empty
/// `[post_edit]` section is equivalent to `PostEditConfig::default()`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PostEditConfigToml {
    /// Run a linter after each successful edit.
    #[serde(default)]
    auto_lint: bool,
    /// Lint command (program + flags) run after an edit when `auto_lint`.
    #[serde(default)]
    lint_command: Option<String>,
    /// Run the test suite after each successful edit.
    #[serde(default)]
    auto_test: bool,
    /// Test command (program + flags) run after an edit when `auto_test`.
    #[serde(default)]
    test_command: Option<String>,
    /// Reproduction gate: instruct the model to write a failing test FIRST for a
    /// reported bug, and enforce the fail→pass transition at turn-end via
    /// `test_command` — even outside a `/goal`. Implies a turn-end test run.
    /// Omitted ⇒ `false` (the crate default), byte-identical to before.
    #[serde(default)]
    repro_gate: bool,
    /// Per-extension formatter overrides as `{ ext = "command" }`. Tried before
    /// the builtin formatter table; extensions match case-insensitively.
    #[serde(default)]
    format_overrides: std::collections::BTreeMap<String, String>,
    /// Maximum automatic repair attempts after a failing check. Omitted ⇒ the
    /// crate default (2).
    #[serde(default)]
    max_repair_iters: Option<u32>,
}

/// On-disk `[notify]` section.
///
/// Maps to the [`origin_notify`] policy layer. `quiet_start`/`quiet_end` are
/// minutes-of-day (`0..=1439`); when both are present a
/// [`origin_notify::QuietHours`] window is built and non-urgent completion
/// notifications inside it are suppressed. `channel` selects delivery; omitted
/// ⇒ `desktop` (the historical behavior).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyConfigToml {
    /// Inclusive quiet-window start, minutes since midnight.
    #[serde(default)]
    quiet_start: Option<u32>,
    /// Exclusive quiet-window end, minutes since midnight.
    #[serde(default)]
    quiet_end: Option<u32>,
    /// Delivery channel: `"desktop"` (default), `"webhook"`, or `"command"`.
    #[serde(default)]
    channel: Option<String>,
    /// Absolute URL for the `webhook` channel.
    #[serde(default)]
    webhook_url: Option<String>,
    /// Program for the `command` channel.
    #[serde(default)]
    command_program: Option<String>,
    /// Arguments for the `command` channel.
    #[serde(default)]
    command_args: Vec<String>,
}

/// One `[[permission_rules]]` entry. `tool` is the canonical tool name (e.g.
/// `"Bash"`, `"Read"`); `scope` defaults to [`PERMISSION_RULE_SCOPE`] (`"*"`,
/// "every session") and `allow` says whether a match auto-approves (`true`) or
/// blocks (`false`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionRuleConfig {
    /// Canonical tool name this rule answers for.
    tool: String,
    /// Scope the rule applies at; defaults to `"*"` (all sessions).
    #[serde(default = "default_rule_scope")]
    scope: String,
    /// `true` ⇒ auto-approve, `false` ⇒ block.
    allow: bool,
}

fn default_rule_scope() -> String {
    PERMISSION_RULE_SCOPE.to_owned()
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

/// User-configured permission rules plus a pre-built bloom pre-check (P10.12).
///
/// Threaded into `LoopOptions` together and held behind an `Arc` so it can be
/// cheaply shared across every per-request loop. The bloom is built once at
/// load time so dispatch never rebuilds it per turn.
#[derive(Debug)]
pub struct PermissionRules {
    /// The exact rule list, walked on a bloom hit.
    pub rules: Vec<Rule>,
    /// Pre-built bloom over every rule's canonical key.
    pub bloom: BloomPreCheck,
}

/// The governance handles threaded into `LoopOptions`. All are `None`/empty when
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
    /// User-configured permission rules + bloom, or `None` when no
    /// `[[permission_rules]]` are set (byte-identical default).
    pub permission_rules: Option<Arc<PermissionRules>>,
    /// Resolved post-edit policy, or `None` when no `[post_edit]` section is set
    /// (byte-identical default: builtin formatter table only, no lint/test).
    pub post_edit: Option<Arc<origin_postedit::PostEditConfig>>,
    /// Resolved notification policy, or `None` when no `[notify]` section is set
    /// (byte-identical default: unconditional desktop toast under
    /// `ORIGIN_NOTIFY=1`).
    pub notify: Option<Arc<NotifyPolicy>>,
}

/// Resolved notification policy threaded into `LoopOptions` (P-notify).
///
/// Built from the on-disk `[notify]` section. Holds an optional quiet-hours
/// window and the delivery channel; the loop consults
/// [`origin_notify::should_send`] before dispatching over `channel`.
#[derive(Debug, Clone)]
pub struct NotifyPolicy {
    /// Quiet-hours window; `None` ⇒ no suppression.
    pub quiet: Option<origin_notify::QuietHours>,
    /// Delivery channel for completion notifications.
    pub channel: origin_notify::Channel,
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
    // Build the permission-rule bundle once at load time (rules + bloom). An
    // empty `[[permission_rules]]` ⇒ `None` ⇒ dispatch never consults the rule
    // path, byte-identical to before this wiring.
    let permission_rules = if cfg.permission_rules.is_empty() {
        None
    } else {
        let rules: Vec<Rule> = cfg
            .permission_rules
            .into_iter()
            .map(|r| Rule {
                tool_name: r.tool,
                scope: r.scope,
                allow: r.allow,
            })
            .collect();
        let bloom = BloomPreCheck::build(&rules);
        Some(Arc::new(PermissionRules { rules, bloom }))
    };
    // `[post_edit]`: map the config-surface struct onto the crate's
    // `PostEditConfig`. Absent ⇒ `None` ⇒ the post-edit path uses only the
    // builtin formatter table, byte-identical to before this wiring.
    let post_edit = cfg.post_edit.map(|pe| {
        let mut base = origin_postedit::PostEditConfig {
            auto_lint: pe.auto_lint,
            lint_command: pe.lint_command,
            auto_test: pe.auto_test,
            test_command: pe.test_command,
            repro_gate: pe.repro_gate,
            format_overrides: pe.format_overrides.into_iter().collect(),
            ..origin_postedit::PostEditConfig::default()
        };
        if let Some(iters) = pe.max_repair_iters {
            base.max_repair_iters = iters;
        }
        Arc::new(base)
    });
    // `[notify]`: build the quiet-hours window (only when both bounds are set)
    // and select the delivery channel. Absent ⇒ `None` ⇒ the desktop toast is
    // spawned unconditionally under `ORIGIN_NOTIFY=1`, byte-identical.
    let notify = cfg.notify.map(|n| {
        let quiet = match (n.quiet_start, n.quiet_end) {
            (Some(s), Some(e)) => Some(origin_notify::QuietHours::new(s, e)),
            _ => None,
        };
        let channel = match n.channel.as_deref() {
            Some("webhook") => origin_notify::Channel::Webhook {
                url: n.webhook_url.unwrap_or_default(),
            },
            Some("command") => origin_notify::Channel::Command {
                program: n.command_program.unwrap_or_default(),
                args: n.command_args,
            },
            // "desktop", an unknown value, or an omitted channel all fall back
            // to the desktop toast (the historical default).
            _ => origin_notify::Channel::Desktop,
        };
        Arc::new(NotifyPolicy { quiet, channel })
    });
    Ok(Governance {
        policy,
        conseca,
        browser_max_actions,
        permission_rules,
        post_edit,
        notify,
    })
}

/// Probe `root` for the ecosystem marker files [`origin_postedit::detect_test_command`]
/// keys on. Pure I/O — the decision (which command) stays in `origin-postedit`.
///
/// The `Makefile` probe additionally greps for a `test:` target so we only pick
/// `make test` when it actually exists (a bare `Makefile` with no test target
/// would otherwise mislead the gate).
#[must_use]
pub fn probe_repo_markers(root: &Path) -> origin_postedit::RepoMarkers {
    let has = |name: &str| root.join(name).exists();
    let makefile_test_target = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(root.join(f)).ok())
        .any(|body| {
            // A line beginning `test:` (optionally `.PHONY`-adjacent). Cheap and
            // conservative; false negatives just leave the gate unset.
            body.lines()
                .any(|l| l.trim_start().starts_with("test:") || l.trim_start().starts_with("test :"))
        });
    origin_postedit::RepoMarkers {
        cargo_toml: has("Cargo.toml"),
        package_json: has("package.json"),
        pyproject_toml: has("pyproject.toml"),
        python_legacy: has("setup.py") || has("setup.cfg") || has("tox.ini"),
        go_mod: has("go.mod"),
        pom_xml: has("pom.xml"),
        build_gradle: has("build.gradle") || has("build.gradle.kts"),
        gemfile: has("Gemfile"),
        makefile_test_target,
    }
}

/// Resolve the effective post-edit policy for a session, honoring the
/// `ORIGIN_REPRO_GATE` env on-switch.
///
/// Precedence:
/// 1. If `ORIGIN_REPRO_GATE` is unset/`0`/`false` ⇒ return `base` unchanged
///    (the `[post_edit]` config, or `None`) — byte-identical to before.
/// 2. Otherwise the reproduction/regression gate is requested for THIS run:
///    - start from the configured `[post_edit]` (so a user's formatter
///      overrides / `max_repair_iters` are preserved) or a fresh default,
///    - set `repro_gate = true`,
///    - if no `test_command` is configured, auto-derive one from `root`'s
///      ecosystem markers ([`probe_repo_markers`] + `detect_test_command`).
///
/// When the gate is requested but no test command can be resolved (unknown
/// ecosystem, no marker files) the returned config still carries `repro_gate`
/// so the `<repro-gate>` *instruction* is injected, but `test_gate_armed()`
/// stays `false` so nothing is executed — the loop degrades to a prompt-only
/// nudge rather than blocking on a command that does not exist.
///
/// The env value may also be an explicit command (anything other than the
/// boolean tokens), in which case it is used verbatim as the `test_command`
/// — an escape hatch for repos whose runner auto-detection can't guess.
#[must_use]
pub fn resolve_post_edit_with_repro_gate(
    base: Option<Arc<origin_postedit::PostEditConfig>>,
    root: &Path,
) -> Option<Arc<origin_postedit::PostEditConfig>> {
    let Some(raw) = std::env::var("ORIGIN_REPRO_GATE").ok() else {
        return base;
    };
    let raw = raw.trim();
    // Off tokens ⇒ no change.
    if raw.is_empty() || raw == "0" || raw.eq_ignore_ascii_case("false") || raw.eq_ignore_ascii_case("off") {
        return base;
    }
    // On. Start from the configured policy (preserve overrides) or a default.
    let mut cfg = base
        .as_deref()
        .cloned()
        .unwrap_or_default();
    cfg.repro_gate = true;
    // An explicit non-boolean value is a command override.
    let explicit_cmd = if raw == "1" || raw.eq_ignore_ascii_case("true") || raw.eq_ignore_ascii_case("on") {
        None
    } else {
        Some(raw.to_string())
    };
    if let Some(cmd) = explicit_cmd {
        cfg.test_command = Some(cmd);
    } else if cfg.test_command.is_none() {
        cfg.test_command = origin_postedit::detect_test_command(&probe_repo_markers(root));
    }
    if cfg.test_command.is_some() {
        tracing::info!(
            test_command = cfg.test_command.as_deref().unwrap_or(""),
            "repro-gate armed: turn-end tests will be enforced without a /goal"
        );
    } else {
        tracing::warn!(
            "ORIGIN_REPRO_GATE set but no test command could be resolved for this repo; \
             injecting the repro-gate instruction only (no turn-end enforcement). \
             Set ORIGIN_REPRO_GATE=<command> or [post_edit] test_command to enforce."
        );
    }
    Some(Arc::new(cfg))
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

    /// Serializes tests that mutate the process-wide `ORIGIN_REPRO_GATE` env var
    /// so they can't interleave (the workspace convention for env-var tests).
    static REPRO_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn probe_repo_markers_detects_common_ecosystems() {
        let dir = tempfile::tempdir().unwrap();
        // Rust + a Makefile WITHOUT a test target.
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::fs::write(dir.path().join("Makefile"), "build:\n\tcargo build\n").unwrap();
        let m = probe_repo_markers(dir.path());
        assert!(m.cargo_toml);
        assert!(!m.makefile_test_target, "no `test:` target ⇒ not detected");
        assert!(!m.package_json);

        // Add a Makefile test target and a package.json.
        std::fs::write(dir.path().join("Makefile"), "test:\n\tcargo test\n").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let m2 = probe_repo_markers(dir.path());
        assert!(m2.makefile_test_target, "`test:` target ⇒ detected");
        assert!(m2.package_json);
        // Cargo still wins the derived command (precedence).
        assert_eq!(
            origin_postedit::detect_test_command(&m2).as_deref(),
            Some("cargo test")
        );
    }

    #[test]
    fn repro_gate_env_unset_passes_config_through() {
        let _guard = REPRO_ENV_LOCK.lock().unwrap();
        std::env::remove_var("ORIGIN_REPRO_GATE");
        let dir = tempfile::tempdir().unwrap();
        // No base config ⇒ stays None (byte-identical default).
        assert!(resolve_post_edit_with_repro_gate(None, dir.path()).is_none());
        // A base config passes through unchanged.
        let base = Arc::new(origin_postedit::PostEditConfig {
            auto_lint: true,
            lint_command: Some("cargo clippy".into()),
            ..Default::default()
        });
        let out = resolve_post_edit_with_repro_gate(Some(base.clone()), dir.path()).unwrap();
        assert_eq!(*out, *base, "unset ⇒ config unchanged");
        assert!(!out.repro_gate);
    }

    #[test]
    fn repro_gate_env_on_arms_and_auto_derives_command() {
        let _guard = REPRO_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::env::set_var("ORIGIN_REPRO_GATE", "1");
        let out = resolve_post_edit_with_repro_gate(None, dir.path()).unwrap();
        std::env::remove_var("ORIGIN_REPRO_GATE");
        assert!(out.repro_gate, "=1 ⇒ repro_gate on");
        assert_eq!(out.test_command.as_deref(), Some("cargo test"), "auto-derived from Cargo.toml");
        assert!(out.test_gate_armed(), "armed: enforcement will run");
    }

    #[test]
    fn repro_gate_env_explicit_command_wins() {
        let _guard = REPRO_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // Even with a Cargo.toml present, an explicit command overrides.
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::env::set_var("ORIGIN_REPRO_GATE", "pytest -q tests/test_bug.py");
        let out = resolve_post_edit_with_repro_gate(None, dir.path()).unwrap();
        std::env::remove_var("ORIGIN_REPRO_GATE");
        assert!(out.repro_gate);
        assert_eq!(out.test_command.as_deref(), Some("pytest -q tests/test_bug.py"));
    }

    #[test]
    fn repro_gate_env_on_unknown_ecosystem_injects_instruction_only() {
        let _guard = REPRO_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap(); // no marker files
        std::env::set_var("ORIGIN_REPRO_GATE", "true");
        let out = resolve_post_edit_with_repro_gate(None, dir.path()).unwrap();
        std::env::remove_var("ORIGIN_REPRO_GATE");
        assert!(out.repro_gate, "instruction still injected");
        assert!(out.test_command.is_none(), "no ecosystem ⇒ no command");
        assert!(!out.test_gate_armed(), "no command ⇒ nothing to enforce (prompt-only)");
    }

    #[test]
    fn repro_gate_env_off_tokens_are_noops() {
        let _guard = REPRO_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        for off in ["0", "false", "off", ""] {
            std::env::set_var("ORIGIN_REPRO_GATE", off);
            let out = resolve_post_edit_with_repro_gate(None, dir.path());
            std::env::remove_var("ORIGIN_REPRO_GATE");
            assert!(out.is_none(), "`{off}` ⇒ off ⇒ config unchanged (None)");
        }
    }

    #[test]
    fn repro_gate_env_on_preserves_base_overrides() {
        let _guard = REPRO_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module x\n").unwrap();
        let base = Arc::new(origin_postedit::PostEditConfig {
            max_repair_iters: 5,
            format_overrides: vec![("rs".into(), "leptosfmt".into())],
            ..Default::default()
        });
        std::env::set_var("ORIGIN_REPRO_GATE", "1");
        let out = resolve_post_edit_with_repro_gate(Some(base), dir.path()).unwrap();
        std::env::remove_var("ORIGIN_REPRO_GATE");
        assert!(out.repro_gate);
        assert_eq!(out.test_command.as_deref(), Some("go test ./..."));
        assert_eq!(out.max_repair_iters, 5, "base budget preserved");
        assert_eq!(out.format_overrides.len(), 1, "base overrides preserved");
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
    fn absent_permission_rules_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "[conseca]\nallow_tools = [\"Read\"]\n").unwrap();
        let gov = load_governance(&path).unwrap();
        assert!(
            gov.permission_rules.is_none(),
            "no [[permission_rules]] ⇒ None (byte-identical default)"
        );
    }

    #[test]
    fn permission_rules_build_bundle_with_bloom() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[[permission_rules]]\n\
             tool = \"Bash\"\n\
             allow = false\n\
             [[permission_rules]]\n\
             tool = \"Read\"\n\
             scope = \"*\"\n\
             allow = true\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        let pr = gov.permission_rules.expect("permission rules present");
        assert_eq!(pr.rules.len(), 2);
        // Default scope is "*" when omitted.
        let bash = pr.rules.iter().find(|r| r.tool_name == "Bash").unwrap();
        assert_eq!(bash.scope, PERMISSION_RULE_SCOPE);
        assert!(!bash.allow);
        let read = pr.rules.iter().find(|r| r.tool_name == "Read").unwrap();
        assert!(read.allow);
        // The bloom must contain both canonical keys.
        assert!(pr.bloom.maybe_contains(&format!("Bash@{PERMISSION_RULE_SCOPE}")));
        assert!(pr.bloom.maybe_contains(&format!("Read@{PERMISSION_RULE_SCOPE}")));
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

    #[test]
    fn absent_post_edit_section_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "[conseca]\nallow_tools = [\"Read\"]\n").unwrap();
        let gov = load_governance(&path).unwrap();
        assert!(
            gov.post_edit.is_none(),
            "no [post_edit] section ⇒ None (byte-identical default: builtin formatter table only)"
        );
    }

    #[test]
    fn post_edit_section_maps_overrides_and_lint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[post_edit]\n\
             auto_lint = true\n\
             lint_command = \"cargo clippy\"\n\
             max_repair_iters = 3\n\
             [post_edit.format_overrides]\n\
             rs = \"leptosfmt\"\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        let pe = gov.post_edit.expect("post_edit present");
        assert!(pe.auto_lint);
        assert_eq!(pe.lint_command.as_deref(), Some("cargo clippy"));
        assert_eq!(pe.max_repair_iters, 3);
        // The override must win over the builtin table for `.rs`.
        assert_eq!(pe.formatter_for("lib.rs").as_deref(), Some("leptosfmt"));
        // A non-overridden extension still resolves via the builtin table.
        assert_eq!(pe.formatter_for("main.go").as_deref(), Some("gofmt"));
    }

    #[test]
    fn empty_post_edit_section_equals_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "[post_edit]\n").unwrap();
        let gov = load_governance(&path).unwrap();
        let pe = gov.post_edit.expect("empty [post_edit] still builds a config");
        assert_eq!(*pe, origin_postedit::PostEditConfig::default());
    }

    #[test]
    fn absent_notify_section_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(&path, "[conseca]\nallow_tools = [\"Read\"]\n").unwrap();
        let gov = load_governance(&path).unwrap();
        assert!(
            gov.notify.is_none(),
            "no [notify] section ⇒ None (byte-identical default: unconditional desktop toast)"
        );
    }

    #[test]
    fn notify_section_builds_quiet_hours_and_webhook_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        std::fs::write(
            &path,
            "[notify]\n\
             quiet_start = 1380\n\
             quiet_end = 420\n\
             channel = \"webhook\"\n\
             webhook_url = \"https://example.test/hook\"\n",
        )
        .unwrap();
        let gov = load_governance(&path).unwrap();
        let np = gov.notify.expect("notify present");
        let quiet = np.quiet.expect("quiet hours present");
        // 23:00–07:00 wrap-around: 02:00 is quiet, 12:00 is not.
        assert!(quiet.is_quiet(2 * 60));
        assert!(!quiet.is_quiet(12 * 60));
        assert_eq!(
            np.channel,
            origin_notify::Channel::Webhook {
                url: "https://example.test/hook".to_owned()
            }
        );
    }

    #[test]
    fn notify_section_defaults_to_desktop_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("governance.toml");
        // No channel key ⇒ desktop; no quiet bounds ⇒ no window.
        std::fs::write(&path, "[notify]\n").unwrap();
        let gov = load_governance(&path).unwrap();
        let np = gov.notify.expect("empty [notify] still builds a policy");
        assert!(np.quiet.is_none(), "no quiet bounds ⇒ no suppression window");
        assert_eq!(np.channel, origin_notify::Channel::Desktop);
    }
}
