// SPDX-License-Identifier: Apache-2.0
//! Static catalog rows — one per supported provider.

use crate::catalog::{AuthScheme, Capabilities, OAuthSpec, ProviderEntry, WireFormat};
use std::borrow::Cow;

const FULL_CAPS: Capabilities = Capabilities {
    streaming: true,
    tools: true,
    prompt_cache: false,
    thinking: false,
};

const STREAM_ONLY: Capabilities = Capabilities {
    streaming: true,
    tools: false,
    prompt_cache: false,
    thinking: false,
};

const fn bearer() -> AuthScheme {
    AuthScheme::ApiKey {
        header: Cow::Borrowed("Authorization"),
        prefix: Cow::Borrowed("Bearer "),
    }
}

const fn xapikey() -> AuthScheme {
    AuthScheme::ApiKey {
        header: Cow::Borrowed("x-api-key"),
        prefix: Cow::Borrowed(""),
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn builtin_catalog() -> Vec<ProviderEntry> {
    vec![
        // ---- Native wire formats ----
        ProviderEntry {
            id: "anthropic".into(),
            display_name: "Anthropic (API key)".into(),
            wire: WireFormat::Anthropic,
            auth: xapikey(),
            base_url: "https://api.anthropic.com".into(),
            chat_path: "/v1/messages".into(),
            default_model: "claude-sonnet-4-6".into(),
            capabilities: Capabilities {
                prompt_cache: true,
                thinking: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "anthropic-oauth".into(),
            display_name: "Anthropic (Claude CLI OAuth)".into(),
            wire: WireFormat::Anthropic,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://claude.com/cai/oauth/authorize".into(),
                token_url: "https://platform.claude.com/v1/oauth/token".into(),
                client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(),
                scopes: Cow::Borrowed(&[
                    Cow::Borrowed("org:create_api_key"),
                    Cow::Borrowed("user:profile"),
                    Cow::Borrowed("user:inference"),
                    Cow::Borrowed("user:sessions:claude_code"),
                    Cow::Borrowed("user:mcp_servers"),
                    Cow::Borrowed("user:file_upload"),
                ]),
                redirect_uri: "https://platform.claude.com/oauth/code/callback".into(),
                pkce: true,
                device_flow: false,
            }),
            base_url: "https://api.anthropic.com".into(),
            chat_path: "/v1/messages".into(),
            default_model: "claude-sonnet-4-6".into(),
            capabilities: Capabilities {
                prompt_cache: true,
                thinking: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "google".into(),
            display_name: "Google (Gemini API key)".into(),
            wire: WireFormat::Gemini,
            auth: AuthScheme::ApiKey {
                header: Cow::Borrowed("x-goog-api-key"),
                prefix: Cow::Borrowed(""),
            },
            base_url: "https://generativelanguage.googleapis.com".into(),
            chat_path: "/v1beta/models".into(),
            default_model: "gemini-2.5-pro".into(),
            capabilities: Capabilities {
                prompt_cache: true,
                thinking: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "gemini-oauth".into(),
            display_name: "Gemini CLI OAuth".into(),
            wire: WireFormat::Gemini,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_url: "https://oauth2.googleapis.com/token".into(),
                client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com".into(),
                scopes: Cow::Borrowed(&[Cow::Borrowed(
                    "https://www.googleapis.com/auth/generative-language.retriever",
                )]),
                redirect_uri: "http://localhost:8085".into(),
                pkce: true,
                device_flow: false,
            }),
            base_url: "https://generativelanguage.googleapis.com".into(),
            chat_path: "/v1beta/models".into(),
            default_model: "gemini-2.5-pro".into(),
            capabilities: Capabilities {
                prompt_cache: true,
                thinking: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "bedrock".into(),
            display_name: "AWS Bedrock".into(),
            wire: WireFormat::Bedrock,
            auth: AuthScheme::SigV4 {
                service: Cow::Borrowed("bedrock"),
            },
            base_url: "https://bedrock-runtime".into(),
            chat_path: "/model".into(),
            default_model: "anthropic.claude-3-haiku-20240307-v1:0".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "ollama".into(),
            display_name: "Ollama (local)".into(),
            wire: WireFormat::Ollama,
            auth: AuthScheme::None,
            base_url: "http://localhost:11434".into(),
            chat_path: "/api/chat".into(),
            default_model: "llama3.2".into(),
            capabilities: STREAM_ONLY,
        },
        ProviderEntry {
            id: "github-copilot".into(),
            display_name: "GitHub Copilot".into(),
            wire: WireFormat::GitHubCopilot,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://github.com/login/device/code".into(),
                token_url: "https://github.com/login/oauth/access_token".into(),
                client_id: "Iv1.b507a08c87ecfe98".into(), // public Copilot client id
                scopes: Cow::Borrowed(&[Cow::Borrowed("read:user")]),
                redirect_uri: "".into(),
                pkce: false,
                device_flow: true,
            }),
            base_url: "https://api.individual.githubcopilot.com".into(),
            chat_path: "/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        // ---- OpenAI Chat-Completions compatible (29 providers) ----
        ProviderEntry {
            id: "openai".into(),
            display_name: "OpenAI".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.openai.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "openai-codex".into(),
            display_name: "OpenAI Codex (ChatGPT OAuth)".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::OAuth(OAuthSpec {
                authorize_url: "https://auth.openai.com/oauth/authorize".into(),
                token_url: "https://auth.openai.com/oauth/token".into(),
                client_id: "app_EMoamEEZ73f0CkXaXp7hrann".into(),
                scopes: Cow::Borrowed(&[
                    Cow::Borrowed("openid"),
                    Cow::Borrowed("profile"),
                    Cow::Borrowed("email"),
                ]),
                redirect_uri: "http://localhost:1455/auth/callback".into(),
                pkce: true,
                // Auth-code + PKCE loopback flow (note the /oauth/authorize
                // endpoint, localhost redirect_uri, and pkce above) — NOT a
                // device-code flow. `true` would route login to the device-code
                // branch, which POSTs a device grant to the auth-code endpoint
                // and fails. Contrast github-copilot, whose authorize_url is a
                // real /login/device/code endpoint.
                device_flow: false,
            }),
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            chat_path: "/responses".into(),
            default_model: "gpt-5-codex".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://openrouter.ai".into(),
            chat_path: "/api/v1/chat/completions".into(),
            default_model: "openrouter/auto".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "deepseek".into(),
            display_name: "DeepSeek".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.deepseek.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "deepseek-chat".into(),
            capabilities: Capabilities {
                thinking: true,
                prompt_cache: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "fireworks".into(),
            display_name: "Fireworks AI".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.fireworks.ai".into(),
            chat_path: "/inference/v1/chat/completions".into(),
            default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "together".into(),
            display_name: "Together AI".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.together.xyz".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "xai".into(),
            display_name: "xAI (Grok)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.x.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "grok-4".into(),
            capabilities: Capabilities {
                thinking: true,
                prompt_cache: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "mistral".into(),
            display_name: "Mistral AI".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.mistral.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "mistral-large-latest".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "moonshot".into(),
            display_name: "Moonshot AI (Kimi)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.moonshot.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "kimi-k2.5".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "minimax".into(),
            display_name: "MiniMax".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.minimax.io".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "abab6.5s-chat".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "stepfun".into(),
            display_name: "StepFun".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.stepfun.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "step-2-16k".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "synthetic".into(),
            display_name: "Synthetic".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.synthetic.new".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "synthetic-coder".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "venice".into(),
            display_name: "Venice AI".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.venice.ai".into(),
            chat_path: "/api/v1/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "arcee".into(),
            display_name: "Arcee AI".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://chat.arcee.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "arcee-spark".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "byteplus".into(),
            display_name: "BytePlus".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://ark.ap-southeast.bytepluses.com".into(),
            chat_path: "/api/v3/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "chutes".into(),
            display_name: "Chutes".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://llm.chutes.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "qwen".into(),
            display_name: "Qwen Cloud (DashScope)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://dashscope.aliyuncs.com".into(),
            chat_path: "/compatible-mode/v1/chat/completions".into(),
            default_model: "qwen-max".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "qianfan".into(),
            display_name: "Qianfan (Baidu)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://qianfan.baidubce.com".into(),
            chat_path: "/v2/chat/completions".into(),
            default_model: "ernie-4.5".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "volcengine".into(),
            display_name: "Volcano Engine".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://ark.cn-beijing.volces.com".into(),
            chat_path: "/api/v3/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "xiaomi".into(),
            display_name: "Xiaomi (MiMo)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.xiaomimimo.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "mimo-v2-flash".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "z-ai".into(),
            display_name: "Z.AI (Zhipu GLM)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.z.ai".into(),
            chat_path: "/api/paas/v4/chat/completions".into(),
            default_model: "glm-4.6".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "ms-foundry".into(),
            display_name: "Microsoft Foundry".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::ApiKey {
                header: Cow::Borrowed("api-key"),
                prefix: Cow::Borrowed(""),
            },
            base_url: "https://models.inference.ai.azure.com".into(),
            chat_path: "/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "litellm".into(),
            display_name: "LiteLLM Proxy".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "http://localhost:4000".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "vercel-ai".into(),
            display_name: "Vercel AI Gateway".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://gateway.ai.vercel.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "openai/gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "cloudflare".into(),
            display_name: "Cloudflare AI Gateway".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway}/compat".into(),
            chat_path: "/chat/completions".into(),
            default_model: "@cf/meta/llama-3.3-70b-instruct".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "kilo".into(),
            display_name: "Kilo Gateway".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.kilo.ai".into(),
            chat_path: "/api/gateway/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "opencode".into(),
            display_name: "OpenCode".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://opencode.ai".into(),
            chat_path: "/zen/go/v1/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "copilot-proxy".into(),
            display_name: "Copilot Proxy".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.copilotproxy.dev".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "gpt-4o".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "vllm".into(),
            display_name: "vLLM".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "http://localhost:8000".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "sglang".into(),
            display_name: "SGLang".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "http://localhost:30000".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "huggingface".into(),
            display_name: "Hugging Face Inference".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://router.huggingface.co".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "meta-llama/Llama-3.3-70B-Instruct".into(),
            capabilities: FULL_CAPS,
        },
        // ---- Providers added from OpenClaw production catalog ----
        ProviderEntry {
            id: "groq".into(),
            display_name: "Groq".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.groq.com".into(),
            chat_path: "/openai/v1/chat/completions".into(),
            default_model: "llama-3.3-70b-versatile".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "cerebras".into(),
            display_name: "Cerebras".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.cerebras.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "llama-3.3-70b".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "deepinfra".into(),
            display_name: "DeepInfra".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.deepinfra.com".into(),
            chat_path: "/v1/openai/chat/completions".into(),
            default_model: "meta-llama/Llama-3.3-70B-Instruct".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "nvidia".into(),
            display_name: "NVIDIA".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://integrate.api.nvidia.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "meta/llama-3.3-70b-instruct".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "tencent".into(),
            display_name: "Tencent Cloud (TokenHub)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://tokenhub.tencentmaas.com".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "deepseek-v3".into(),
            capabilities: Capabilities {
                prompt_cache: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "lmstudio".into(),
            display_name: "LM Studio (local)".into(),
            wire: WireFormat::OpenAIChat,
            auth: AuthScheme::None,
            base_url: "http://localhost:1234".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "default".into(),
            capabilities: FULL_CAPS,
        },
        ProviderEntry {
            id: "kimi".into(),
            display_name: "Kimi Code (subscription)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://api.moonshot.ai".into(),
            chat_path: "/v1/chat/completions".into(),
            default_model: "kimi-k2.5".into(),
            capabilities: Capabilities {
                thinking: true,
                ..FULL_CAPS
            },
        },
        ProviderEntry {
            id: "qwen-intl".into(),
            display_name: "Qwen Cloud (Global)".into(),
            wire: WireFormat::OpenAIChat,
            auth: bearer(),
            base_url: "https://dashscope-intl.aliyuncs.com".into(),
            chat_path: "/compatible-mode/v1/chat/completions".into(),
            default_model: "qwen-max".into(),
            capabilities: FULL_CAPS,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique() {
        let cat = builtin_catalog();
        let mut seen = HashSet::new();
        for e in &cat {
            assert!(seen.insert(e.id.clone()), "duplicate id: {}", e.id);
        }
        assert!(cat.len() >= 40, "expected >=40 providers, got {}", cat.len());
    }

    #[test]
    fn oauth_specs_well_formed() {
        for e in builtin_catalog() {
            if let AuthScheme::OAuth(spec) = &e.auth {
                assert!(!spec.token_url.is_empty(), "{}: empty token_url", e.id);
                assert!(!spec.client_id.is_empty(), "{}: empty client_id", e.id);
            }
        }
    }

    #[test]
    #[allow(clippy::panic)]
    fn base_urls_parse() {
        for e in builtin_catalog() {
            // {placeholder}s are templated later; strip for the parse check.
            let cleaned = e.base_url.replace("{account_id}", "x").replace("{gateway}", "x");
            url::Url::parse(&cleaned).unwrap_or_else(|_| panic!("bad base_url for {}: {}", e.id, e.base_url));
        }
    }
}
