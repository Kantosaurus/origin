// SPDX-License-Identifier: Apache-2.0
//! Per-backend quirk handling for the `OpenAI`-compatibility shim.
//!
//! Pure request/response massaging so that one client can talk to many subtly
//! different `OpenAI`-compatible backends without panicking on their quirks.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors produced while massaging shim payloads.
#[derive(Debug, Error)]
pub enum ShimError {
    /// A JSON value could not be parsed or had an unexpected shape.
    #[error("json error: {0}")]
    Json(String),
}

/// A known `OpenAI`-compatible backend flavor.
///
/// Used to select which request/response quirks to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Canonical `OpenAI` API.
    OpenAi,
    /// `vLLM` self-hosted `OpenAI`-compatible server.
    VLlm,
    /// Cerebras inference cloud.
    Cerebras,
    /// Groq inference cloud.
    Groq,
    /// Together AI.
    Together,
    /// Ollama local server.
    Ollama,
    /// Mistral La Plateforme.
    Mistral,
    /// `DeepSeek` platform.
    DeepSeek,
    /// Anything unrecognized; treated conservatively.
    Other,
}

impl Backend {
    /// Classifies a backend from its base URL host (and path for local servers).
    ///
    /// Matching is host-substring based and case-insensitive. Unknown hosts map
    /// to [`Backend::Other`]. Localhost on the common Ollama port maps to
    /// [`Backend::Ollama`].
    #[must_use]
    pub fn from_base_url(url: &str) -> Self {
        let lower = url.to_ascii_lowercase();
        // Order matters: check the more specific vendors before generic fallbacks.
        if lower.contains("cerebras.ai") || lower.contains("cerebras.net") {
            Self::Cerebras
        } else if lower.contains("groq.com") {
            Self::Groq
        } else if lower.contains("together.ai") || lower.contains("together.xyz") {
            Self::Together
        } else if lower.contains("mistral.ai") {
            Self::Mistral
        } else if lower.contains("deepseek.com") {
            Self::DeepSeek
        } else if lower.contains("11434") || lower.contains("ollama") {
            Self::Ollama
        } else if lower.contains("vllm") || lower.contains("8000") {
            Self::VLlm
        } else if lower.contains("openai.com") || lower.contains("api.openai") {
            Self::OpenAi
        } else {
            Self::Other
        }
    }
}

/// Standalone classifier mirroring [`Backend::from_base_url`].
///
/// Provided so callers can use a free function form.
#[must_use]
pub fn from_base_url(url: &str) -> Backend {
    Backend::from_base_url(url)
}

/// Mutates an `OpenAI`-shaped request body in place to satisfy backend quirks.
///
/// This is best-effort and never panics: unexpected shapes are simply left
/// alone. Examples of quirks handled:
/// - `vLLM` and Cerebras reject a top-level `store` field, so it is removed.
/// - Several non-OpenAI backends do not understand `parallel_tool_calls`, so it
///   is dropped for them.
pub fn apply_request_quirks(backend: Backend, body: &mut serde_json::Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    match backend {
        Backend::VLlm | Backend::Cerebras => {
            obj.remove("store");
            obj.remove("parallel_tool_calls");
        }
        Backend::Groq | Backend::Together | Backend::Ollama | Backend::DeepSeek => {
            obj.remove("parallel_tool_calls");
        }
        Backend::OpenAi | Backend::Mistral | Backend::Other => {}
    }
}

/// Maps a model identifier to the name a given backend expects.
///
/// Returns an owned alias when one is known for the backend, otherwise echoes
/// the input unchanged (identity). This keeps unknown models passing through
/// untouched.
#[must_use]
pub fn map_model_name(backend: Backend, model: &str) -> String {
    let alias: Option<&str> = match backend {
        Backend::Groq => match model {
            "llama-3.1-70b" => Some("llama-3.1-70b-versatile"),
            "llama-3.1-8b" => Some("llama-3.1-8b-instant"),
            _ => None,
        },
        Backend::Cerebras => match model {
            "llama-3.1-70b" => Some("llama3.1-70b"),
            "llama-3.1-8b" => Some("llama3.1-8b"),
            _ => None,
        },
        Backend::Ollama => match model {
            "llama-3.1-8b" => Some("llama3.1:8b"),
            _ => None,
        },
        Backend::OpenAi
        | Backend::VLlm
        | Backend::Together
        | Backend::Mistral
        | Backend::DeepSeek
        | Backend::Other => None,
    };
    alias.map_or_else(|| model.to_owned(), ToOwned::to_owned)
}

/// Redacts secrets from a diagnostic URL so it is safe to log.
///
/// Replaces the values of common credential query parameters (`api_key`, `key`,
/// `token`, `access_token`, `apikey`) and any inline `user:password@` userinfo
/// with `***`. The structure of the URL is otherwise preserved.
#[must_use]
pub fn redact_url_secrets(url: &str) -> String {
    // Split off any fragment so it is not mistaken for query content.
    let (head, fragment) = url.split_once('#').map_or((url, None), |(h, f)| (h, Some(f)));

    let (mut base, query) = head
        .split_once('?')
        .map_or_else(|| (head.to_owned(), None), |(b, q)| (b.to_owned(), Some(q)));

    base = redact_userinfo(&base);

    let mut out = base;
    if let Some(query) = query {
        out.push('?');
        out.push_str(&redact_query(query));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Returns whether `userinfo` (`user:pass@`) should be scrubbed and rebuilds it.
fn redact_userinfo(base: &str) -> String {
    let Some(scheme_end) = base.find("://") else {
        return base.to_owned();
    };
    let (scheme, rest) = base.split_at(scheme_end + 3);
    let Some(at) = rest.find('@') else {
        return base.to_owned();
    };
    let authority_and_path = &rest[at + 1..];
    let userinfo = &rest[..at];
    if let Some((user, _pass)) = userinfo.split_once(':') {
        format!("{scheme}{user}:***@{authority_and_path}")
    } else {
        format!("{scheme}***@{authority_and_path}")
    }
}

/// Redacts secret-bearing parameters within a query string.
fn redact_query(query: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for pair in query.split('&') {
        if let Some((name, _value)) = pair.split_once('=') {
            if is_secret_param(name) {
                parts.push(format!("{name}=***"));
            } else {
                parts.push(pair.to_owned());
            }
        } else {
            parts.push(pair.to_owned());
        }
    }
    parts.join("&")
}

/// Returns true when a query-parameter name names a credential.
fn is_secret_param(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "api_key" | "apikey" | "key" | "token" | "access_token" | "auth"
    )
}

/// Reports whether a completion was cut off because it hit a length limit.
///
/// Returns `true` for the `OpenAI`-family finish reasons that indicate truncation
/// (`length`, `max_tokens`). All other reasons (including `None`) are `false`.
#[must_use]
pub fn detect_truncation(finish_reason: Option<&str>) -> bool {
    matches!(finish_reason, Some("length" | "max_tokens"))
}

/// Extracts a tool call that a backend emitted as raw assistant text.
///
/// Some `OpenAI`-compatible backends do not populate the structured `tool_calls`
/// field and instead emit the call inline, either wrapped in a `<tool_call>`
/// XML-ish tag or inside a fenced json code block. The wrapped object is
/// expected to contain a `name` and an `arguments` field; `arguments` may itself
/// be a JSON object/array (re-serialized) or already a string.
///
/// Returns `Some((name, arguments_json))` on success, or `None` when no such
/// tool call can be found.
#[must_use]
pub fn parse_raw_toolcall_text(text: &str) -> Option<(String, String)> {
    let candidate = extract_tag_body(text)
        .or_else(|| extract_fenced_json(text))
        .or_else(|| find_json_object(text))?;

    let value: serde_json::Value = serde_json::from_str(candidate.trim()).ok()?;
    let obj = value.as_object()?;

    let name = obj.get("name")?.as_str()?.to_owned();
    if name.is_empty() {
        return None;
    }

    let arguments = match obj.get("arguments") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).ok()?,
        None => "{}".to_owned(),
    };

    Some((name, arguments))
}

/// Returns the body inside a `<tool_call>...</tool_call>` tag, if present.
fn extract_tag_body(text: &str) -> Option<String> {
    const OPENS: [&str; 2] = ["<tool_call>", "<tool_call "];
    for open in OPENS {
        if let Some(start) = text.find(open) {
            // Skip to the end of the opening tag (handles attributes).
            let after_open = &text[start + open.len() - 1..];
            let body_start = after_open.find('>')? + 1;
            let body = &after_open[body_start..];
            let end = body.find("</tool_call>")?;
            return Some(body[..end].to_owned());
        }
    }
    None
}

/// Returns the contents of the first fenced code block, if present.
fn extract_fenced_json(text: &str) -> Option<String> {
    let fence_start = text.find("```")?;
    let after = &text[fence_start + 3..];
    // Drop an optional language tag up to the first newline.
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let end = body.find("```")?;
    Some(body[..end].to_owned())
}

/// Returns the first balanced top-level JSON object substring, if any.
fn find_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=idx].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_base_url_classifies_hosts() {
        assert_eq!(from_base_url("https://api.openai.com/v1"), Backend::OpenAi);
        assert_eq!(from_base_url("https://api.cerebras.ai/v1"), Backend::Cerebras);
        assert_eq!(from_base_url("https://api.groq.com/openai/v1"), Backend::Groq);
        assert_eq!(from_base_url("http://localhost:8000/v1"), Backend::VLlm);
        assert_eq!(from_base_url("http://localhost:11434/v1"), Backend::Ollama);
        assert_eq!(from_base_url("https://example.invalid/v1"), Backend::Other);
    }

    #[test]
    fn apply_request_quirks_strips_store_for_vllm_only() {
        let mut vllm = json!({ "model": "m", "store": true, "messages": [] });
        apply_request_quirks(Backend::VLlm, &mut vllm);
        assert!(vllm.get("store").is_none());

        let mut openai = json!({ "model": "m", "store": true, "messages": [] });
        apply_request_quirks(Backend::OpenAi, &mut openai);
        assert_eq!(openai.get("store"), Some(&json!(true)));
    }

    #[test]
    fn apply_request_quirks_drops_parallel_tool_calls_and_never_panics() {
        let mut groq = json!({ "parallel_tool_calls": true, "n": 1 });
        apply_request_quirks(Backend::Groq, &mut groq);
        assert!(groq.get("parallel_tool_calls").is_none());

        // Non-object bodies are left untouched without panicking.
        let mut scalar = json!("not an object");
        apply_request_quirks(Backend::Cerebras, &mut scalar);
        assert_eq!(scalar, json!("not an object"));
    }

    #[test]
    fn redact_url_secrets_hides_api_key_and_userinfo() {
        let redacted = redact_url_secrets("https://user:pass@host.example/v1?api_key=sk-secret&model=x");
        assert!(redacted.contains("api_key=***"));
        assert!(!redacted.contains("sk-secret"));
        assert!(redacted.contains("user:***@"));
        assert!(!redacted.contains("pass@"));
        assert!(redacted.contains("model=x"));
    }

    #[test]
    fn redact_url_secrets_handles_token_and_no_secrets() {
        assert_eq!(
            redact_url_secrets("https://host/v1?token=abc&q=1"),
            "https://host/v1?token=***&q=1"
        );
        assert_eq!(redact_url_secrets("https://host/v1?q=1"), "https://host/v1?q=1");
    }

    #[test]
    fn detect_truncation_length_true_other_false() {
        assert!(detect_truncation(Some("length")));
        assert!(detect_truncation(Some("max_tokens")));
        assert!(!detect_truncation(Some("stop")));
        assert!(!detect_truncation(None));
    }

    #[test]
    fn parse_raw_toolcall_text_tag_form() {
        let text =
            "sure!\n<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"city\": \"NYC\"}}</tool_call>";
        let (name, args) = parse_raw_toolcall_text(text).unwrap();
        assert_eq!(name, "get_weather");
        let parsed: serde_json::Value = serde_json::from_str(&args).unwrap();
        assert_eq!(parsed, json!({ "city": "NYC" }));
    }

    #[test]
    fn parse_raw_toolcall_text_fenced_form_and_string_args() {
        let text = "```json\n{\"name\": \"search\", \"arguments\": \"{\\\"q\\\":\\\"rust\\\"}\"}\n```";
        let (name, args) = parse_raw_toolcall_text(text).unwrap();
        assert_eq!(name, "search");
        assert_eq!(args, "{\"q\":\"rust\"}");
    }

    #[test]
    fn parse_raw_toolcall_text_absent_returns_none() {
        assert!(parse_raw_toolcall_text("just some prose, no tool call").is_none());
        assert!(parse_raw_toolcall_text("<tool_call>not json</tool_call>").is_none());
    }

    #[test]
    fn map_model_name_aliases_and_identity() {
        assert_eq!(
            map_model_name(Backend::Groq, "llama-3.1-70b"),
            "llama-3.1-70b-versatile"
        );
        assert_eq!(map_model_name(Backend::Cerebras, "llama-3.1-8b"), "llama3.1-8b");
        // Identity on unknown model / backend without an alias table.
        assert_eq!(map_model_name(Backend::Groq, "gpt-4o"), "gpt-4o");
        assert_eq!(map_model_name(Backend::OpenAi, "gpt-4o"), "gpt-4o");
    }

    #[test]
    fn shim_error_displays_json_message() {
        let err = ShimError::Json("bad".to_owned());
        assert_eq!(err.to_string(), "json error: bad");
    }
}
