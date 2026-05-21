//! Connectivity probe used by `origin init` after credentials are captured.
//!
//! The probe doubles as both an auth-works check and a model-list discovery:
//! each supported [`WireFormat`] has a well-known "list models" endpoint that
//! returns the catalog of models the credential is authorized to call. A
//! 200 means auth works AND we can populate the model picker; 401/403 means
//! the credential is wrong; everything else means the provider is unreachable
//! (network down, base_url wrong, server 5xx).
//!
//! Encapsulating it behind [`ConnectivityProbe`] keeps `init.rs` testable —
//! tests inject a [`MockProbe`] returning a fixed outcome and model list,
//! avoiding both real HTTP and the `reqwest` + `tokio::net` stack.

use async_trait::async_trait;
use origin_keyvault::KeyVault;
use origin_provider::catalog::{AuthScheme, ProviderEntry, WireFormat};
use std::time::Duration;

/// What happened during the probe. The variant tells `init.rs` how to react:
/// `Ok` → continue, `AuthFailed` → offer to retry credentials, `Unreachable`
/// → warn and ask to continue anyway, `Skipped` → no test performed (silent).
#[derive(Debug, Clone)]
pub enum ProbeOutcome {
    /// 2xx response from the models endpoint. Credentials work.
    Ok,
    /// 401 / 403 from the models endpoint. The credential is rejected.
    AuthFailed { status: u16, detail: String },
    /// Network error, DNS failure, timeout, 5xx — anything that's not
    /// definitively an auth problem.
    Unreachable { detail: String },
    /// No probe was attempted (wire format has no probe path, or auth
    /// scheme requires bespoke signing we don't yet do).
    Skipped { reason: String },
}

impl ProbeOutcome {
    /// `true` for `Ok` and `Skipped` — both should let the flow continue
    /// without offering a retry loop.
    #[must_use]
    pub const fn is_passing(&self) -> bool {
        matches!(self, Self::Ok | Self::Skipped { .. })
    }
}

/// Probe result: outcome plus any model ids the endpoint advertised. The
/// list is `Vec::new()` for skipped probes, network errors, parse failures,
/// or providers whose endpoint shape we don't recognize — callers should
/// always fall back to `entry.default_model` when this is empty.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub outcome: ProbeOutcome,
    pub models: Vec<String>,
}

/// Object-safe probe interface. `init.rs` takes `&dyn ConnectivityProbe` so
/// tests can substitute [`MockProbe`] without dragging in tokio HTTP machinery.
#[async_trait]
pub trait ConnectivityProbe: Send + Sync {
    async fn probe(&self, entry: &ProviderEntry, vault: &KeyVault, account: &str) -> ProbeResult;
}

/// Production probe — issues a real HTTP GET to the wire-appropriate models
/// endpoint, parses the response, and classifies the status.
#[derive(Debug, Default)]
pub struct LiveProbe {
    client: reqwest::Client,
}

impl LiveProbe {
    /// Build a probe with a sane default timeout (10s) so a wedged provider
    /// can't hang onboarding indefinitely.
    #[must_use]
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { client }
    }
}

#[async_trait]
impl ConnectivityProbe for LiveProbe {
    async fn probe(&self, entry: &ProviderEntry, vault: &KeyVault, account: &str) -> ProbeResult {
        // Step 1: derive the models endpoint for this wire. None means we
        // don't know how to probe — surface `Skipped` so the user sees that
        // we're not silently faking success.
        let Some(path) = models_endpoint(entry) else {
            return ProbeResult {
                outcome: ProbeOutcome::Skipped {
                    reason: format!("no probe defined for wire {:?}", entry.wire),
                },
                models: Vec::new(),
            };
        };
        let url = format!("{}{}", entry.base_url, path);

        let mut req = self.client.get(&url);

        // Anthropic v1 endpoints all require the version header, including
        // /v1/models. Without it the endpoint returns 400.
        if matches!(entry.wire, WireFormat::Anthropic) {
            req = req.header("anthropic-version", "2023-06-01");
        }

        // Step 2: attach the credential. Returns `Skipped` for auth schemes
        // we can't currently exercise (SigV4 requires AWS signing; Custom is
        // out-of-band by definition).
        match &entry.auth {
            AuthScheme::None => {}
            AuthScheme::ApiKey { header, prefix } => {
                let key = match fetch_api_key(vault, &entry.id, account).await {
                    Ok(k) => k,
                    Err(detail) => {
                        return ProbeResult {
                            outcome: ProbeOutcome::Unreachable { detail },
                            models: Vec::new(),
                        };
                    }
                };
                // Gemini's `x-goog-api-key` works as a header but the
                // documented convention is the `?key=` query param. Both
                // work; we use the header form for symmetry with other
                // ApiKey providers and to keep the URL out of logs.
                req = req.header(header.as_ref(), format!("{prefix}{key}"));
            }
            AuthScheme::OAuth(_) => {
                let access = match fetch_oauth_access(vault, &entry.id, account).await {
                    Ok(t) => t,
                    Err(detail) => {
                        return ProbeResult {
                            outcome: ProbeOutcome::Unreachable { detail },
                            models: Vec::new(),
                        };
                    }
                };
                // Most OAuth-protected providers accept `Authorization: Bearer …`.
                // Anthropic OAuth still uses Bearer at the auth header level
                // (the `x-api-key` form is only for the API-key flow), so
                // this single branch covers every OAuth wire we handle.
                req = req.header("Authorization", format!("Bearer {access}"));
            }
            AuthScheme::SigV4 { service } => {
                return ProbeResult {
                    outcome: ProbeOutcome::Skipped {
                        reason: format!("SigV4 ({service}) probing not implemented yet"),
                    },
                    models: Vec::new(),
                };
            }
            AuthScheme::Custom => {
                return ProbeResult {
                    outcome: ProbeOutcome::Skipped {
                        reason: "custom auth schemes are tested out-of-band".into(),
                    },
                    models: Vec::new(),
                };
            }
        }

        // Step 3: send + classify.
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return ProbeResult {
                    outcome: ProbeOutcome::Unreachable { detail: e.to_string() },
                    models: Vec::new(),
                };
            }
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if (200..300).contains(&status) {
            let models = parse_models(entry.wire, &body);
            ProbeResult {
                outcome: ProbeOutcome::Ok,
                models,
            }
        } else if status == 401 || status == 403 {
            ProbeResult {
                outcome: ProbeOutcome::AuthFailed {
                    status,
                    detail: truncate(&body, 200),
                },
                models: Vec::new(),
            }
        } else {
            ProbeResult {
                outcome: ProbeOutcome::Unreachable {
                    detail: format!("HTTP {status}: {}", truncate(&body, 200)),
                },
                models: Vec::new(),
            }
        }
    }
}

/// Wire-specific GET path that returns the list of available models.
/// `None` means probing is not supported for that wire (Bedrock needs SigV4
/// signing; GitHubCopilot's models endpoint requires a separate token
/// exchange that's beyond v1 scope).
fn models_endpoint(entry: &ProviderEntry) -> Option<String> {
    match entry.wire {
        WireFormat::OpenAIChat => Some(openai_models_path(&entry.chat_path)),
        WireFormat::Anthropic => Some("/v1/models".to_string()),
        WireFormat::Gemini => Some("/v1beta/models".to_string()),
        WireFormat::Ollama => Some("/api/tags".to_string()),
        WireFormat::Bedrock | WireFormat::GitHubCopilot => None,
    }
}

/// Derive an OpenAI-compatible models path from a chat path. Each OpenAI-
/// compatible gateway puts its API under a different prefix
/// (`/v1`, `/api/v1`, `/inference/v1`, …), but every one of them puts
/// `models` next to `chat/completions`, so the derivation just swaps the
/// last two segments.
///
/// Falls back to `/v1/models` for catalog entries whose `chat_path` doesn't
/// follow the `chat/completions` convention (e.g. OpenAI Codex `/responses`).
fn openai_models_path(chat_path: &str) -> String {
    if let Some(stem) = chat_path.strip_suffix("/chat/completions") {
        return format!("{stem}/models");
    }
    if let Some(idx) = chat_path.rfind('/') {
        if idx > 0 {
            return format!("{}/models", &chat_path[..idx]);
        }
    }
    "/v1/models".to_string()
}

/// Best-effort model-list parse keyed on `WireFormat`. Returns an empty
/// vector if the body doesn't match the expected shape; the caller falls
/// back to the catalog default in that case.
fn parse_models(wire: WireFormat, body: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    match wire {
        WireFormat::OpenAIChat | WireFormat::Anthropic => v
            .get("data")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|x| x.as_str().map(String::from)))
                    .collect()
            })
            .unwrap_or_default(),
        WireFormat::Gemini => v
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        m.get("name").and_then(|x| {
                            x.as_str()
                                .map(|s| s.strip_prefix("models/").unwrap_or(s).to_string())
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        WireFormat::Ollama => v
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        m.get("name")
                            .and_then(|x| x.as_str())
                            .map(|s| {
                                // Ollama returns `llama3.2:latest`; the
                                // tag prefix before `:` is what users type.
                                s.split(':').next().unwrap_or(s).to_string()
                            })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        WireFormat::Bedrock | WireFormat::GitHubCopilot => Vec::new(),
    }
}

async fn fetch_api_key(vault: &KeyVault, provider: &str, account: &str) -> Result<String, String> {
    vault
        .get(provider, account)
        .await
        .map(|s| s.expose().to_string())
        .map_err(|e| format!("vault get {provider}:{account}: {e}"))
}

/// Read the OAuth token blob persisted by `keyring_login::run` (or by the
/// daemon's `OAuthClient::persist`) and return the `access` token.
async fn fetch_oauth_access(vault: &KeyVault, provider: &str, account: &str) -> Result<String, String> {
    let key = format!("{account}/oauth");
    let blob = vault
        .get(provider, &key)
        .await
        .map(|s| s.expose().to_string())
        .map_err(|e| format!("vault get {provider}:{key}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&blob).map_err(|e| format!("parse token json: {e}"))?;
    v.get("access")
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .ok_or_else(|| "token blob missing `access` field".into())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

// ---------------------------------------------------------------------------
// Test stubs
// ---------------------------------------------------------------------------

/// Deterministic stub used by `init.rs` tests. Constructed with a fixed
/// outcome and (optionally) a canned model list; replays the same response
/// for every call.
#[derive(Debug, Clone)]
pub struct MockProbe {
    pub outcome: ProbeOutcome,
    pub models: Vec<String>,
}

impl MockProbe {
    #[must_use]
    pub fn ok_with_models(models: Vec<String>) -> Self {
        Self {
            outcome: ProbeOutcome::Ok,
            models,
        }
    }

    #[must_use]
    pub fn ok_no_models() -> Self {
        Self::ok_with_models(Vec::new())
    }

    #[must_use]
    pub fn auth_failed() -> Self {
        Self {
            outcome: ProbeOutcome::AuthFailed {
                status: 401,
                detail: "unauthorized".into(),
            },
            models: Vec::new(),
        }
    }
}

#[async_trait]
impl ConnectivityProbe for MockProbe {
    async fn probe(&self, _entry: &ProviderEntry, _vault: &KeyVault, _account: &str) -> ProbeResult {
        ProbeResult {
            outcome: self.outcome.clone(),
            models: self.models.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_path_derivation() {
        assert_eq!(openai_models_path("/v1/chat/completions"), "/v1/models");
        assert_eq!(
            openai_models_path("/api/v1/chat/completions"),
            "/api/v1/models"
        );
        assert_eq!(
            openai_models_path("/inference/v1/chat/completions"),
            "/inference/v1/models"
        );
        // Codex-style: no `/chat/completions` suffix → fallback.
        assert_eq!(openai_models_path("/responses"), "/v1/models");
    }

    #[test]
    fn parse_openai_models() {
        let body = r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#;
        assert_eq!(
            parse_models(WireFormat::OpenAIChat, body),
            vec!["gpt-4o", "gpt-4o-mini"]
        );
    }

    #[test]
    fn parse_anthropic_models() {
        let body = r#"{"data":[{"id":"claude-sonnet-4-6","type":"model"}],"has_more":false}"#;
        assert_eq!(
            parse_models(WireFormat::Anthropic, body),
            vec!["claude-sonnet-4-6"]
        );
    }

    #[test]
    fn parse_gemini_models_strips_prefix() {
        let body = r#"{"models":[{"name":"models/gemini-2.5-pro"},{"name":"models/gemini-2.5-flash"}]}"#;
        assert_eq!(
            parse_models(WireFormat::Gemini, body),
            vec!["gemini-2.5-pro", "gemini-2.5-flash"]
        );
    }

    #[test]
    fn parse_ollama_models_strips_tag() {
        let body = r#"{"models":[{"name":"llama3.2:latest"},{"name":"qwen2.5:7b"}]}"#;
        assert_eq!(
            parse_models(WireFormat::Ollama, body),
            vec!["llama3.2", "qwen2.5"]
        );
    }

    #[test]
    fn parse_malformed_returns_empty() {
        assert!(parse_models(WireFormat::OpenAIChat, "not json").is_empty());
        assert!(parse_models(WireFormat::OpenAIChat, r#"{"unexpected":"shape"}"#).is_empty());
    }

    #[test]
    fn outcome_is_passing_covers_skipped_and_ok() {
        assert!(ProbeOutcome::Ok.is_passing());
        assert!(ProbeOutcome::Skipped {
            reason: "x".into(),
        }
        .is_passing());
        assert!(!ProbeOutcome::AuthFailed {
            status: 401,
            detail: "x".into(),
        }
        .is_passing());
        assert!(!ProbeOutcome::Unreachable { detail: "x".into() }.is_passing());
    }
}
