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

use std::sync::Arc;

use origin_keyvault::{Error as VaultError, KeyVault};
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
#[cfg(feature = "bedrock")]
use origin_provider_bedrock::Bedrock;
#[cfg(feature = "gemini")]
use origin_provider_gemini::Gemini;
#[cfg(feature = "github-models")]
use origin_provider_github::GitHubModels;
#[cfg(feature = "ollama")]
use origin_provider_ollama::Ollama;
#[cfg(feature = "openai")]
use origin_provider_openai::OpenAi;
#[cfg(feature = "openrouter")]
use origin_provider_openrouter::OpenRouter;
use thiserror::Error;

/// Identifier for every provider the daemon knows how to build.
///
/// Mirrors the on-wire `provider` string used by
/// [`crate::protocol::ClientMessage::SwitchAccount`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderId {
    Anthropic,
    OpenAi,
    Gemini,
    Ollama,
    #[cfg(feature = "openrouter")]
    OpenRouter,
    #[cfg(feature = "bedrock")]
    Bedrock,
    #[cfg(feature = "github-models")]
    GitHubModels,
}

impl ProviderId {
    /// Parses a lowercase provider identifier (matches [`Self::as_str`]).
    /// Accepts a handful of common aliases (`"openai"`, `"open-ai"`, …).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai" | "open-ai" | "open_ai" => Some(Self::OpenAi),
            "gemini" | "google" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
            #[cfg(feature = "openrouter")]
            "openrouter" | "open-router" => Some(Self::OpenRouter),
            #[cfg(feature = "bedrock")]
            "bedrock" | "aws-bedrock" => Some(Self::Bedrock),
            #[cfg(feature = "github-models")]
            "github" | "github-models" => Some(Self::GitHubModels),
            _ => None,
        }
    }

    /// Stable lowercase string form. Round-trips through [`Self::parse`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Ollama => "ollama",
            #[cfg(feature = "openrouter")]
            Self::OpenRouter => "openrouter",
            #[cfg(feature = "bedrock")]
            Self::Bedrock => "bedrock",
            #[cfg(feature = "github-models")]
            Self::GitHubModels => "github-models",
        }
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
#[cfg(feature = "bedrock")]
#[derive(Debug, serde::Deserialize)]
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

/// Builds providers on demand from a backing [`KeyVault`].
#[derive(Clone)]
pub struct ProviderFactory {
    vault: KeyVault,
    cas: Option<Arc<origin_cas::Store>>,
}

impl core::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProviderFactory")
            .field("vault", &self.vault)
            .field("cas", &self.cas.as_ref().map(|_| "<cas>"))
            .finish()
    }
}

impl ProviderFactory {
    /// Wraps an existing vault. The factory clones the handle (cheap —
    /// `KeyVault` is itself `Arc`-backed) for every `build` call so the
    /// caller can keep the original vault for direct lookups.
    #[must_use]
    pub const fn new(vault: KeyVault) -> Self {
        Self { vault, cas: None }
    }

    /// Attaches a CAS handle so providers that re-inflate `ToolResult`
    /// blocks from CAS bytes (currently only Anthropic) pick it up. The
    /// factory ignores the CAS for every other provider so this is safe
    /// to call unconditionally from the daemon bootstrap.
    #[must_use]
    pub fn with_cas(mut self, cas: Arc<origin_cas::Store>) -> Self {
        self.cas = Some(cas);
        self
    }

    /// Construct the provider matching `id` using the credential stored
    /// under (`id.as_str()`, `account`).
    ///
    /// # Errors
    /// Returns [`FactoryError::MissingCredential`] when the vault has no
    /// entry for the (provider, account) pair, [`FactoryError::Vault`] for
    /// other backend failures, and [`FactoryError::CredentialParse`] when
    /// a structured blob (Bedrock) cannot be decoded.
    pub async fn build(&self, id: ProviderId, account: &str) -> Result<Arc<dyn Provider>, FactoryError> {
        match id {
            ProviderId::Anthropic => {
                let secret = self
                    .vault
                    .get("anthropic", account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, "anthropic", account))?;
                let mut provider = Anthropic::new(secret.expose().clone());
                if let Some(cas) = self.cas.clone() {
                    provider = provider.with_cas(cas);
                }
                Ok(Arc::new(provider))
            }
            #[cfg(feature = "openai")]
            ProviderId::OpenAi => {
                let secret = self
                    .vault
                    .get("openai", account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, "openai", account))?;
                Ok(Arc::new(OpenAi::new(secret.expose().clone())))
            }
            #[cfg(not(feature = "openai"))]
            ProviderId::OpenAi => Err(FactoryError::UnknownProvider("openai".into())),
            #[cfg(feature = "gemini")]
            ProviderId::Gemini => {
                let secret = self
                    .vault
                    .get("gemini", account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, "gemini", account))?;
                Ok(Arc::new(Gemini::new(secret.expose().clone())))
            }
            #[cfg(not(feature = "gemini"))]
            ProviderId::Gemini => Err(FactoryError::UnknownProvider("gemini".into())),
            #[cfg(feature = "ollama")]
            ProviderId::Ollama => {
                // Ollama has no credential; the account selector is reserved
                // for a future base-url override.
                let _ = account;
                Ok(Arc::new(Ollama::new()))
            }
            #[cfg(not(feature = "ollama"))]
            ProviderId::Ollama => Err(FactoryError::UnknownProvider("ollama".into())),
            #[cfg(feature = "openrouter")]
            ProviderId::OpenRouter => {
                let secret = self
                    .vault
                    .get("openrouter", account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, "openrouter", account))?;
                Ok(Arc::new(OpenRouter::new(secret.expose().clone())))
            }
            #[cfg(feature = "bedrock")]
            ProviderId::Bedrock => {
                let secret = self
                    .vault
                    .get("bedrock", account)
                    .await
                    .map_err(|e| FactoryError::from_vault(e, "bedrock", account))?;
                let creds: BedrockCreds = serde_json::from_str(secret.expose())
                    .map_err(|e| FactoryError::CredentialParse(e.to_string()))?;
                let endpoint = creds.endpoint();
                let model_id = creds.model_id();
                Ok(Arc::new(Bedrock::new(
                    endpoint,
                    creds.region.clone(),
                    model_id,
                    creds.access.clone(),
                    creds.secret,
                )))
            }
            #[cfg(feature = "github-models")]
            ProviderId::GitHubModels => {
                // GitHub Models reads its OAuth token from the vault on every
                // request; we only need to hand it the vault handle + account.
                Ok(Arc::new(GitHubModels::new(self.vault.clone(), account)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trip_known_ids() {
        for id in [
            ProviderId::Anthropic,
            ProviderId::OpenAi,
            ProviderId::Gemini,
            ProviderId::Ollama,
        ] {
            assert_eq!(ProviderId::parse(id.as_str()), Some(id));
        }
    }

    #[test]
    fn parse_aliases() {
        assert_eq!(ProviderId::parse("open-ai"), Some(ProviderId::OpenAi));
        assert_eq!(ProviderId::parse("google"), Some(ProviderId::Gemini));
        assert!(ProviderId::parse("totally-not-a-provider").is_none());
    }
}
