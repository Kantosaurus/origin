// SPDX-License-Identifier: Apache-2.0
//! Layered governance / managed-settings engine for `origin`.
//!
//! Five precedence tiers (`System` > `Admin` > `Managed` > `Project` > `User`)
//! each contribute a [`PolicyLayer`] of optional rules: tool allow/deny lists,
//! model allow/deny lists, a USD spend cap, trusted-folder roots, and an RBAC
//! role. A [`PolicyEngine`] resolves the stack into effective decisions. This
//! fuses claude's MDM managed settings, gemini's 5-tier policy + trusted
//! folders, and cline/kilo/oc RBAC + model allow-lists + spend limits — minus
//! external SSO, which is out of scope for this pure-logic crate.
//!
//! Resolution rules:
//! - A higher-precedence tier's *deny* is final and cannot be re-allowed below.
//! - Allow-lists *intersect* across the tiers that set one (most restrictive).
//! - The spend cap is the *minimum* across layers that set one.
//! - Trusted folders are the *union* of every layer; a path is trusted if it
//!   equals or is nested under any trusted root.
//! - The effective role comes from the highest-precedence tier that sets one.
//! - Within a single layer, deny beats allow.
//!
//! The crate is pure logic — TOML-loadable layers in, decisions out — with no
//! I/O and no async, so it is trivially testable.
//!
//! ```
//! use origin_policy::{parse_layer, PolicyEngine, Tier};
//!
//! let admin = parse_layer("denied_tools = [\"shell\"]\nmax_spend_usd = 5.0", Tier::Admin)
//!     .expect("valid layer");
//! let user = parse_layer("allowed_tools = [\"shell\", \"read\"]", Tier::User)
//!     .expect("valid layer");
//! let engine = PolicyEngine::new(vec![user, admin]);
//! assert!(!engine.is_tool_allowed("shell")); // admin deny wins
//! assert!(engine.is_tool_allowed("read"));
//! assert_eq!(engine.spend_cap_usd(), Some(5.0));
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Governance tier a [`PolicyLayer`] belongs to, in ascending declaration
/// order of precedence.
///
/// The derived [`Ord`] follows declaration order, so a *larger* value means
/// *higher* precedence: `System > Admin > Managed > Project > User`. Use
/// [`Tier::precedence`] for an explicit numeric rank if comparing across types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// Lowest precedence: the individual user's own settings.
    User,
    /// Project-scoped settings (e.g. a repo's checked-in policy).
    Project,
    /// Organisation-managed settings pushed to the workstation.
    Managed,
    /// Administrator-enforced settings.
    Admin,
    /// Highest precedence: system / OS-level mandatory policy.
    System,
}

impl Tier {
    /// Numeric precedence rank; higher means it overrides lower tiers.
    ///
    /// `User = 0`, `Project = 1`, `Managed = 2`, `Admin = 3`, `System = 4`.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::User => 0,
            Self::Project => 1,
            Self::Managed => 2,
            Self::Admin => 3,
            Self::System => 4,
        }
    }
}

/// One tier's contribution to the policy stack. Every field is optional; an
/// unset field means "this tier expresses no opinion" for that dimension.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyLayer {
    /// Tier this layer applies at (skipped during TOML parse; supplied by the
    /// caller via [`parse_layer`]).
    #[serde(skip)]
    pub tier: Tier,
    /// If set, only these tools are permitted (subject to intersection with
    /// other allow-lists and any deny).
    pub allowed_tools: Option<Vec<String>>,
    /// Tools forbidden at this tier; a deny is final and overrides lower allows.
    pub denied_tools: Option<Vec<String>>,
    /// If set, only these models are permitted.
    pub allowed_models: Option<Vec<String>>,
    /// Models forbidden at this tier.
    pub denied_models: Option<Vec<String>>,
    /// Maximum cumulative spend in USD this tier will tolerate.
    pub max_spend_usd: Option<f64>,
    /// Filesystem roots this tier trusts (prefix match, path-segment aware).
    pub trusted_folders: Option<Vec<String>>,
    /// RBAC role asserted at this tier.
    pub role: Option<String>,
}

impl Default for Tier {
    fn default() -> Self {
        Self::User
    }
}

/// Errors that can arise while loading a [`PolicyLayer`] from TOML.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// The source was not valid TOML or did not match the layer schema.
    #[error("failed to parse policy layer: {0}")]
    Toml(#[from] toml::de::Error),
    /// A `max_spend_usd` value was negative or not finite.
    #[error("max_spend_usd must be finite and non-negative, got {0}")]
    InvalidSpend(f64),
}

/// Parse a single [`PolicyLayer`] from a TOML document, tagging it with `tier`.
///
/// The TOML must contain only the layer's data fields (no `tier` key — the
/// caller owns that). Unknown keys are ignored so newer policies degrade
/// gracefully on older clients.
///
/// # Errors
/// Returns [`PolicyError::Toml`] if `toml_src` is malformed or has a field of
/// the wrong type, and [`PolicyError::InvalidSpend`] if `max_spend_usd` is
/// negative or non-finite.
pub fn parse_layer(toml_src: &str, tier: Tier) -> Result<PolicyLayer, PolicyError> {
    let mut layer: PolicyLayer = toml::from_str(toml_src)?;
    layer.tier = tier;
    if let Some(spend) = layer.max_spend_usd {
        if !spend.is_finite() || spend < 0.0 {
            return Err(PolicyError::InvalidSpend(spend));
        }
    }
    Ok(layer)
}

/// Resolves a stack of [`PolicyLayer`]s into effective governance decisions.
#[derive(Debug, Clone, Default)]
pub struct PolicyEngine {
    layers: Vec<PolicyLayer>,
}

impl PolicyEngine {
    /// Build an engine from `layers`. Order is irrelevant — precedence is taken
    /// from each layer's [`Tier`], so duplicate tiers simply stack.
    #[must_use]
    pub const fn new(layers: Vec<PolicyLayer>) -> Self {
        Self { layers }
    }

    /// Layers sorted from highest precedence to lowest. Within equal tiers the
    /// original relative order is preserved (stable sort).
    fn by_precedence_desc(&self) -> Vec<&PolicyLayer> {
        let mut refs: Vec<&PolicyLayer> = self.layers.iter().collect();
        refs.sort_by(|a, b| b.tier.precedence().cmp(&a.tier.precedence()));
        refs
    }

    /// Decide membership for `item` against allow/deny lists, honouring the
    /// precedence rules: any deny (at any tier) is final, and allow-lists
    /// intersect (an item must appear in *every* allow-list that is set).
    fn is_allowed(
        &self,
        item: &str,
        allow_of: impl Fn(&PolicyLayer) -> Option<&Vec<String>>,
        deny_of: impl Fn(&PolicyLayer) -> Option<&Vec<String>>,
    ) -> bool {
        for layer in &self.layers {
            if deny_of(layer).is_some_and(|d| contains(d, item)) {
                return false;
            }
        }
        for layer in &self.layers {
            if let Some(allow) = allow_of(layer) {
                if !contains(allow, item) {
                    return false;
                }
            }
        }
        true
    }

    /// `true` if `tool` is permitted by the resolved policy. A tool is allowed
    /// unless some tier denies it or some tier's allow-list omits it.
    #[must_use]
    pub fn is_tool_allowed(&self, tool: &str) -> bool {
        self.is_allowed(
            tool,
            |l| l.allowed_tools.as_ref(),
            |l| l.denied_tools.as_ref(),
        )
    }

    /// `true` if `model` is permitted by the resolved policy.
    #[must_use]
    pub fn is_model_allowed(&self, model: &str) -> bool {
        self.is_allowed(
            model,
            |l| l.allowed_models.as_ref(),
            |l| l.denied_models.as_ref(),
        )
    }

    /// The effective spend cap: the minimum `max_spend_usd` across every layer
    /// that sets one, or `None` if no layer constrains spend.
    #[must_use]
    pub fn spend_cap_usd(&self) -> Option<f64> {
        self.layers
            .iter()
            .filter_map(|l| l.max_spend_usd)
            .fold(None, |acc, cap| {
                Some(acc.map_or(cap, |best: f64| best.min(cap)))
            })
    }

    /// `true` if `spent_usd` is within the effective cap (inclusive). When no
    /// cap is set, spending is unconstrained and this always returns `true`.
    #[must_use]
    pub fn within_spend(&self, spent_usd: f64) -> bool {
        self.spend_cap_usd().is_none_or(|cap| spent_usd <= cap)
    }

    /// `true` if `path` is trusted: it equals, or is nested beneath, any
    /// trusted root contributed by any layer (the union of all `trusted_folders`).
    #[must_use]
    pub fn folder_trusted(&self, path: &str) -> bool {
        self.layers
            .iter()
            .filter_map(|l| l.trusted_folders.as_ref())
            .flatten()
            .any(|root| path_under(root, path))
    }

    /// The effective RBAC role: the role from the highest-precedence tier that
    /// sets one, or `None` if no layer asserts a role.
    #[must_use]
    pub fn effective_role(&self) -> Option<String> {
        self.by_precedence_desc()
            .into_iter()
            .find_map(|l| l.role.clone())
    }
}

/// Case-sensitive membership test against a slice of names.
fn contains(list: &[String], item: &str) -> bool {
    list.iter().any(|x| x == item)
}

/// `true` if `path` is `root` itself or a descendant of `root`. Matching is
/// path-segment aware (after normalising `\` to `/` and trimming a trailing
/// separator) so `/srv/app` trusts `/srv/app/sub` but **not** `/srv/apple`.
fn path_under(root: &str, path: &str) -> bool {
    let root = normalize_path(root);
    let path = normalize_path(path);
    if path == root {
        return true;
    }
    // Guard against an empty root matching everything.
    if root.is_empty() {
        return false;
    }
    path.strip_prefix(&root)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Lower-noise path: backslashes to slashes, trailing slash trimmed.
fn normalize_path(p: &str) -> String {
    let slashed = p.replace('\\', "/");
    slashed.trim_end_matches('/').to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn layer(tier: Tier) -> PolicyLayer {
        PolicyLayer {
            tier,
            ..PolicyLayer::default()
        }
    }

    #[test]
    fn tier_precedence_orders_system_highest() {
        assert!(Tier::System > Tier::Admin);
        assert!(Tier::Admin > Tier::Managed);
        assert!(Tier::Managed > Tier::Project);
        assert!(Tier::Project > Tier::User);
        assert_eq!(Tier::System.precedence(), 4);
        assert_eq!(Tier::User.precedence(), 0);
    }

    #[test]
    fn admin_deny_overrides_user_allow() {
        let mut admin = layer(Tier::Admin);
        admin.denied_tools = Some(vec!["shell".into()]);
        let mut user = layer(Tier::User);
        user.allowed_tools = Some(vec!["shell".into(), "read".into()]);
        let engine = PolicyEngine::new(vec![user, admin]);
        assert!(!engine.is_tool_allowed("shell"), "admin deny is final");
        assert!(engine.is_tool_allowed("read"));
    }

    #[test]
    fn allow_lists_intersect_across_tiers() {
        let mut managed = layer(Tier::Managed);
        managed.allowed_models = Some(vec!["a".into(), "b".into(), "c".into()]);
        let mut user = layer(Tier::User);
        user.allowed_models = Some(vec!["b".into(), "c".into(), "d".into()]);
        let engine = PolicyEngine::new(vec![managed, user]);
        // Only the intersection {b, c} is allowed.
        assert!(engine.is_model_allowed("b"));
        assert!(engine.is_model_allowed("c"));
        assert!(!engine.is_model_allowed("a"), "not in user allow-list");
        assert!(!engine.is_model_allowed("d"), "not in managed allow-list");
    }

    #[test]
    fn no_lists_means_everything_allowed() {
        let engine = PolicyEngine::new(vec![layer(Tier::User), layer(Tier::Admin)]);
        assert!(engine.is_tool_allowed("anything"));
        assert!(engine.is_model_allowed("any-model"));
    }

    #[test]
    fn deny_beats_allow_within_a_layer() {
        let mut l = layer(Tier::Project);
        l.allowed_tools = Some(vec!["edit".into(), "shell".into()]);
        l.denied_tools = Some(vec!["shell".into()]);
        let engine = PolicyEngine::new(vec![l]);
        assert!(engine.is_tool_allowed("edit"));
        assert!(!engine.is_tool_allowed("shell"), "deny wins within layer");
    }

    #[test]
    fn spend_cap_is_min_across_layers() {
        let mut admin = layer(Tier::Admin);
        admin.max_spend_usd = Some(50.0);
        let mut user = layer(Tier::User);
        user.max_spend_usd = Some(10.0);
        let mut project = layer(Tier::Project); // sets none
        project.max_spend_usd = None;
        let engine = PolicyEngine::new(vec![admin, user, project]);
        assert_eq!(engine.spend_cap_usd(), Some(10.0));
        assert!(engine.within_spend(10.0), "inclusive of the cap");
        assert!(engine.within_spend(9.99));
        assert!(!engine.within_spend(10.01));
    }

    #[test]
    fn no_spend_cap_is_unconstrained() {
        let engine = PolicyEngine::new(vec![layer(Tier::User)]);
        assert_eq!(engine.spend_cap_usd(), None);
        assert!(engine.within_spend(1_000_000.0));
    }

    #[test]
    fn folder_trust_is_prefix_and_segment_aware() {
        let mut l = layer(Tier::Managed);
        l.trusted_folders = Some(vec!["/srv/app".into(), "C:\\work".into()]);
        let engine = PolicyEngine::new(vec![l]);
        assert!(engine.folder_trusted("/srv/app"), "exact root");
        assert!(engine.folder_trusted("/srv/app/sub/file"), "nested");
        assert!(!engine.folder_trusted("/srv/apple"), "sibling prefix is not trusted");
        assert!(!engine.folder_trusted("/srv"), "parent is not trusted");
        // Windows path normalised to forward slashes.
        assert!(engine.folder_trusted("C:/work/project"));
    }

    #[test]
    fn role_comes_from_highest_tier() {
        let mut user = layer(Tier::User);
        user.role = Some("viewer".into());
        let mut admin = layer(Tier::Admin);
        admin.role = Some("operator".into());
        let mut project = layer(Tier::Project);
        project.role = Some("contributor".into());
        let engine = PolicyEngine::new(vec![user, project, admin]);
        assert_eq!(engine.effective_role().as_deref(), Some("operator"));
        // With no admin role, fall through to the next-highest that sets one.
        let engine2 = PolicyEngine::new(vec![
            {
                let mut u = layer(Tier::User);
                u.role = Some("viewer".into());
                u
            },
            {
                let mut p = layer(Tier::Project);
                p.role = Some("contributor".into());
                p
            },
        ]);
        assert_eq!(engine2.effective_role().as_deref(), Some("contributor"));
    }

    #[test]
    fn parse_layer_reads_toml_and_tags_tier() {
        let src = r#"
            allowed_tools = ["read", "edit"]
            denied_tools = ["shell"]
            allowed_models = ["claude-sonnet-4-6"]
            max_spend_usd = 25.0
            trusted_folders = ["/srv/app"]
            role = "operator"
        "#;
        let layer = parse_layer(src, Tier::Admin).unwrap();
        assert_eq!(layer.tier, Tier::Admin);
        assert_eq!(layer.allowed_tools.as_deref(), Some(&["read".to_string(), "edit".to_string()][..]));
        assert_eq!(layer.denied_tools.as_deref(), Some(&["shell".to_string()][..]));
        assert_eq!(layer.max_spend_usd, Some(25.0));
        assert_eq!(layer.role.as_deref(), Some("operator"));
    }

    #[test]
    fn parse_layer_ignores_unknown_keys() {
        let layer = parse_layer("future_field = 42\nrole = \"viewer\"", Tier::User).unwrap();
        assert_eq!(layer.role.as_deref(), Some("viewer"));
    }

    #[test]
    fn parse_layer_rejects_negative_spend() {
        let err = parse_layer("max_spend_usd = -1.0", Tier::User).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidSpend(v) if v == -1.0));
    }

    #[test]
    fn parse_layer_rejects_malformed_toml() {
        let err = parse_layer("this is = = not toml", Tier::User).unwrap_err();
        assert!(matches!(err, PolicyError::Toml(_)));
    }

    #[test]
    fn end_to_end_resolution_from_toml_layers() {
        let system = parse_layer("denied_models = [\"banned-model\"]", Tier::System).unwrap();
        let admin = parse_layer(
            "allowed_tools = [\"read\", \"edit\", \"shell\"]\nmax_spend_usd = 100.0",
            Tier::Admin,
        )
        .unwrap();
        let user = parse_layer(
            "allowed_tools = [\"read\", \"edit\"]\nmax_spend_usd = 20.0\nrole = \"dev\"",
            Tier::User,
        )
        .unwrap();
        let engine = PolicyEngine::new(vec![user, admin, system]);
        assert!(engine.is_tool_allowed("read"));
        assert!(!engine.is_tool_allowed("shell"), "intersected out by user allow-list");
        assert!(!engine.is_model_allowed("banned-model"));
        assert!(engine.is_model_allowed("some-model"));
        assert_eq!(engine.spend_cap_usd(), Some(20.0));
        assert_eq!(engine.effective_role().as_deref(), Some("dev"));
    }
}
