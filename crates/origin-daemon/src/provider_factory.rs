// SPDX-License-Identifier: Apache-2.0
//! `ProviderFactory` — builds an [`Arc<dyn Provider>`] for a given
//! [`ProviderId`] + account by reading the matching credential from a
//! [`KeyVault`].
//!
//! Compiled-in providers depend on cargo features:
//!   * `anthropic`, `openai`, `gemini`, `ollama` ship by default;
//!   * `openrouter`, `bedrock`, `github-models` are opt-in.
//!
//! Phase 8.9 wires this factory into the daemon so the CLI's `/account`
//! command can hot-swap providers without restarting.

use std::borrow::Cow;
use std::sync::Arc;

use origin_keyvault::{Error as VaultError, KeyVault};
use origin_provider::catalog::Catalog;
use origin_provider::Provider;
use thiserror::Error;

/// Stable id of a provider in the merged catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderId(pub Cow<'static, str>);

impl ProviderId {
    #[must_use]
    pub fn parse(s: &str, catalog: &Catalog) -> Option<Self> {
        let normalised = s.to_ascii_lowercase();
        // Common aliases: "open-ai" → "openai", "aws-bedrock" → "bedrock"
        let canonical = match normalised.as_str() {
            "open-ai" | "open_ai" => "openai",
            "aws-bedrock" => "bedrock",
            "gemini" => "google",
            "github" | "github-models" => "github-copilot",
            "open-router" => "openrouter",
            other => other,
        };
        catalog.lookup(canonical).map(|e| Self(e.id.clone()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors surfaced by [`ProviderFactory::build`].
#[derive(Debug, Error)]
pub enum FactoryError {
    /// The supplied id string did not match any compiled-in provider.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    /// No credential exists in the vault for the requested account.
    #[error("credential not found for provider `{provider}` account `{account}`")]
    MissingCredential { provider: String, account: String },
    /// A credential blob (e.g. the Bedrock JSON) failed to parse.
    #[error("credential parse: {0}")]
    CredentialParse(String),
    /// Wrapped backend failure from the keyvault.
    #[error("keyvault: {0}")]
    Vault(String),
}

impl FactoryError {
    fn from_vault(err: VaultError, provider: &str, account: &str) -> Self {
        match err {
            VaultError::NotFound { .. } => Self::MissingCredential {
                provider: provider.to_string(),
                account: account.to_string(),
            },
            other => Self::Vault(other.to_string()),
        }
    }
}

/// Bedrock credentials stored as a single JSON blob under
/// (`"bedrock"`, `account`). The minimum fields are `access`, `secret`,
/// and `region`; `endpoint` and `model_id` fall back to sensible defaults
/// when omitted so a P8.9 vault entry only needs the three core fields.
// No `Debug` derive — `secret` and `access` are credential material; a
// stray `{:?}` print would leak them. The `xtask lint-secrets` (P11.14)
// CI gate enforces this rule for any `#[derive(Debug)]` struct with a
// secret-looking field.
#[cfg(feature = "bedrock")]
#[derive(serde::Deserialize)]
struct BedrockCreds {
    access: String,
    secret: String,
    region: String,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
}

#[cfg(feature = "bedrock")]
impl BedrockCreds {
    fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| format!("https://bedrock-runtime.{}.amazonaws.com", self.region))
    }

    fn model_id(&self) -> String {
        self.model_id
            .clone()
            .unwrap_or_else(|| "anthropic.claude-3-haiku-20240307-v1:0".to_string())
    }
}

use origin_provider_openai_compat::TokenSource;

#[derive(Clone)]
pub struct ProviderFactory {
    vault: KeyVault,
    cas: Option<Arc<origin_cas::Store>>,
    catalog: Catalog,
    /// N4.3 handle→band index, shared with every Anthropic provider this
    /// factory builds via `Anthropic::with_plan(plan.clone())`. The same
    /// `Plan` instance is also cloned into per-request `LoopOptions` so
    /// the daemon's dispatch site can `register_handle` for each produced
    /// CAS handle; both ends see the same map via `Arc<RwLock<…>>` inside
    /// `Plan`. `None` skips the wiring (preserves the pre-N4.3 behavior
    /// of inlining every handle).
    plan: Option<origin_planner::Plan>,
}

impl core::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProviderFactory")
            .field("vault", &self.vault)
            .field("cas", &self.cas.as_ref().map(|_| "<cas>"))
            .field("catalog_entries", &self.catalog.entries().len())
            .field("plan", &self.plan.as_ref().map(|_| "<plan>"))
            .finish()
    }
}

impl ProviderFactory {
    #[must_use]
    pub const fn new(vault: KeyVault, catalog: Catalog) -> Self {
        Self {
            vault,
            cas: None,
            catalog,
            plan: None,
        }
    }

    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }

    /// Attach the daemon-wide handle→band plan. Cloned into every
    /// Anthropic-wire provider built by this factory.
    #[must_use]
    pub fn with_plan(mut self, plan: origin_planner::Plan) -> Self {
        self.plan = Some(plan);
        self
    }

    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Route a phase to a model via an [`origin_router::Router`] (Task 6).
    ///
    /// This is an *available* helper — it does NOT change the default
    /// single-model selection performed by [`ProviderFactory::build`]; callers
    /// opt in explicitly. It wraps `origin_router` so the daemon can pick a
    /// `ModelRef` for a `phase` from `candidates` under any [`origin_router::Strategy`].
    /// Returns `None` when the strategy yields no model (e.g. an empty
    /// quota-fallback chain or every candidate exhausted).
    #[must_use]
    pub fn route(
        &self,
        strategy: origin_router::Strategy,
        phase: origin_router::Phase,
        candidates: &[origin_router::ModelRef],
    ) -> Option<origin_router::ModelRef> {
        origin_router::Router::new(strategy).choose(phase, candidates)
    }

    /// Build a provider for a cross-provider per-turn routing pick.
    ///
    /// Given a router-chosen `provider_id` (e.g. `"google"`, `"openai"`) this
    /// resolves the credential for `account` and constructs an owned
    /// [`Arc<dyn Provider>`] the agent loop can use in place of the borrowed
    /// active provider for a single turn. The per-turn model id is applied by
    /// the caller via [`ChatRequest::model`](origin_provider::ChatRequest), so
    /// `model` here is informational only (it is *not* baked into the provider).
    ///
    /// Returns `None` — never an error — when the id is unknown to the catalog
    /// or no credential is reachable for `(provider, account)`, so the caller
    /// can fall back to the active provider for that turn without a panic. The
    /// failure reason is logged at `debug`/`warn`.
    ///
    /// This is the cross-provider rebuild scaffold (foundation L84 / kilo L265).
    /// It is reachable from the agent loop only when a factory has been
    /// registered via [`set_global`]; see that function for the wiring contract.
    pub async fn build_provider_for(
        &self,
        provider_id: &str,
        model: &str,
        account: &str,
    ) -> Option<Arc<dyn Provider>> {
        let Some(id) = ProviderId::parse(provider_id, &self.catalog) else {
            tracing::debug!(provider_id, "router cross-provider pick: unknown provider id");
            return None;
        };
        match self.build(&id, account).await {
            Ok(p) => {
                tracing::debug!(provider = id.as_str(), model, account, "router cross-provider rebuild ok");
                Some(p)
            }
            Err(e) => {
                tracing::warn!(
                    provider = id.as_str(),
                    model,
                    account,
                    error = %e,
                    "router cross-provider rebuild failed; caller will fall back to active provider"
                );
                None
            }
        }
    }

    /// Build an [`Arc<dyn Provider>`] for the given catalog id + account.
    ///
    /// # Errors
    /// Returns [`FactoryError::UnknownProvider`] if `id` is not in the merged
    /// catalog, [`FactoryError::MissingCredential`] if the vault has no entry
    /// for `(provider, account)`, [`FactoryError::CredentialParse`] for
    /// malformed credential blobs, or [`FactoryError::Vault`] for any other
    /// keyvault backend failure.
    pub async fn build(&self, id: &ProviderId, account: &str) -> Result<Arc<dyn Provider>, FactoryError> {
        let entry = self
            .catalog
            .lookup(id.as_str())
            .ok_or_else(|| FactoryError::UnknownProvider(id.as_str().to_string()))?
            .clone();
        let token = self.resolve_auth(&entry, account).await?;
        self.build_for_wire(&entry, token, account).await
    }

    async fn resolve_auth(
        &self,
        entry: &origin_provider::catalog::ProviderEntry,
        account: &str,
    ) -> Result<Arc<dyn TokenSource>, FactoryError> {
        use origin_provider::catalog::AuthScheme;
        use origin_provider_openai_compat::{NoAuth, StaticBearer, StaticHeader};
        match &entry.auth {
            AuthScheme::None => Ok(NoAuth::new()),
            AuthScheme::ApiKey { header, prefix } => {
                let secret = self
                    .vault
                    .get(entry.id.as_ref(), account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                if header.eq_ignore_ascii_case("Authorization") && prefix.as_ref() == "Bearer " {
                    Ok(StaticBearer::new(secret.expose().clone()))
                } else {
                    Ok(StaticHeader::new(
                        header.to_string(),
                        prefix.to_string(),
                        secret.expose().clone(),
                    ))
                }
            }
            AuthScheme::OAuth(_) => {
                use origin_keyvault::OAuthClient;
                use std::time::Duration;
                if let AuthScheme::OAuth(spec) = &entry.auth {
                    let client = OAuthClient::new(
                        entry.id.to_string(),
                        spec.token_url.to_string(),
                        spec.client_id.to_string(),
                    );
                    match client
                        .refresh_if_due(&self.vault, account, Duration::from_secs(60))
                        .await
                    {
                        Ok(origin_keyvault::RefreshOutcome::Rotated { access }) => {
                            Ok(StaticBearer::new(access.expose().to_string()))
                        }
                        Ok(origin_keyvault::RefreshOutcome::NotDue { .. } | _) => {
                            let secret = self
                                .vault
                                .get(entry.id.as_ref(), &format!("{account}/oauth"))
                                .await
                                .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                            let stored: serde_json::Value = serde_json::from_str(secret.expose())
                                .map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
                            let access = stored.get("access").and_then(|v| v.as_str()).ok_or_else(|| {
                                FactoryError::CredentialParse("oauth blob missing 'access'".into())
                            })?;
                            Ok(StaticBearer::new(access.to_string()))
                        }
                        Err(e) => Err(FactoryError::Vault(e.to_string())),
                    }
                } else {
                    unreachable!()
                }
            }
            AuthScheme::SigV4 { .. } => {
                // SigV4 is handled inside the Bedrock builder — return NoAuth as a placeholder.
                Ok(origin_provider_openai_compat::NoAuth::new())
            }
            AuthScheme::Custom => Ok(origin_provider_openai_compat::NoAuth::new()),
        }
    }

    // `build_for_wire` dispatches across every supported wire format in one
    // place. Splitting per-wire helpers is a multi-provider polish item.
    #[allow(clippy::too_many_lines)]
    async fn build_for_wire(
        &self,
        entry: &origin_provider::catalog::ProviderEntry,
        token: Arc<dyn TokenSource>,
        account: &str,
    ) -> Result<Arc<dyn Provider>, FactoryError> {
        use origin_provider::catalog::WireFormat;
        use origin_provider_openai_compat::{OpenAiCompat, OpenAiCompatConfig};
        // Obtain a `&'static str` for `Provider::name()`. Interned so each
        // distinct provider id is leaked at most once — repeated SwitchAccount
        // calls reuse the cached pointer instead of leaking a fresh string per
        // build (total leaked memory is bounded by the finite catalog id set).
        let name: &'static str = intern_provider_name(entry.id.as_ref());
        match entry.wire {
            WireFormat::OpenAIChat => {
                let cfg = OpenAiCompatConfig {
                    name,
                    base_url: render_base_url(
                        entry.base_url.as_ref(),
                        &self.vault,
                        entry.id.as_ref(),
                        account,
                    )
                    .await?,
                    chat_path: entry.chat_path.to_string(),
                    auth: token,
                    extra_headers: openai_extra_headers(entry.id.as_ref()),
                };
                let mut p = OpenAiCompat::new(cfg);
                if let Some(cas) = self.cas.clone() {
                    // Inflate handle-backed tool results before wire encoding;
                    // the daemon stores every tool result as a CAS handle.
                    p = p.with_cas(cas);
                }
                Ok(Arc::new(p))
            }
            WireFormat::Anthropic => {
                use origin_provider::catalog::AuthScheme;
                let base = entry.base_url.as_ref();
                let mut p = match &entry.auth {
                    AuthScheme::ApiKey { .. } => {
                        let secret = self
                            .vault
                            .get(entry.id.as_ref(), account)
                            .await
                            .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                        origin_provider_anthropic::Anthropic::new(secret.expose().clone()).with_base(base)
                    }
                    AuthScheme::OAuth(_) => {
                        let (_, bearer) = token
                            .header()
                            .await
                            .map_err(|_| FactoryError::Vault("oauth token".into()))?;
                        let raw = bearer.strip_prefix("Bearer ").unwrap_or(&bearer).to_string();
                        origin_provider_anthropic::Anthropic::with_oauth_bearer(raw).with_base(base)
                    }
                    _ => {
                        return Err(FactoryError::CredentialParse(
                            "anthropic wire requires ApiKey or OAuth".into(),
                        ))
                    }
                };
                if let Some(cas) = self.cas.clone() {
                    p = p.with_cas(cas);
                }
                if let Some(plan) = self.plan.clone() {
                    // Shared Plan: cheap clone of the inner Arc<RwLock<…>>.
                    // The dispatch loop registers handles into the same map.
                    p = p.with_plan(plan);
                }
                Ok(Arc::new(p))
            }
            #[cfg(feature = "gemini")]
            WireFormat::Gemini => {
                use origin_provider::catalog::AuthScheme;
                let p = match &entry.auth {
                    AuthScheme::ApiKey { .. } => {
                        let secret = self
                            .vault
                            .get(entry.id.as_ref(), account)
                            .await
                            .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                        origin_provider_gemini::Gemini::new(secret.expose().clone())
                    }
                    AuthScheme::OAuth(_) => {
                        let (_, bearer) = token
                            .header()
                            .await
                            .map_err(|_| FactoryError::Vault("oauth token".into()))?;
                        let raw = bearer.strip_prefix("Bearer ").unwrap_or(&bearer).to_string();
                        origin_provider_gemini::Gemini::with_oauth_bearer(raw)
                    }
                    _ => {
                        return Err(FactoryError::CredentialParse(
                            "gemini wire requires ApiKey or OAuth".into(),
                        ))
                    }
                };
                let p = match self.cas.clone() {
                    Some(cas) => p.with_cas(cas),
                    None => p,
                };
                Ok(Arc::new(p))
            }
            #[cfg(not(feature = "gemini"))]
            WireFormat::Gemini => Err(FactoryError::UnknownProvider("gemini".into())),
            #[cfg(feature = "bedrock")]
            WireFormat::Bedrock => {
                let secret = self
                    .vault
                    .get(entry.id.as_ref(), account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, entry.id.as_ref(), account))?;
                let creds: BedrockCreds = serde_json::from_str(secret.expose())
                    .map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
                let mut p = origin_provider_bedrock::Bedrock::new(
                    creds.endpoint(),
                    creds.region.clone(),
                    creds.model_id(),
                    creds.access.clone(),
                    creds.secret,
                );
                if let Some(cas) = self.cas.clone() {
                    p = p.with_cas(cas);
                }
                Ok(Arc::new(p))
            }
            #[cfg(not(feature = "bedrock"))]
            WireFormat::Bedrock => Err(FactoryError::UnknownProvider("bedrock".into())),
            #[cfg(feature = "ollama")]
            WireFormat::Ollama => {
                let _ = account;
                Ok(Arc::new(origin_provider_ollama::Ollama::new()))
            }
            #[cfg(not(feature = "ollama"))]
            WireFormat::Ollama => Err(FactoryError::UnknownProvider("ollama".into())),
            #[cfg(feature = "github-models")]
            WireFormat::GitHubCopilot => Ok(Arc::new(origin_provider_github::GitHubModels::new(
                self.vault.clone(),
                account,
            ))),
            #[cfg(not(feature = "github-models"))]
            WireFormat::GitHubCopilot => Err(FactoryError::UnknownProvider("github-copilot".into())),
        }
    }
}

/// Process-wide handle the agent loop uses to reach a [`ProviderFactory`] for
/// cross-provider per-turn routing, paired with the default account to resolve
/// credentials against.
///
/// `run_loop` only holds a `&dyn Provider` + `LoopOptions`; it cannot reach the
/// `KeyVault` / `Catalog` a rebuild needs. Rather than thread a new field
/// through `LoopOptions` (its construction site is exhaustive), the daemon can
/// register the factory *once* here at startup. When it is **not** registered
/// (the default), [`global`] returns `None`, the loop never rebuilds, and a
/// cross-provider pick simply falls back to the active provider — byte-identical
/// to the pre-existing same-provider-only behaviour.
static GLOBAL_FACTORY: std::sync::OnceLock<GlobalFactory> = std::sync::OnceLock::new();

/// A registered factory plus the account its rebuilds resolve credentials for.
///
/// `account` is behind a `std::sync::RwLock` so a `/account` switch can update
/// which account a cross-provider rebuild resolves credentials for, after the
/// factory was registered once at startup. The lock is held only long enough to
/// clone the inner `Arc<str>` (never across an `await`), so it cannot deadlock
/// the async loop.
#[derive(Clone)]
struct GlobalFactory {
    factory: Arc<ProviderFactory>,
    account: Arc<std::sync::RwLock<Arc<str>>>,
}

/// Read the current account out of a registered factory, cloning the inner
/// `Arc<str>` and releasing the lock immediately. Falls back to the poisoned
/// inner value rather than panicking, so a poisoned lock never aborts a turn.
fn current_account_of(g: &GlobalFactory) -> Arc<str> {
    g.account
        .read()
        .map_or_else(|p| Arc::clone(&p.into_inner()), |a| Arc::clone(&a))
}

/// Register the daemon-wide [`ProviderFactory`] + default `account` so the agent
/// loop can rebuild a provider for a cross-provider routing pick.
///
/// Idempotent: only the first registration wins (later calls are a no-op),
/// mirroring the daemon's single-startup lifecycle. Wiring this from `main.rs`
/// after the factory is constructed turns cross-provider per-turn routing on;
/// leaving it unwired keeps the loop byte-identical (the rebuild path stays
/// dormant). Kept additive so it can be adopted without touching `LoopOptions`.
pub fn set_global(factory: Arc<ProviderFactory>, account: impl Into<Arc<str>>) {
    let _ = GLOBAL_FACTORY.set(GlobalFactory {
        factory,
        account: Arc::new(std::sync::RwLock::new(account.into())),
    });
}

/// Update the account the process-wide factory resolves credentials for.
///
/// Called from the `/account` switch handler so a subsequent cross-provider
/// rebuild uses the freshly-selected account's credentials rather than the
/// startup default. No-op when [`set_global`] was never called (the default),
/// keeping the agent loop byte-identical. The write lock is held only for the
/// assignment.
pub fn update_global_account(account: &str) {
    if let Some(g) = GLOBAL_FACTORY.get() {
        if let Ok(mut w) = g.account.write() {
            *w = Arc::from(account);
        }
    }
}

/// The account the process-wide factory currently resolves credentials for, or
/// `None` when no factory is registered. Read-only query, primarily for tests
/// and diagnostics.
#[must_use]
pub fn get_current_account() -> Option<Arc<str>> {
    GLOBAL_FACTORY.get().map(current_account_of)
}

/// The registered factory + account, or `None` when the daemon never called
/// [`set_global`] (the default ⇒ no cross-provider rebuild, loop unchanged).
#[must_use]
fn global() -> Option<GlobalFactory> {
    GLOBAL_FACTORY.get().cloned()
}

/// Build a provider for a cross-provider routing pick using the process-wide
/// factory registered via [`set_global`], if any.
///
/// Returns `None` — never panics — when no factory is registered, the
/// `provider_id` is unknown, or the credential is missing, so the agent loop
/// can fall back to the active provider for that turn. This is the single entry
/// point the loop calls; it owns the "is a factory reachable?" decision so the
/// call site stays a one-liner.
pub async fn build_provider_for(provider_id: &str, model: &str) -> Option<Arc<dyn Provider>> {
    let g = global()?;
    // Clone the current account out and drop the lock before the await.
    let account = current_account_of(&g);
    g.factory.build_provider_for(provider_id, model, &account).await
}

/// Intern a provider id into a process-static `&'static str`, leaking each
/// distinct id at most once. Bounds the leak to the finite catalog regardless
/// of how many provider rebuilds (account switches) occur.
fn intern_provider_name(id: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    static NAMES: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let map = NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&existing) = guard.get(id) {
        return existing;
    }
    let leaked: &'static str = Box::leak(id.to_string().into_boxed_str());
    guard.insert(id.to_string(), leaked);
    leaked
}

async fn render_base_url(
    template: &str,
    vault: &KeyVault,
    provider: &str,
    account: &str,
) -> Result<String, FactoryError> {
    if !template.contains('{') {
        return Ok(template.to_string());
    }
    let extras_key = format!("{account}/extras");
    let extras = vault
        .get(provider, &extras_key)
        .await
        .map_err(|e| FactoryError::from_vault(e, provider, &extras_key))?;
    let json: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(extras.expose()).map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
    let mut out = template.to_string();
    for (k, v) in json {
        if let Some(s) = v.as_str() {
            out = out.replace(&format!("{{{k}}}"), s);
        }
    }
    Ok(out)
}

fn openai_extra_headers(id: &str) -> Vec<(String, String)> {
    match id {
        "openrouter" => vec![
            ("HTTP-Referer".to_string(), "https://origin.local".to_string()),
            ("X-Title".to_string(), "origin".to_string()),
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trip_known_ids() {
        let cat = Catalog::builtin();
        for id in ["anthropic", "openai", "google", "ollama"] {
            let parsed = ProviderId::parse(id, &cat).expect(id);
            assert_eq!(parsed.as_str(), id);
        }
    }

    #[test]
    fn parse_aliases() {
        let cat = Catalog::builtin();
        assert_eq!(
            ProviderId::parse("open-ai", &cat)
                .expect("open-ai alias")
                .as_str(),
            "openai"
        );
        assert_eq!(
            ProviderId::parse("gemini", &cat).expect("gemini alias").as_str(),
            "google"
        );
        assert!(ProviderId::parse("totally-not-a-provider", &cat).is_none());
    }

    /// Task 6: the router helper is *available* and routes by phase without
    /// touching the default `build()` selection path.
    #[test]
    fn route_helper_selects_by_phase() {
        use origin_router::{ModelRef, Phase, Strategy};
        let cat = Catalog::builtin();
        let vault = KeyVault::in_memory();
        let factory = ProviderFactory::new(vault, cat);

        let architect = ModelRef::new("anthropic", "claude-opus-4");
        let editor = ModelRef::new("anthropic", "claude-sonnet-4-6");
        let strat = Strategy::ArchitectEditor {
            architect: architect.clone(),
            editor: editor.clone(),
        };

        assert_eq!(
            factory.route(strat.clone(), Phase::Plan, &[]),
            Some(architect)
        );
        assert_eq!(factory.route(strat, Phase::Edit, &[]), Some(editor));

        // QuotaFallback with an empty chain yields no model.
        assert_eq!(
            factory.route(Strategy::QuotaFallback { chain: vec![] }, Phase::Edit, &[]),
            None
        );
    }

    /// Cross-provider rebuild scaffold: a known id with a reachable credential
    /// builds a provider offline (the Anthropic ApiKey wire never touches the
    /// network in `build`); an unknown id yields `None` instead of erroring so
    /// the agent loop can fall back to the active provider.
    #[tokio::test]
    async fn build_provider_for_known_id_and_unknown_id() {
        let cat = Catalog::builtin();
        let vault = KeyVault::in_memory();
        // Seed a credential so the Anthropic ApiKey wire build succeeds offline.
        vault
            .set("anthropic", "default", origin_keyvault::Secret::new("sk-test".to_string()))
            .await
            .expect("seed credential");
        let factory = ProviderFactory::new(vault, cat);

        // Known id + present credential ⇒ Some(provider) with the right name.
        let built = factory
            .build_provider_for("anthropic", "claude-sonnet-4-6", "default")
            .await;
        assert!(built.is_some(), "known id with credential must build");
        assert_eq!(built.expect("some").name(), "anthropic");

        // A model id is informational only; it is not baked into the provider,
        // so an arbitrary model still builds the same provider.
        assert!(factory
            .build_provider_for("anthropic", "any-model-id", "default")
            .await
            .is_some());

        // Unknown provider id ⇒ None (no panic, caller falls back).
        assert!(factory
            .build_provider_for("totally-not-a-provider", "m", "default")
            .await
            .is_none());

        // Known id but NO credential for this account ⇒ None.
        assert!(factory
            .build_provider_for("anthropic", "m", "no-such-account")
            .await
            .is_none());
    }

    /// The process-wide registry is dormant until [`set_global`] is called: the
    /// free `build_provider_for` returns `None` when nothing is registered, which
    /// is exactly the byte-identical fall-back the agent loop relies on.
    #[tokio::test]
    async fn global_build_provider_for_is_none_when_unregistered() {
        // NB: `set_global` is process-wide + idempotent, so this test only
        // asserts the *unregistered* default (registering here would leak into
        // other tests in the same process).
        assert!(super::build_provider_for("anthropic", "m").await.is_none());
    }
}
