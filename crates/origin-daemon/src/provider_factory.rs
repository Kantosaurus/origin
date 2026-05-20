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
}

impl core::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProviderFactory")
            .field("vault", &self.vault)
            .field("cas", &self.cas.as_ref().map(|_| "<cas>"))
            .field("catalog_entries", &self.catalog.entries().len())
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
        }
    }

    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }

    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
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
        // Leak the id to obtain a &'static str for Provider::name(). One leak per
        // provider construction is acceptable for a daemon's lifetime; the
        // alternative (changing the trait signature) is a much bigger refactor.
        #[allow(clippy::box_collection)]
        let name: &'static str = Box::leak(entry.id.to_string().into_boxed_str());
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
                Ok(Arc::new(OpenAiCompat::new(cfg)))
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
                Ok(Arc::new(origin_provider_bedrock::Bedrock::new(
                    creds.endpoint(),
                    creds.region.clone(),
                    creds.model_id(),
                    creds.access.clone(),
                    creds.secret,
                )))
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
}
