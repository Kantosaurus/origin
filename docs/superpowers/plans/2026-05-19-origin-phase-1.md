# `origin` Phase 1 — First End-to-End Turn — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A working harness that takes a prompt in the CLI, calls Anthropic with the provider trait, dispatches Read/Glob/Grep/Edit/Bash tools through permission gating, persists the session to SQLite, and shows the final answer in a Ratatui baseline TUI.

**Architecture:** Build the `origin-provider` trait + Anthropic non-streaming implementation, the tool registry with five builtin tools, a tier-based permission engine with an interactive prompter, the agent loop in `origin-daemon` (no streaming yet — that arrives in P2), session persistence via blob-inline messages, and a Ratatui-based TUI client. End state: dogfoodable harness, Anthropic only.

**Tech Stack:** Rust 1.83 (MSRV pin), Tokio, `reqwest` + `rustls`, `wiremock` for provider tests, `inventory` for tool registry, `glob`, `grep-searcher` (ripgrep core), Ratatui + crossterm.

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` (spec) and the patterns established in Phase 0 (`p0-complete` tag at `7f5dea7`).

**Phase 1 spec mechanism citations:** N4.5 (KeyVault is *not* in P1 — we use raw env var for API key in P1; KeyVault arrives in P8), N5.1 (compile-time tool registry — basic form in P1, full proc-macro polish refined later), N9.1 (tier macros — basic form), N10.2 (audience-typed errors — basic form).

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**
1. Write the failing test.
2. Run it — confirm the expected failure mode.
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit.

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / daemon | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |

**Patterns inherited from Phase 0:**
- All shared types derive `Archive + Serialize + Deserialize` from rkyv 0.7 with `#[archive(check_bytes)]` when sent over IPC or stored.
- `[lints] workspace = true` in every crate `Cargo.toml`.
- Workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- Tests use `.expect("…")` with meaningful messages — never `#[allow(clippy::unwrap_used)]`.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- For types with a `new()` that has side effects (e.g., ULID generation), do NOT implement `Default` — apply `#[allow(clippy::new_without_default)]` to `new()` instead.
- Add `Hash` to wrapper types pre-emptively so they're usable as map keys.
- Custom error enums via `thiserror`; document `# Errors` + `# Panics` on `pub fn`s.
- Targeted `#[allow(clippy::…)]` annotations carry an inline comment justifying them; never blanket-suppress.
- MSRV-pin reflex: when a new crate's `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offending dep with `cargo update -p <crate> --precise <ver>`. Record pins in `Cargo.lock`. Reference: `[[project-msrv-dep-pinning]]` memory.
- WAL+migrations: SQLite `PRAGMA journal_mode = WAL` must run OUTSIDE refinery migrations (refinery wraps each migration in a tx and SQLite refuses WAL inside a tx). Apply WAL/synchronous PRAGMAs in `Store::open` before running migrations.

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit**.

---

## File map for Phase 1

| New file | Responsibility |
|---|---|
| `crates/origin-provider/Cargo.toml` | manifest |
| `crates/origin-provider/src/lib.rs` | `Provider` trait + `ChatRequest`/`ChatResponse`/`ToolSchema`/`Usage`/`ProviderError` |
| `crates/origin-provider/tests/trait.rs` | trait sanity test |
| `crates/origin-provider-anthropic/Cargo.toml` | manifest |
| `crates/origin-provider-anthropic/src/lib.rs` | `Anthropic` struct + `Provider` impl |
| `crates/origin-provider-anthropic/src/wire.rs` | wire JSON types (private) |
| `crates/origin-provider-anthropic/tests/messages.rs` | wiremock-driven test of `/v1/messages` |
| `crates/origin-tools/Cargo.toml` | manifest |
| `crates/origin-tools/src/lib.rs` | `Tier`/`Urgency`/`SideEffects`/`ToolMeta`/registry exports |
| `crates/origin-tools/src/registry.rs` | inventory-backed registry + dispatch |
| `crates/origin-tools/src/macros.rs` | `origin_tool!` macro |
| `crates/origin-tools/src/builtins/mod.rs` | re-exports |
| `crates/origin-tools/src/builtins/read.rs` | Read tool |
| `crates/origin-tools/src/builtins/glob_tool.rs` | Glob tool (named `glob_tool` to avoid the crate name clash with the `glob` crate) |
| `crates/origin-tools/src/builtins/grep_tool.rs` | Grep tool (same reason) |
| `crates/origin-tools/src/builtins/edit.rs` | Edit tool |
| `crates/origin-tools/src/builtins/bash.rs` | Bash tool |
| `crates/origin-tools/tests/registry.rs` | registry registration test |
| `crates/origin-tools/tests/read.rs` | Read tests |
| `crates/origin-tools/tests/glob.rs` | Glob tests |
| `crates/origin-tools/tests/grep.rs` | Grep tests |
| `crates/origin-tools/tests/edit.rs` | Edit tests |
| `crates/origin-tools/tests/bash.rs` | Bash tests |
| `crates/origin-permission/Cargo.toml` | manifest |
| `crates/origin-permission/src/lib.rs` | `Decision`/`Outcome`/`Prompter`/`check()` |
| `crates/origin-permission/src/prompt.rs` | `Prompter` trait + `AlwaysAllow`/`AlwaysDeny` test prompters |
| `crates/origin-permission/tests/check.rs` | tier-dispatch tests |
| `crates/origin-daemon/src/session.rs` | `Session` struct + `prompt()` |
| `crates/origin-daemon/src/agent.rs` | agent loop (tool dispatch + permission + cap) |
| `crates/origin-daemon/src/session_store.rs` | SQLite persistence wrapper |
| `crates/origin-daemon/src/protocol.rs` | IPC request/response shapes (Prompt / Reply) |
| `crates/origin-daemon/tests/loop_e2e.rs` | agent-loop integration test (fake provider + fake tool) |
| `crates/origin-cli/src/tui.rs` | Ratatui state + render loop |
| `crates/origin-cli/src/input.rs` | event-loop + key handling |
| `crates/origin-cli/src/screen.rs` | layout widgets |
| `Modified files` | `crates/origin-daemon/{Cargo.toml,src/main.rs}`, `crates/origin-cli/{Cargo.toml,src/main.rs}` |

Decomposition: tool **registry** is one crate; each builtin tool lives in its own file; **permission** is its own crate; **provider trait** is its own crate; **Anthropic** is its own crate. This keeps each file small (<200 LOC target) and lets P8 add OpenAI/Gemini/etc. by adding crates without touching shared code.

---

## Task P1.1 — `origin-provider` trait

**Files:**
- Create: `crates/origin-provider/Cargo.toml`
- Create: `crates/origin-provider/src/lib.rs`
- Create: `crates/origin-provider/tests/trait.rs`

- [ ] **Step 1: Write `crates/origin-provider/Cargo.toml`**

```toml
[package]
name = "origin-provider"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
async-trait = "0.1"
thiserror = "1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Write failing test at `crates/origin-provider/tests/trait.rs`**

```rust
use origin_core::types::{Block, Message, Role};
use origin_provider::{
    ChatRequest, ChatResponse, Provider, ProviderError, ToolSchema, Usage,
};

struct FakeProv;

#[async_trait::async_trait]
impl Provider for FakeProv {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("hi")),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn fake_provider_round_trips() {
    let p = FakeProv;
    assert_eq!(p.name(), "fake");
    let req = ChatRequest {
        system: String::new(),
        messages: vec![Message::new(Role::User).with_block(Block::text("hello"))],
        model: "fake-1".to_string(),
        tools: Vec::<ToolSchema>::new(),
    };
    let resp = p.chat(req).await.expect("fake provider should not fail");
    assert_eq!(resp.assistant.role, Role::Assistant);
    assert_eq!(resp.assistant.blocks.len(), 1);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-provider`
Expected: compile error — `origin_provider` and its types do not exist.

- [ ] **Step 4: Implement `crates/origin-provider/src/lib.rs`**

```rust
//! Provider trait + canonical request/response/usage/error types.
//!
//! Each `Provider` implementation translates the canonical types to its wire
//! format. Multi-provider support is achieved by per-provider crates that
//! `impl Provider for …`.

use origin_core::types::Message;
use thiserror::Error;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the tool input shape.
    pub input_schema_json: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub model: String,
    pub tools: Vec<ToolSchema>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub assistant: Message,
    pub usage: Usage,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("api: {0}")]
    Api(String),
    #[error("auth")]
    Auth,
    #[error("rate limit; retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u32 },
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    /// Send a single non-streaming chat request.
    ///
    /// # Errors
    /// Returns `ProviderError` for transport, API, auth, or rate-limit failures.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p origin-provider`
Expected: PASS, 1 test.

- [ ] **Step 6: Verification gate**

Run, all must exit 0:
- `cargo test -p origin-provider`
- `cargo clippy -p origin-provider --all-targets -- -D warnings`
- `cargo fmt --check`

If clippy raises pedantic warnings on the public struct fields being `pub` (e.g., `missing_const_for_fn` on `Usage::default()`), do **not** silence broadly — derive `Default` for `Usage` (already does via `#[derive]`), which is fine.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-provider/
git commit -m "feat(origin-provider): Provider trait + ChatRequest/Response"
```

---

## Task P1.2 — Anthropic provider (non-streaming, API key)

**Files:**
- Create: `crates/origin-provider-anthropic/Cargo.toml`
- Create: `crates/origin-provider-anthropic/src/lib.rs`
- Create: `crates/origin-provider-anthropic/src/wire.rs`
- Create: `crates/origin-provider-anthropic/tests/messages.rs`

- [ ] **Step 1: Manifest**

```toml
[package]
name = "origin-provider-anthropic"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
origin-provider = { path = "../origin-provider" }
async-trait = "0.1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
wiremock = "0.6"
```

- [ ] **Step 2: Write failing test at `crates/origin-provider-anthropic/tests/messages.rs`**

```rust
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, Provider};
use origin_provider_anthropic::Anthropic;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn calls_anthropic_messages_endpoint() {
    let server = MockServer::start().await;

    let response_body = json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-7",
        "content": [
            { "type": "text", "text": "hi" }
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0
        }
    });

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&server)
        .await;

    let provider = Anthropic::with_base_url("test-key", &server.uri());
    let req = ChatRequest {
        system: "be terse".into(),
        messages: vec![Message::new(Role::User).with_block(Block::text("ping"))],
        model: "claude-opus-4-7".into(),
        tools: vec![],
    };
    let resp = provider
        .chat(req)
        .await
        .expect("anthropic chat should succeed");

    assert_eq!(resp.assistant.role, Role::Assistant);
    let txt = match &resp.assistant.blocks[0] {
        Block::Text { text, .. } => text.clone(),
        other => panic!("expected text block, got {other:?}"),
    };
    assert_eq!(txt, "hi");
    assert_eq!(resp.usage.input_tokens, 10);
    assert_eq!(resp.usage.output_tokens, 5);
}

#[tokio::test]
async fn auth_error_maps_to_provider_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({
                "type": "error",
                "error": { "type": "authentication_error", "message": "invalid key" }
            })),
        )
        .mount(&server)
        .await;
    let provider = Anthropic::with_base_url("bad", &server.uri());
    let err = provider
        .chat(ChatRequest {
            system: String::new(),
            messages: vec![],
            model: "x".into(),
            tools: vec![],
        })
        .await
        .expect_err("should fail with auth");
    assert!(matches!(err, origin_provider::ProviderError::Auth));
}
```

- [ ] **Step 3: Run — confirm fail**

Run: `cargo test -p origin-provider-anthropic`
Expected: compile error — crate is empty.

- [ ] **Step 4: Implement wire types at `crates/origin-provider-anthropic/src/wire.rs`**

```rust
//! Private wire JSON types for the Anthropic Messages API.

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct WireRequest<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub system: Option<&'a str>,
    pub messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<WireTool<'a>>,
}

#[derive(Serialize)]
pub struct WireMessage<'a> {
    pub role: &'static str,
    pub content: Vec<WireBlock<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireBlock<'a> {
    Text { text: &'a str },
    ToolUse { id: &'a str, name: &'a str, input: serde_json::Value },
    ToolResult { tool_use_id: &'a str, content: &'a str, is_error: bool },
}

#[derive(Serialize)]
pub struct WireTool<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub input_schema: serde_json::Value,
}

#[derive(Deserialize)]
pub struct WireResponse {
    pub content: Vec<WireRespBlock>,
    pub usage: WireUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireRespBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    Thinking { thinking: String, #[serde(default)] signature: Option<String> },
}

#[derive(Deserialize, Default)]
pub struct WireUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}
```

- [ ] **Step 5: Implement the provider at `crates/origin-provider-anthropic/src/lib.rs`**

```rust
//! Anthropic Messages API provider (non-streaming, API key auth).
//!
//! Streaming support arrives in Phase 2 (origin spec N4.4).

mod wire;

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use reqwest::StatusCode;
use serde_json::json;

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct Anthropic {
    api_key: String,
    base: String,
    client: reqwest::Client,
}

impl Anthropic {
    /// Construct with the default base URL.
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE)
    }

    /// Construct against an arbitrary base URL (for testing).
    #[must_use]
    pub fn with_base_url(api_key: impl Into<String>, base: &str) -> Self {
        Self {
            api_key: api_key.into(),
            base: base.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let wire_messages = req.messages.iter().map(message_to_wire).collect::<Vec<_>>();
        let wire_tools = req.tools.iter().map(|t| wire::WireTool {
            name: &t.name,
            description: &t.description,
            input_schema: serde_json::from_str(&t.input_schema_json)
                .unwrap_or_else(|_| json!({})),
        }).collect::<Vec<_>>();

        let body = wire::WireRequest {
            model: &req.model,
            max_tokens: DEFAULT_MAX_TOKENS,
            system: if req.system.is_empty() { None } else { Some(req.system.as_str()) },
            messages: wire_messages,
            tools: wire_tools,
        };

        let url = format!("{}/v1/messages", self.base);
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        match resp.status() {
            StatusCode::OK => {
                let wire: wire::WireResponse = resp
                    .json()
                    .await
                    .map_err(|e| ProviderError::Api(format!("decode: {e}")))?;
                Ok(decode_response(wire))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ProviderError::Auth),
            StatusCode::TOO_MANY_REQUESTS => {
                let retry = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1);
                Err(ProviderError::RateLimit { retry_after_secs: retry })
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(ProviderError::Api(format!("status {s}: {body}")))
            }
        }
    }
}

fn message_to_wire(m: &Message) -> wire::WireMessage<'_> {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user", // Anthropic represents tool results as user messages with ToolResult blocks
        Role::System => "user", // system is sent via `system` field, never as a message
    };
    let content = m.blocks.iter().filter_map(block_to_wire).collect();
    wire::WireMessage { role, content }
}

fn block_to_wire(b: &Block) -> Option<wire::WireBlock<'_>> {
    match b {
        Block::Text { text, .. } => Some(wire::WireBlock::Text { text }),
        Block::ToolUse { id, name, input_json, .. } => {
            let input: serde_json::Value =
                serde_json::from_slice(input_json).unwrap_or_else(|_| json!({}));
            Some(wire::WireBlock::ToolUse { id, name, input })
        }
        Block::ToolResult { tool_use_id, inline, .. } => {
            let content_str = inline
                .as_ref()
                .map_or("", |v| std::str::from_utf8(v).unwrap_or(""));
            Some(wire::WireBlock::ToolResult {
                tool_use_id,
                content: content_str,
                is_error: false,
            })
        }
        Block::Thinking { .. } => None, // do not re-send thinking blocks
    }
}

fn decode_response(wire: wire::WireResponse) -> ChatResponse {
    let blocks = wire.content.into_iter().map(|b| match b {
        wire::WireRespBlock::Text { text } => Block::Text { text, cache_marker: None },
        wire::WireRespBlock::ToolUse { id, name, input } => Block::ToolUse {
            id,
            name,
            input_json: serde_json::to_vec(&input).unwrap_or_default(),
            cache_marker: None,
        },
        wire::WireRespBlock::Thinking { thinking, signature } => {
            Block::Thinking { tokens: thinking, signature }
        }
    }).collect();

    let assistant = Message { role: Role::Assistant, blocks };
    let usage = Usage {
        input_tokens: wire.usage.input_tokens,
        output_tokens: wire.usage.output_tokens,
        cache_read_input_tokens: wire.usage.cache_read_input_tokens,
        cache_creation_input_tokens: wire.usage.cache_creation_input_tokens,
    };
    ChatResponse { assistant, usage }
}
```

- [ ] **Step 6: Run test — confirm pass**

Run: `cargo test -p origin-provider-anthropic`
Expected: 2 tests pass.

If `wiremock` pulls a transitive dep requiring `edition2024`, follow the MSRV-pin pattern (see `[[project-msrv-dep-pinning]]` memory): `cargo update -p <offending-dep> --precise <ver>` until `cargo build -p origin-provider-anthropic` succeeds, and commit the resulting `Cargo.lock` deltas as part of this task.

- [ ] **Step 7: Verification gate**

- `cargo test --workspace` — workspace stays green (20 tests now: 18 prior + 2 new).
- `cargo clippy --workspace --all-targets -- -D warnings` — all green.
- `cargo fmt --check`.

If clippy raises `unwrap_used` on `serde_json::from_str(...).unwrap_or_else(...)` — that's actually `unwrap_or_else` not `unwrap`, so it should not fire. If clippy raises `match_wildcard_for_single_variants` on the Role→str match, leave the explicit cases; safer than relying on a wildcard.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-provider-anthropic/ Cargo.lock
git commit -m "feat(origin-provider-anthropic): non-streaming chat endpoint"
```

---

## Task P1.3 — `origin-tools` skeleton + tool macro

**Files:**
- Create: `crates/origin-tools/Cargo.toml`
- Create: `crates/origin-tools/src/lib.rs`
- Create: `crates/origin-tools/src/registry.rs`
- Create: `crates/origin-tools/src/macros.rs`
- Create: `crates/origin-tools/tests/registry.rs`

- [ ] **Step 1: Manifest**

```toml
[package]
name = "origin-tools"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-core = { path = "../origin-core" }
inventory = "0.3"
async-trait = "0.1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process", "io-util", "fs"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Failing test at `crates/origin-tools/tests/registry.rs`**

```rust
use origin_tools::{registry_iter, SideEffects, Tier, ToolMeta, Urgency};

// Register a tiny test tool using the macro.
origin_tools::origin_tool! {
    name: "registry-test-echo",
    description: "echoes input (test only)",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
}

#[test]
fn registry_contains_test_tool() {
    let names: Vec<&'static str> = registry_iter().map(|m: &ToolMeta| m.name).collect();
    assert!(
        names.contains(&"registry-test-echo"),
        "registry should include the test tool, got {names:?}"
    );
}

#[test]
fn tool_meta_fields_present() {
    let meta = registry_iter()
        .find(|m| m.name == "registry-test-echo")
        .expect("test tool must be registered");
    assert_eq!(meta.tier, Tier::AutoAllowed);
    assert_eq!(meta.urgency, Urgency::Low);
    assert_eq!(meta.side_effects, SideEffects::Pure);
    assert!(!meta.input_schema.is_empty());
}
```

- [ ] **Step 3: Run — confirm fail (crate doesn't exist).**

- [ ] **Step 4: Implement `crates/origin-tools/src/lib.rs`**

```rust
//! Tool registry + macros + builtin tools.

pub mod macros;
pub mod registry;

pub use registry::{registry_iter, ToolMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    AutoAllowed,
    RequiresPermission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Urgency {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SideEffects {
    Pure,
    Mutating,
}
```

- [ ] **Step 5: Implement `crates/origin-tools/src/registry.rs`**

```rust
//! Compile-time tool registry backed by the `inventory` crate.
//!
//! Each `origin_tool!` invocation submits a `ToolMeta` into the inventory.
//! `registry_iter` walks all registered tools at runtime.

use crate::{SideEffects, Tier, Urgency};

#[derive(Debug)]
pub struct ToolMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub urgency: Urgency,
    pub side_effects: SideEffects,
    pub input_schema: &'static str,
}

inventory::collect!(ToolMeta);

#[must_use]
pub fn registry_iter() -> inventory::iter<ToolMeta> {
    inventory::iter::<ToolMeta>
}
```

- [ ] **Step 6: Implement `crates/origin-tools/src/macros.rs`**

```rust
//! `origin_tool!` macro — registers a tool's metadata into the inventory.

#[macro_export]
macro_rules! origin_tool {
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $sfx:expr,
        input_schema: $schema:expr
        $(,)?
    ) => {
        inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $sfx,
                input_schema: $schema,
            }
        }
    };
}
```

- [ ] **Step 7: Run test — confirm pass**

Run: `cargo test -p origin-tools`
Expected: 2 tests pass.

- [ ] **Step 8: Verification gate**

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 9: Commit**

```bash
git add crates/origin-tools/
git commit -m "feat(origin-tools): inventory-backed registry + origin_tool! macro"
```

---

## Task P1.4 — Tool: `Read`

**Files:**
- Create: `crates/origin-tools/src/builtins/mod.rs`
- Create: `crates/origin-tools/src/builtins/read.rs`
- Create: `crates/origin-tools/tests/read.rs`
- Modify: `crates/origin-tools/src/lib.rs` — `pub mod builtins;`

- [ ] **Step 1: Failing test at `crates/origin-tools/tests/read.rs`**

```rust
use origin_tools::builtins::read::read_tool;
use std::io::Write;

#[test]
fn reads_an_existing_file() {
    let mut f = tempfile::NamedTempFile::new().expect("create tempfile");
    f.write_all(b"hello world").expect("write tempfile");
    let path = f.path().to_str().expect("path utf8");
    let out = read_tool(path).expect("read should succeed");
    assert_eq!(out, "hello world");
}

#[test]
fn missing_file_returns_error() {
    let err = read_tool("/this/path/definitely/does/not/exist/origin-test")
        .expect_err("missing file should error");
    let msg = format!("{err}");
    assert!(
        msg.to_ascii_lowercase().contains("not"),
        "error should mention not-found, got: {msg}"
    );
}
```

- [ ] **Step 2: Run — confirm fail (`read_tool` doesn't exist).**

- [ ] **Step 3: Implement `crates/origin-tools/src/builtins/mod.rs`**

```rust
pub mod read;
```

- [ ] **Step 4: Implement `crates/origin-tools/src/builtins/read.rs`**

```rust
//! `Read` tool — reads a UTF-8 text file in full.

use crate::{SideEffects, Tier, Urgency};

/// Read the contents of a UTF-8 text file.
///
/// # Errors
/// Returns `io::Error` if the file cannot be opened or is not valid UTF-8.
pub fn read_tool(path: &str) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

crate::origin_tool! {
    name: "Read",
    description: "Read the contents of a UTF-8 text file at the given path.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Absolute file path" }
        },
        "required": ["path"]
    }"#,
}
```

- [ ] **Step 5: Add `pub mod builtins;` to `crates/origin-tools/src/lib.rs`**

Edit the existing `lib.rs`:

```rust
//! Tool registry + macros + builtin tools.

pub mod builtins;
pub mod macros;
pub mod registry;

pub use registry::{registry_iter, ToolMeta};

// (rest unchanged)
```

- [ ] **Step 6: Run tests — confirm pass.** `cargo test -p origin-tools` → 4 tests (2 registry + 2 read).

- [ ] **Step 7: Verification gate** (workspace fmt + clippy + test).

- [ ] **Step 8: Commit**

```bash
git add crates/origin-tools/
git commit -m "feat(origin-tools): Read builtin"
```

---

## Task P1.5 — Tool: `Glob`

**Files:**
- Create: `crates/origin-tools/src/builtins/glob_tool.rs`
- Create: `crates/origin-tools/tests/glob.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs` — add `pub mod glob_tool;`
- Modify: `crates/origin-tools/Cargo.toml` — add `glob = "0.3"`.

- [ ] **Step 1: Add `glob = "0.3"` to `[dependencies]` in `crates/origin-tools/Cargo.toml`.**

- [ ] **Step 2: Failing test at `crates/origin-tools/tests/glob.rs`**

```rust
use origin_tools::builtins::glob_tool::glob_tool;
use std::fs::{self, File};

fn make_tree(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    File::create(root.join("src/main.rs")).unwrap();
    File::create(root.join("src/lib.rs")).unwrap();
    File::create(root.join("tests/it.rs")).unwrap();
    File::create(root.join("README.md")).unwrap();
}

#[test]
fn matches_rust_files_under_src() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_tree(dir.path());
    let pattern = dir.path().join("src/*.rs");
    let mut hits = glob_tool(pattern.to_str().expect("utf8")).expect("glob ok");
    hits.sort();
    assert_eq!(hits.len(), 2);
    assert!(hits[0].ends_with("lib.rs"));
    assert!(hits[1].ends_with("main.rs"));
}

#[test]
fn returns_empty_when_no_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_tree(dir.path());
    let pattern = dir.path().join("**/*.py");
    let hits = glob_tool(pattern.to_str().expect("utf8")).expect("glob ok");
    assert!(hits.is_empty(), "expected no python hits, got {hits:?}");
}

#[test]
fn recursive_pattern_descends() {
    let dir = tempfile::tempdir().expect("tempdir");
    make_tree(dir.path());
    let pattern = dir.path().join("**/*.rs");
    let hits = glob_tool(pattern.to_str().expect("utf8")).expect("glob ok");
    assert_eq!(hits.len(), 3, "expected 3 rust files anywhere, got {hits:?}");
}
```

- [ ] **Step 3: Run — confirm fail.**

- [ ] **Step 4: Implement `crates/origin-tools/src/builtins/glob_tool.rs`**

```rust
//! `Glob` tool — return absolute paths matching a glob pattern.

use crate::{SideEffects, Tier, Urgency};

/// Resolve a glob pattern to a list of file paths (as strings).
///
/// # Errors
/// Returns a `String` describing a malformed pattern or filesystem walk failure.
pub fn glob_tool(pattern: &str) -> Result<Vec<String>, String> {
    let walker = glob::glob(pattern).map_err(|e| format!("bad pattern: {e}"))?;
    let mut out = Vec::new();
    for entry in walker {
        match entry {
            Ok(p) => {
                if let Some(s) = p.to_str() {
                    out.push(s.to_string());
                }
            }
            Err(e) => return Err(format!("walk error: {e}")),
        }
    }
    Ok(out)
}

crate::origin_tool! {
    name: "Glob",
    description: "List files matching a glob pattern. Supports ** for recursive descent.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Glob pattern; use ** for recursion" }
        },
        "required": ["pattern"]
    }"#,
}
```

- [ ] **Step 5: Add `pub mod glob_tool;` to `crates/origin-tools/src/builtins/mod.rs`.**

- [ ] **Step 6: Run tests — confirm pass.** Total now 7.

- [ ] **Step 7: Verification gate.**

- [ ] **Step 8: Commit**

```bash
git add crates/origin-tools/
git commit -m "feat(origin-tools): Glob builtin"
```

---

## Task P1.6 — Tool: `Grep`

**Files:**
- Create: `crates/origin-tools/src/builtins/grep_tool.rs`
- Create: `crates/origin-tools/tests/grep.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs` — `pub mod grep_tool;`.
- Modify: `crates/origin-tools/Cargo.toml` — add `grep-searcher`, `grep-regex`, `grep-matcher`.

- [ ] **Step 1: Add deps**

In `crates/origin-tools/Cargo.toml` `[dependencies]`:

```toml
grep-searcher = "0.1"
grep-regex    = "0.1"
grep-matcher  = "0.1"
walkdir       = "2"
```

- [ ] **Step 2: Failing test at `crates/origin-tools/tests/grep.rs`**

```rust
use origin_tools::builtins::grep_tool::grep_tool;
use std::fs;

#[test]
fn finds_pattern_in_one_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma").expect("write");
    fs::write(dir.path().join("b.txt"), "delta\nepsilon").expect("write");

    let mut hits = grep_tool(
        r"alpha|epsilon",
        dir.path().to_str().expect("utf8 path"),
    )
    .expect("grep ok");
    hits.sort();
    assert_eq!(hits.len(), 2, "expected one hit per file, got {hits:?}");
}

#[test]
fn returns_empty_when_no_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.txt"), "alpha\nbeta").expect("write");
    let hits = grep_tool("zzz", dir.path().to_str().expect("utf8")).expect("grep ok");
    assert!(hits.is_empty(), "expected no hits, got {hits:?}");
}

#[test]
fn invalid_regex_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = grep_tool("(", dir.path().to_str().expect("utf8")).expect_err("bad regex");
    let s = format!("{err}");
    assert!(s.contains("regex"), "expected regex error, got: {s}");
}
```

- [ ] **Step 3: Run — confirm fail.**

- [ ] **Step 4: Implement `crates/origin-tools/src/builtins/grep_tool.rs`**

```rust
//! `Grep` tool — search files under a root for a regex pattern.
//!
//! Returns lines as `"<path>:<line_number>: <line_content>"`.

use crate::{SideEffects, Tier, Urgency};
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use walkdir::WalkDir;

/// Search for `pattern` (regex) under `root` (directory or file).
///
/// # Errors
/// Returns a `String` describing a bad regex, walk failure, or read failure.
pub fn grep_tool(pattern: &str, root: &str) -> Result<Vec<String>, String> {
    let matcher =
        RegexMatcher::new(pattern).map_err(|e| format!("regex error: {e}"))?;
    let mut results = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| format!("walk error: {e}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path().to_path_buf();
        let path_display = path.display().to_string();
        let mut searcher = Searcher::new();
        let mut local: Vec<String> = Vec::new();
        let sink_path = path_display.clone();
        let res = searcher.search_path(
            &matcher,
            &path,
            UTF8(|lnum, line| {
                local.push(format!("{sink_path}:{lnum}: {}", line.trim_end_matches('\n')));
                Ok(true)
            }),
        );
        if let Err(e) = res {
            // skip non-utf8 / binary files
            let s = e.to_string();
            if !s.contains("invalid utf-8") {
                return Err(format!("read error in {path_display}: {e}"));
            }
        }
        results.extend(local);
    }
    Ok(results)
}

crate::origin_tool! {
    name: "Grep",
    description: "Recursively search files under a root for a regex pattern. Returns path:line:content.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "pattern": { "type": "string", "description": "Regex pattern" },
            "root":    { "type": "string", "description": "Directory or file path" }
        },
        "required": ["pattern", "root"]
    }"#,
}
```

- [ ] **Step 5: Add `pub mod grep_tool;` to `builtins/mod.rs`.**

- [ ] **Step 6: Run tests — confirm pass.** Total now 10.

- [ ] **Step 7: Verification gate.**

- [ ] **Step 8: Commit**

```bash
git add crates/origin-tools/
git commit -m "feat(origin-tools): Grep builtin"
```

---

## Task P1.7 — Tool: `Edit`

**Files:**
- Create: `crates/origin-tools/src/builtins/edit.rs`
- Create: `crates/origin-tools/tests/edit.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs` — `pub mod edit;`.

- [ ] **Step 1: Failing test at `crates/origin-tools/tests/edit.rs`**

```rust
use origin_tools::builtins::edit::edit_tool;
use std::fs;

#[test]
fn replaces_unique_string() {
    let f = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(f.path(), "alpha beta gamma").expect("write");
    edit_tool(
        f.path().to_str().expect("utf8"),
        "beta",
        "BETA",
    )
    .expect("edit ok");
    let after = fs::read_to_string(f.path()).expect("read");
    assert_eq!(after, "alpha BETA gamma");
}

#[test]
fn errors_when_old_string_not_found() {
    let f = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(f.path(), "alpha beta").expect("write");
    let err = edit_tool(
        f.path().to_str().expect("utf8"),
        "gamma",
        "GAMMA",
    )
    .expect_err("should error on missing");
    let msg = format!("{err}");
    assert!(msg.contains("not found"), "got: {msg}");
}

#[test]
fn errors_when_old_string_is_ambiguous() {
    let f = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(f.path(), "abc abc abc").expect("write");
    let err = edit_tool(
        f.path().to_str().expect("utf8"),
        "abc",
        "XYZ",
    )
    .expect_err("should error on ambiguous");
    let msg = format!("{err}");
    assert!(
        msg.contains("ambig") || msg.contains("multiple") || msg.contains("not unique"),
        "got: {msg}"
    );
}
```

- [ ] **Step 2: Run — confirm fail.**

- [ ] **Step 3: Implement `crates/origin-tools/src/builtins/edit.rs`**

```rust
//! `Edit` tool — find-and-replace a unique string in a UTF-8 file.

use crate::{SideEffects, Tier, Urgency};

/// Replace `old` with `new` in the file at `path`. `old` must appear exactly once.
///
/// # Errors
/// Returns a `String` describing not-found, ambiguous (multiple matches), or I/O failure.
pub fn edit_tool(path: &str, old: &str, new: &str) -> Result<(), String> {
    let contents = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let count = contents.matches(old).count();
    match count {
        0 => Err(format!("'{old}' not found in {path}")),
        1 => {
            let updated = contents.replacen(old, new, 1);
            std::fs::write(path, updated).map_err(|e| format!("write: {e}"))?;
            Ok(())
        }
        n => Err(format!(
            "'{old}' is not unique in {path} ({n} occurrences); refine the search string"
        )),
    }
}

crate::origin_tool! {
    name: "Edit",
    description: "Find-and-replace a unique string in a file. Errors if old_string is missing or ambiguous.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "path":       { "type": "string" },
            "old_string": { "type": "string" },
            "new_string": { "type": "string" }
        },
        "required": ["path", "old_string", "new_string"]
    }"#,
}
```

- [ ] **Step 4: Add `pub mod edit;` to `builtins/mod.rs`.**

- [ ] **Step 5: Run tests — confirm pass.** Total now 13.

- [ ] **Step 6: Verification gate.**

- [ ] **Step 7: Commit**

```bash
git add crates/origin-tools/
git commit -m "feat(origin-tools): Edit builtin"
```

---

## Task P1.8 — Tool: `Bash`

**Files:**
- Create: `crates/origin-tools/src/builtins/bash.rs`
- Create: `crates/origin-tools/tests/bash.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs` — `pub mod bash;`.

- [ ] **Step 1: Failing test at `crates/origin-tools/tests/bash.rs`**

```rust
use origin_tools::builtins::bash::bash_tool;

#[tokio::test]
async fn echoes_and_returns_stdout() {
    // Use a portable invocation: print a fixed string via the platform shell.
    #[cfg(unix)]
    let cmd = "printf 'hello-bash'";
    #[cfg(windows)]
    let cmd = "Write-Host -NoNewline 'hello-bash'";

    let out = bash_tool(cmd).await.expect("bash ok");
    assert!(out.stdout.contains("hello-bash"), "got: {:?}", out);
    assert_eq!(out.exit_code, 0);
}

#[tokio::test]
async fn non_zero_exit_propagates() {
    #[cfg(unix)]
    let cmd = "exit 7";
    #[cfg(windows)]
    let cmd = "exit 7";

    let out = bash_tool(cmd).await.expect("bash ran");
    assert_eq!(out.exit_code, 7, "expected exit 7, got {:?}", out);
}
```

- [ ] **Step 2: Run — confirm fail.**

- [ ] **Step 3: Implement `crates/origin-tools/src/builtins/bash.rs`**

```rust
//! `Bash` tool — run a shell command, capture stdout/stderr/exit-code.
//!
//! Uses `sh -c` on Unix, `pwsh -Command` on Windows (fallback `powershell.exe`).

use crate::{SideEffects, Tier, Urgency};
use tokio::process::Command;

#[derive(Debug)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Run `command` through the platform shell, capturing all output.
///
/// # Errors
/// Returns a `String` describing process-spawn failure (the shell itself missing).
/// Non-zero exit codes do NOT error — they're returned in `BashOutput.exit_code`.
pub async fn bash_tool(command: &str) -> Result<BashOutput, String> {
    #[cfg(unix)]
    let (program, args): (&str, [&str; 2]) = ("sh", ["-c", command]);

    #[cfg(windows)]
    let (program, args): (&str, [&str; 2]) = ("pwsh", ["-NoProfile", command]);

    let result = Command::new(program)
        .args(args)
        .output()
        .await;

    #[cfg(windows)]
    let result = match result {
        Ok(o) => Ok(o),
        Err(_) => Command::new("powershell")
            .args(["-NoProfile", "-Command", command])
            .output()
            .await,
    };

    let output = result.map_err(|e| format!("spawn: {e}"))?;
    Ok(BashOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

crate::origin_tool! {
    name: "Bash",
    description: "Execute a shell command. Returns stdout, stderr, and exit code.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "Shell command line" }
        },
        "required": ["command"]
    }"#,
}
```

- [ ] **Step 4: Add `pub mod bash;` to `builtins/mod.rs`.**

- [ ] **Step 5: Run tests — confirm pass.** Total now 15.

If the Windows test invocation fails because `pwsh` isn't on the runner, the fallback path takes `powershell.exe` automatically. Verify the test passes on whichever Windows shell is available locally.

- [ ] **Step 6: Verification gate.**

- [ ] **Step 7: Commit**

```bash
git add crates/origin-tools/
git commit -m "feat(origin-tools): Bash builtin"
```

---

## Task P1.9 — `origin-permission` (tier check + prompter trait)

**Files:**
- Create: `crates/origin-permission/Cargo.toml`
- Create: `crates/origin-permission/src/lib.rs`
- Create: `crates/origin-permission/src/prompt.rs`
- Create: `crates/origin-permission/tests/check.rs`

- [ ] **Step 1: Manifest**

```toml
[package]
name = "origin-permission"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
origin-tools = { path = "../origin-tools" }
async-trait = "0.1"
thiserror = "1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Failing test at `crates/origin-permission/tests/check.rs`**

```rust
use origin_permission::{check, prompt::{AlwaysAllow, AlwaysDeny}, Outcome};
use origin_tools::{SideEffects, Tier, ToolMeta, Urgency};

fn meta(name: &'static str, tier: Tier) -> ToolMeta {
    ToolMeta {
        name,
        description: "test",
        tier,
        urgency: Urgency::Low,
        side_effects: SideEffects::Pure,
        input_schema: "{}",
    }
}

#[tokio::test]
async fn auto_allowed_bypasses_prompter() {
    let m = meta("Read", Tier::AutoAllowed);
    let decision = check(&m, "/tmp/x", &AlwaysDeny).await;
    assert_eq!(decision.outcome, Outcome::Allow);
}

#[tokio::test]
async fn requires_permission_asks_prompter_allow() {
    let m = meta("Bash", Tier::RequiresPermission);
    let decision = check(&m, "rm -rf /", &AlwaysAllow).await;
    assert_eq!(decision.outcome, Outcome::Allow);
}

#[tokio::test]
async fn requires_permission_asks_prompter_deny() {
    let m = meta("Bash", Tier::RequiresPermission);
    let decision = check(&m, "rm -rf /", &AlwaysDeny).await;
    assert_eq!(decision.outcome, Outcome::Deny);
}
```

- [ ] **Step 3: Run — confirm fail.**

- [ ] **Step 4: Implement `crates/origin-permission/src/lib.rs`**

```rust
//! Permission engine: tier-based check with a pluggable `Prompter`.
//!
//! `AutoAllowed` tools bypass the prompter; `RequiresPermission` tools ask.
//! Later phases add user-configured rules (P10) and bloom-filter pre-check (N9.2).

pub mod prompt;

use origin_tools::{Tier, ToolMeta};
use prompt::Prompter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Deny,
}

#[derive(Debug)]
pub struct Decision {
    pub outcome: Outcome,
    pub reason: String,
}

/// Decide whether `meta`'s invocation with `args_preview` is allowed.
///
/// `args_preview` is a short human-readable summary of the tool's input
/// (e.g., `"/path/to/file"` for Read or `"git status"` for Bash).
pub async fn check(meta: &ToolMeta, args_preview: &str, prompter: &dyn Prompter) -> Decision {
    match meta.tier {
        Tier::AutoAllowed => Decision {
            outcome: Outcome::Allow,
            reason: "tier=AutoAllowed".into(),
        },
        Tier::RequiresPermission => {
            let allowed = prompter.ask(meta, args_preview).await;
            Decision {
                outcome: if allowed { Outcome::Allow } else { Outcome::Deny },
                reason: if allowed {
                    "user-approved".into()
                } else {
                    "user-denied".into()
                },
            }
        }
    }
}
```

- [ ] **Step 5: Implement `crates/origin-permission/src/prompt.rs`**

```rust
//! `Prompter` trait + test prompters.
//!
//! Production prompter lives in the TUI client (P1.11+) and asks the user via
//! the side panel (introduced in P4). Headless prompter (auto-deny by default)
//! arrives in P13.

use async_trait::async_trait;
use origin_tools::ToolMeta;

#[async_trait]
pub trait Prompter: Send + Sync {
    /// Ask the user to approve a tool invocation. Returns `true` for allow.
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool;
}

pub struct AlwaysAllow;
pub struct AlwaysDeny;

#[async_trait]
impl Prompter for AlwaysAllow {
    async fn ask(&self, _meta: &ToolMeta, _args_preview: &str) -> bool { true }
}

#[async_trait]
impl Prompter for AlwaysDeny {
    async fn ask(&self, _meta: &ToolMeta, _args_preview: &str) -> bool { false }
}
```

- [ ] **Step 6: Run — confirm pass.** Three tests pass.

- [ ] **Step 7: Verification gate.**

- [ ] **Step 8: Commit**

```bash
git add crates/origin-permission/
git commit -m "feat(origin-permission): tier-based check with Prompter trait"
```

---

## Task P1.10 — Agent loop in daemon

**Files:**
- Modify: `crates/origin-daemon/Cargo.toml` — add deps.
- Create: `crates/origin-daemon/src/protocol.rs`
- Create: `crates/origin-daemon/src/agent.rs`
- Create: `crates/origin-daemon/src/session.rs`
- Modify: `crates/origin-daemon/src/main.rs` — wire request kinds.
- Create: `crates/origin-daemon/tests/loop_e2e.rs`

- [ ] **Step 1: Add deps to `crates/origin-daemon/Cargo.toml`**

```toml
origin-core       = { path = "../origin-core" }
origin-provider   = { path = "../origin-provider" }
origin-provider-anthropic = { path = "../origin-provider-anthropic" }
origin-tools      = { path = "../origin-tools" }
origin-permission = { path = "../origin-permission" }
serde             = { version = "1", features = ["derive"] }
serde_json        = "1"
async-trait       = "0.1"
thiserror         = "1"
```

Move existing `tokio` and `tracing` blocks down to keep them grouped.

- [ ] **Step 2: Create `crates/origin-daemon/src/protocol.rs`**

```rust
//! IPC request/response shapes for daemon ↔ client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptRequest {
    pub system: String,
    pub model: String,
    pub user_text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptReply {
    pub assistant_text: String,
    pub turns: u32,
}
```

- [ ] **Step 3: Failing test at `crates/origin-daemon/tests/loop_e2e.rs`**

```rust
use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self { responses: Mutex::new(responses) }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str { "scripted" }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let mut q = self.responses.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

#[tokio::test]
async fn loop_terminates_when_no_tool_use() {
    let provider = ScriptedProvider::new(vec![ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
        usage: Usage::default(),
    }]);
    let mut session = Session::new("test", "claude-opus-4-7");
    let summary = run_loop(
        &mut session,
        "hello",
        &provider,
        &AlwaysAllow,
        LoopOptions::default(),
    )
    .await
    .expect("loop ok");
    assert_eq!(summary.assistant_text, "done");
    assert_eq!(summary.turns, 1);
}

#[tokio::test]
async fn loop_dispatches_tool_and_continues() {
    let tool_use = Block::ToolUse {
        id: "tu_1".into(),
        name: "Read".into(),
        // We'll set up a tempfile right after this block:
        input_json: serde_json::to_vec(&serde_json::json!({"path": std::env::temp_dir().join("origin-loop-test.txt").to_str().expect("utf8")})).expect("json"),
        cache_marker: None,
    };
    let final_text = Block::text("final");
    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(tool_use),
            usage: Usage::default(),
        },
        ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(final_text),
            usage: Usage::default(),
        },
    ]);

    let path = std::env::temp_dir().join("origin-loop-test.txt");
    std::fs::write(&path, "tool result content").expect("write tempfile");

    let mut session = Session::new("test", "claude-opus-4-7");
    let summary = run_loop(
        &mut session,
        "please read the file",
        &provider,
        &AlwaysAllow,
        LoopOptions::default(),
    )
    .await
    .expect("loop ok");

    let _ = std::fs::remove_file(&path);
    assert_eq!(summary.assistant_text, "final");
    assert_eq!(summary.turns, 2);
}

#[tokio::test]
async fn loop_caps_at_max_turns() {
    // Provider always emits a tool_use — never terminates on its own.
    let tool_use = || Block::ToolUse {
        id: ulid::Ulid::new().to_string(),
        name: "Read".into(),
        input_json: serde_json::to_vec(&serde_json::json!({"path": "/dev/null"})).expect("json"),
        cache_marker: None,
    };
    let mut responses = Vec::new();
    for _ in 0..5 {
        responses.push(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(tool_use()),
            usage: Usage::default(),
        });
    }
    let provider = ScriptedProvider::new(responses);
    let mut session = Session::new("test", "claude-opus-4-7");
    let opts = LoopOptions { max_turns: 3 };
    let err = run_loop(&mut session, "loop", &provider, &AlwaysAllow, opts)
        .await
        .expect_err("should hit max_turns");
    let msg = format!("{err}");
    assert!(msg.contains("max_turns"), "got: {msg}");
}
```

Note: `/dev/null` is Unix-only — on Windows replace with `NUL`. Add a `#[cfg(unix)]` arm or use a tempfile path. For simplicity, use a single throwaway path that exists on the host:

```rust
let path = std::env::temp_dir().join("origin-cap-test.txt");
std::fs::write(&path, "x").expect("write");
// ... use path.to_str() in input_json ...
```

Update the test accordingly before running.

- [ ] **Step 4: Run — confirm fail.**

- [ ] **Step 5: Implement `crates/origin-daemon/src/session.rs`**

```rust
//! `Session` — in-memory message log + metadata.
//!
//! Persistence (P1.12) wraps this with SQLite writes per turn.

use origin_core::types::{Message, MessageId};

#[derive(Debug)]
pub struct Session {
    pub id: MessageId,
    pub provider_name: String,
    pub model: String,
    pub messages: Vec<Message>,
}

impl Session {
    #[must_use]
    pub fn new(provider_name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            provider_name: provider_name.into(),
            model: model.into(),
            messages: Vec::new(),
        }
    }

    pub fn push(&mut self, m: Message) {
        self.messages.push(m);
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<Message> {
        self.messages.clone()
    }
}
```

- [ ] **Step 6: Implement `crates/origin-daemon/src/agent.rs`**

```rust
//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::session::Session;
use origin_core::types::{Block, Message, Role};
use origin_permission::{check, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_tools::{registry_iter, ToolMeta};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct LoopOptions {
    pub max_turns: u32,
}
impl Default for LoopOptions {
    fn default() -> Self { Self { max_turns: 25 } }
}

#[derive(Debug)]
pub struct LoopSummary {
    pub assistant_text: String,
    pub turns: u32,
}

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("provider: {0}")]
    Provider(#[from] origin_provider::ProviderError),
    #[error("hit max_turns ({0})")]
    MaxTurns(u32),
    #[error("tool not found: {0}")]
    UnknownTool(String),
    #[error("tool denied: {0}")]
    Denied(String),
    #[error("tool failure: {0}")]
    ToolFailure(String),
    #[error("malformed tool args: {0}")]
    BadArgs(String),
}

pub async fn run_loop(
    session: &mut Session,
    user_text: &str,
    provider: &dyn Provider,
    prompter: &dyn Prompter,
    opts: LoopOptions,
) -> Result<LoopSummary, LoopError> {
    session.push(Message::new(Role::User).with_block(Block::text(user_text)));

    let tools_schema = registry_iter()
        .map(|m| origin_provider::ToolSchema {
            name: m.name.to_string(),
            description: m.description.to_string(),
            input_schema_json: m.input_schema.to_string(),
        })
        .collect::<Vec<_>>();

    for turn in 1..=opts.max_turns {
        let req = ChatRequest {
            system: String::new(),
            messages: session.snapshot(),
            model: session.model.clone(),
            tools: tools_schema.clone(),
        };
        let resp = provider.chat(req).await?;
        session.push(resp.assistant.clone());

        // If no tool_use blocks, we're done.
        let tool_uses: Vec<(&str, &str, &[u8])> = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, name, input_json, .. } => {
                    Some((id.as_str(), name.as_str(), input_json.as_slice()))
                }
                _ => None,
            })
            .collect();
        if tool_uses.is_empty() {
            let text = resp
                .assistant
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            return Ok(LoopSummary {
                assistant_text: text,
                turns: turn,
            });
        }

        // Dispatch each tool_use.
        let mut tool_results: Vec<Block> = Vec::with_capacity(tool_uses.len());
        for (id, name, input_bytes) in tool_uses {
            let meta = registry_iter()
                .find(|m| m.name == name)
                .ok_or_else(|| LoopError::UnknownTool(name.into()))?;
            let args: Value = serde_json::from_slice(input_bytes)
                .map_err(|e| LoopError::BadArgs(e.to_string()))?;
            let preview = args.to_string();

            let decision = check(meta, &preview, prompter).await;
            if decision.outcome == Outcome::Deny {
                return Err(LoopError::Denied(name.into()));
            }

            let result_text = dispatch_tool(meta, &args).await?;
            tool_results.push(Block::ToolResult {
                tool_use_id: id.into(),
                handle: None,
                inline: Some(result_text.into_bytes()),
                cache_marker: None,
            });
        }

        // Append tool results as a single user-role message (Anthropic's convention).
        let mut tool_msg = Message::new(Role::Tool);
        tool_msg.blocks = tool_results;
        session.push(tool_msg);
    }
    Err(LoopError::MaxTurns(opts.max_turns))
}

async fn dispatch_tool(meta: &ToolMeta, args: &Value) -> Result<String, LoopError> {
    match meta.name {
        "Read" => {
            let path = args.get("path").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Read: missing `path`".into()))?;
            origin_tools::builtins::read::read_tool(path)
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "Glob" => {
            let pat = args.get("pattern").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Glob: missing `pattern`".into()))?;
            let hits = origin_tools::builtins::glob_tool::glob_tool(pat)
                .map_err(LoopError::ToolFailure)?;
            Ok(hits.join("\n"))
        }
        "Grep" => {
            let pat = args.get("pattern").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Grep: missing `pattern`".into()))?;
            let root = args.get("root").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Grep: missing `root`".into()))?;
            let hits = origin_tools::builtins::grep_tool::grep_tool(pat, root)
                .map_err(LoopError::ToolFailure)?;
            Ok(hits.join("\n"))
        }
        "Edit" => {
            let path = args.get("path").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `path`".into()))?;
            let old = args.get("old_string").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `old_string`".into()))?;
            let new = args.get("new_string").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `new_string`".into()))?;
            origin_tools::builtins::edit::edit_tool(path, old, new)
                .map(|()| "edit ok".to_string())
                .map_err(LoopError::ToolFailure)
        }
        "Bash" => {
            let cmd = args.get("command").and_then(|v| v.as_str())
                .ok_or_else(|| LoopError::BadArgs("Bash: missing `command`".into()))?;
            let out = origin_tools::builtins::bash::bash_tool(cmd)
                .await
                .map_err(LoopError::ToolFailure)?;
            Ok(format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                out.exit_code, out.stdout, out.stderr
            ))
        }
        other => Err(LoopError::UnknownTool(other.into())),
    }
}
```

The hard-coded dispatch table is a pragmatic shortcut for P1 — a fully generic dispatcher arrives with the proc-macro upgrade in a later phase (the master plan references this).

- [ ] **Step 7: Expose modules from `crates/origin-daemon/src/main.rs`**

Add to the top of `main.rs`:

```rust
pub mod agent;
pub mod protocol;
pub mod session;
```

Note: `main.rs` is the crate root for a binary crate. To expose modules in an integration test (`tests/loop_e2e.rs`), the binary crate also needs a library entry. The cleanest fix is to add a sibling `lib.rs`:

Create `crates/origin-daemon/src/lib.rs`:

```rust
pub mod agent;
pub mod protocol;
pub mod session;
```

Then keep `main.rs` minimal — it can `use origin_daemon::{...}` as a library consumer (binary + library in the same crate; cargo handles this).

In `crates/origin-daemon/Cargo.toml`, add:

```toml
[lib]
name = "origin_daemon"
path = "src/lib.rs"

[[bin]]
name = "origin-daemon"
path = "src/main.rs"
```

- [ ] **Step 8: Run tests — confirm pass.**

Run: `cargo test -p origin-daemon`
Expected: 3 tests pass.

- [ ] **Step 9: Wire the prompt request handler in `src/main.rs`**

Replace the echo loop. New `main.rs`:

```rust
use std::env;

use anyhow::Result;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::{PromptReply, PromptRequest};
use origin_daemon::session::Session;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::Listener;
use origin_permission::prompt::AlwaysAllow;
use origin_provider_anthropic::Anthropic;
use tracing::{error, info};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let api_key = env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY must be set"))?;
    let provider = std::sync::Arc::new(Anthropic::new(api_key));

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    loop {
        let mut conn = listener.accept().await?;
        let provider = std::sync::Arc::clone(&provider);
        tokio::spawn(async move {
            loop {
                let body = match conn.read_frame_body().await {
                    Ok(b) => b,
                    Err(_) => break,
                };
                let req: PromptRequest = match serde_json::from_slice(&body) {
                    Ok(r) => r,
                    Err(e) => {
                        error!(error = %e, "bad prompt request");
                        let _ = conn.write_frame(FrameKind::ErrorFrame, format!("bad request: {e}").as_bytes()).await;
                        continue;
                    }
                };
                let mut session = Session::new("anthropic", &req.model);
                match run_loop(&mut session, &req.user_text, provider.as_ref(), &AlwaysAllow, LoopOptions::default()).await {
                    Ok(summary) => {
                        let reply = PromptReply {
                            assistant_text: summary.assistant_text,
                            turns: summary.turns,
                        };
                        let bytes = serde_json::to_vec(&reply).expect("serialize reply");
                        if let Err(e) = conn.write_frame(FrameKind::Response, &bytes).await {
                            error!(error = %e, "write reply");
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = conn.write_frame(FrameKind::ErrorFrame, format!("loop error: {e}").as_bytes()).await;
                    }
                }
            }
        });
    }
}

fn default_path() -> String {
    #[cfg(unix)]
    { format!("{}/origin.sock", std::env::temp_dir().display()) }
    #[cfg(windows)]
    { r"\\.\pipe\origin".to_string() }
}
```

- [ ] **Step 10: Verification gate**

- `cargo test --workspace` — all green; 3 new tests in `loop_e2e.rs`.
- `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo fmt --check`.
- `cargo build -p origin-daemon` — confirm the daemon still compiles.

- [ ] **Step 11: Commit**

```bash
git add crates/origin-daemon/
git commit -m "feat(origin-daemon): agent loop with provider + tool dispatch"
```

---

## Task P1.11 — Ratatui baseline TUI

**Files:**
- Modify: `crates/origin-cli/Cargo.toml` — add Ratatui + crossterm deps.
- Create: `crates/origin-cli/src/tui.rs`
- Create: `crates/origin-cli/src/input.rs`
- Create: `crates/origin-cli/src/screen.rs`
- Modify: `crates/origin-cli/src/main.rs`

- [ ] **Step 1: Update `crates/origin-cli/Cargo.toml` `[dependencies]`**

```toml
ratatui = { version = "0.28", default-features = false, features = ["crossterm"] }
crossterm = "0.28"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

(Existing deps `origin-ipc`, `tokio`, `anyhow` stay.)

- [ ] **Step 2: Create `crates/origin-cli/src/screen.rs`**

```rust
//! Screen layout primitives.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub fn split_main_input(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);
    (chunks[0], chunks[1])
}
```

- [ ] **Step 3: Create `crates/origin-cli/src/input.rs`**

```rust
//! Input event handling (key reducer).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    Insert(char),
    Backspace,
    Submit(String),
    Quit,
    Noop,
}

#[must_use]
pub fn reduce(buffer: &mut String, ev: KeyEvent) -> InputAction {
    match (ev.code, ev.modifiers) {
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        (KeyCode::Esc, _) => InputAction::Quit,
        (KeyCode::Enter, _) => {
            if buffer.is_empty() {
                InputAction::Noop
            } else {
                let out = std::mem::take(buffer);
                InputAction::Submit(out)
            }
        }
        (KeyCode::Backspace, _) => {
            let popped = buffer.pop();
            if popped.is_some() {
                InputAction::Backspace
            } else {
                InputAction::Noop
            }
        }
        (KeyCode::Char(c), _) => {
            buffer.push(c);
            InputAction::Insert(c)
        }
        _ => InputAction::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn enter_submits_buffer() {
        let mut buf = "hello".to_string();
        assert_eq!(reduce(&mut buf, k(KeyCode::Enter)), InputAction::Submit("hello".into()));
        assert!(buf.is_empty());
    }

    #[test]
    fn enter_on_empty_is_noop() {
        let mut buf = String::new();
        assert_eq!(reduce(&mut buf, k(KeyCode::Enter)), InputAction::Noop);
    }

    #[test]
    fn typing_appends_to_buffer() {
        let mut buf = "h".to_string();
        assert_eq!(reduce(&mut buf, k(KeyCode::Char('i'))), InputAction::Insert('i'));
        assert_eq!(buf, "hi");
    }

    #[test]
    fn backspace_pops() {
        let mut buf = "hi".to_string();
        assert_eq!(reduce(&mut buf, k(KeyCode::Backspace)), InputAction::Backspace);
        assert_eq!(buf, "h");
    }

    #[test]
    fn ctrl_c_quits() {
        let mut buf = String::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(reduce(&mut buf, ev), InputAction::Quit);
    }
}
```

- [ ] **Step 4: Create `crates/origin-cli/src/tui.rs`**

```rust
//! Ratatui baseline TUI: scrollback + prompt input.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::screen::split_main_input;

pub struct App {
    pub scrollback: Vec<String>,
    pub input: String,
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self { scrollback: Vec::new(), input: String::new() }
    }

    pub fn add_line(&mut self, prefix: &str, body: &str) {
        self.scrollback.push(format!("{prefix}{body}"));
    }
}

impl Default for App {
    fn default() -> Self { Self::new() }
}

pub fn draw(f: &mut Frame<'_>, app: &App) {
    let (main, prompt) = split_main_input(f.area());

    let lines: Vec<Line> = app.scrollback.iter()
        .map(|s| Line::from(Span::raw(s.clone())))
        .collect();
    let scroll = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("origin"));
    f.render_widget(scroll, main);

    let input = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::styled(app.input.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ])).block(Block::default().borders(Borders::ALL));
    f.render_widget(input, prompt);
}
```

- [ ] **Step 5: Replace `crates/origin-cli/src/main.rs` with the wired TUI**

```rust
use std::env;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use origin_cli::input::{reduce, InputAction};
use origin_cli::tui::{draw, App};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct PromptRequest<'a> {
    system: &'a str,
    model: &'a str,
    user_text: &'a str,
}

#[derive(Deserialize)]
struct PromptReply {
    assistant_text: String,
    turns: u32,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let model = env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into());

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.add_line("", "Connected; type a prompt and press Enter. Ctrl-C to quit.");

    loop {
        terminal.draw(|f| draw(f, &app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(ev) = event::read()? {
                match reduce(&mut app.input, ev) {
                    InputAction::Quit => break,
                    InputAction::Submit(text) => {
                        app.add_line("you> ", &text);
                        let reply = call_daemon(&path, &model, &text).await;
                        match reply {
                            Ok(r) => app.add_line(
                                &format!("origin ({} turns)> ", r.turns),
                                &r.assistant_text,
                            ),
                            Err(e) => app.add_line("error> ", &format!("{e}")),
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

async fn call_daemon(path: &str, model: &str, user_text: &str) -> Result<PromptReply> {
    let mut client = Connector::connect(path).await?;
    let body = serde_json::to_vec(&PromptRequest { system: "", model, user_text })?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;
    let resp = client.read_frame_body().await?;
    let reply: PromptReply = serde_json::from_slice(&resp)?;
    Ok(reply)
}

fn default_path() -> String {
    #[cfg(unix)]
    { format!("{}/origin.sock", std::env::temp_dir().display()) }
    #[cfg(windows)]
    { r"\\.\pipe\origin".to_string() }
}
```

For modules to be visible from `main.rs`, give origin-cli a library entry too. Create `crates/origin-cli/src/lib.rs`:

```rust
pub mod input;
pub mod screen;
pub mod tui;
```

Add to `crates/origin-cli/Cargo.toml`:

```toml
[lib]
name = "origin_cli"
path = "src/lib.rs"

[[bin]]
name = "origin"
path = "src/main.rs"
```

- [ ] **Step 6: Run unit tests — confirm pass.**

`cargo test -p origin-cli` should show the 5 input-reducer tests passing.

- [ ] **Step 7: Verification gate (workspace).**

- [ ] **Step 8: Manual smoke test**

Start the daemon (`cargo run -p origin-daemon` with `ANTHROPIC_API_KEY` set), then in another terminal `cargo run -p origin-cli`. Type a prompt, hit Enter, observe the reply rendered in the scrollback. Ctrl-C to quit.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-cli/
git commit -m "feat(origin-cli): ratatui baseline TUI"
```

---

## Task P1.12 — Sessions persisted to SQLite (inline blobs)

**Files:**
- Modify: `crates/origin-daemon/Cargo.toml` — add `origin-store`.
- Create: `crates/origin-daemon/src/session_store.rs`
- Modify: `crates/origin-daemon/src/session.rs` — add persistence hooks.
- Modify: `crates/origin-daemon/src/lib.rs` — expose new module.
- Create: `crates/origin-daemon/tests/session_persist.rs`

- [ ] **Step 1: Add dep**

In `crates/origin-daemon/Cargo.toml`:

```toml
origin-store = { path = "../origin-store" }
rkyv = { version = "0.7", features = ["validation", "bytecheck"] }
ulid = "1"
```

(`rkyv` and `ulid` aren't there yet; they're transitive via origin-core but we need direct access for the persistence code.)

- [ ] **Step 2: Failing test at `crates/origin-daemon/tests/session_persist.rs`**

```rust
use origin_core::types::{Block, Message, Role};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;

#[test]
fn round_trip_persists_messages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("origin.db");
    let store = SessionStore::open(&db).expect("open store");

    let mut s = Session::new("anthropic", "claude-opus-4-7");
    let sid = s.id.to_string();
    s.push(Message::new(Role::User).with_block(Block::text("hello")));
    s.push(Message::new(Role::Assistant).with_block(Block::text("hi")));

    store.persist_session(&s).expect("persist meta");
    for (i, m) in s.messages.iter().enumerate() {
        store
            .persist_message(&sid, u32::try_from(i).expect("u32 fits"), m)
            .expect("persist message");
    }

    let loaded = store.load_messages(&sid).expect("load");
    assert_eq!(loaded.len(), 2);
    let first_text = match &loaded[0].blocks[0] {
        Block::Text { text, .. } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert_eq!(first_text, "hello");
    assert_eq!(loaded[1].role, Role::Assistant);
}
```

- [ ] **Step 3: Run — confirm fail.**

- [ ] **Step 4: Implement `crates/origin-daemon/src/session_store.rs`**

```rust
//! SQLite-backed session persistence (inline blobs for P1; CAS handles arrive in P2).

use std::path::Path;

use origin_core::types::{Message, Role};
use origin_store::{Store, StoreError};
use thiserror::Error;

use crate::session::Session;

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("rkyv: {0}")]
    Rkyv(String),
}

pub struct SessionStore {
    inner: Store,
}

impl SessionStore {
    /// Open or create the SQLite database at `path` and run migrations.
    ///
    /// # Errors
    /// Propagates `StoreError` for open/migration failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        Ok(Self { inner: Store::open(path)? })
    }

    /// Insert (or replace) a session metadata row.
    ///
    /// # Errors
    /// Returns a sqlite error on write failure.
    pub fn persist_session(&self, s: &Session) -> Result<(), SessionStoreError> {
        let id = s.id.to_string();
        let provider = s.provider_name.clone();
        let model = s.model.clone();
        let now = now_ms();
        self.inner.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO sessions (id, created_at, title, provider, model) \
                 VALUES (?1, ?2, NULL, ?3, ?4)",
                rusqlite::params![id, now, provider, model],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Append a message row at the given turn index.
    ///
    /// # Errors
    /// Returns a sqlite error on write or an rkyv error on serialization failure.
    pub fn persist_message(
        &self,
        session_id: &str,
        turn_index: u32,
        m: &Message,
    ) -> Result<(), SessionStoreError> {
        let bytes = rkyv::to_bytes::<_, 4096>(m)
            .map_err(|e| SessionStoreError::Rkyv(e.to_string()))?
            .to_vec();
        let role: i64 = match m.role {
            Role::User => 0,
            Role::Assistant => 1,
            Role::Tool => 2,
            Role::System => 3,
        };
        let now = now_ms();
        self.inner.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO messages \
                 (session_id, turn_index, role, body_inline, handle_root, summary, created_at) \
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
                rusqlite::params![session_id, turn_index, role, bytes, now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Load all messages for a session, ordered by turn.
    ///
    /// # Errors
    /// Returns a sqlite error on read failure or an rkyv error on decode failure.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionStoreError> {
        let rows: Vec<Vec<u8>> = self.inner.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT body_inline FROM messages WHERE session_id = ?1 ORDER BY turn_index ASC",
            )?;
            let iter = stmt.query_map([session_id], |r| {
                let b: Vec<u8> = r.get(0)?;
                Ok(b)
            })?;
            let mut out = Vec::new();
            for r in iter { out.push(r?); }
            Ok(out)
        })?;

        let mut messages = Vec::with_capacity(rows.len());
        for bytes in rows {
            let archived = rkyv::check_archived_root::<Message>(&bytes)
                .map_err(|e| SessionStoreError::Rkyv(e.to_string()))?;
            let m: Message = rkyv::Deserialize::deserialize(archived, &mut rkyv::Infallible)
                .map_err(|e| SessionStoreError::Rkyv(format!("{e:?}")))?;
            messages.push(m);
        }
        Ok(messages)
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| {
            // Saturating cast — won't overflow in our lifetime.
            i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
        })
        .unwrap_or(0)
}
```

- [ ] **Step 5: Expose the module — add `pub mod session_store;` to `crates/origin-daemon/src/lib.rs`.**

- [ ] **Step 6: Run tests — confirm pass.**

- [ ] **Step 7: Wire persistence into the daemon `main.rs`**

In the daemon's prompt-handling task, open a `SessionStore` once at startup and call `persist_session` + `persist_message` for each turn boundary. Minimal change to `main.rs`:

```rust
let db_path = env::var("ORIGIN_DB").unwrap_or_else(|_| default_db_path());
let session_store = std::sync::Arc::new(
    origin_daemon::session_store::SessionStore::open(&db_path)?
);
```

Then inside the spawn, after `run_loop` succeeds:

```rust
let _ = session_store.persist_session(&session);
for (i, m) in session.messages.iter().enumerate() {
    let _ = session_store.persist_message(&session.id.to_string(), u32::try_from(i).expect("u32 fits"), m);
}
```

(For P1 we tolerate persistence failures by logging; later phases will treat them as session errors.)

Add a default DB path helper:

```rust
fn default_db_path() -> String {
    let mut p = std::env::temp_dir();
    p.push("origin.db");
    p.to_string_lossy().into_owned()
}
```

- [ ] **Step 8: Verification gate (workspace).**

- [ ] **Step 9: Commit**

```bash
git add crates/origin-daemon/
git commit -m "feat(origin-daemon): persist sessions to sqlite"
```

---

## Task P1.13 — Phase 1 checkpoint + dogfood

- [ ] **Step 1: Full verification suite**

Run each, capture exit codes:

- `cargo fmt --check` → exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings` → exit 0.
- `cargo test --workspace` → exit 0. Capture the `test result: ok. N passed; 0 failed` line. Expected N around 33–37 (18 from P0 + ~15 new across origin-provider, origin-provider-anthropic, origin-tools, origin-permission, origin-daemon, origin-cli).

- [ ] **Step 2: Manual dogfood smoke**

In one terminal:
```
$env:ANTHROPIC_API_KEY = "<your-key>"   # PowerShell, or `export` on bash
cargo run -p origin-daemon
```

In another:
```
cargo run -p origin-cli
```

Type a prompt like `What files are in the current directory? (use Glob then Read once on the README)`. Observe:
- TUI scrollback shows `you> …` and `origin (N turns)> …`.
- Daemon log shows the agent loop running (tool calls dispatched).
- Reply text mentions actual filenames from the project root.

If the model emits the tool calls but expects permission for Edit/Bash, the AlwaysAllow prompter (P1.9) will approve silently — that's the P1 behavior; interactive prompts arrive in P4.

Record the actual model output and number of turns in the CHANGELOG entry.

- [ ] **Step 3: Update `CHANGELOG.md` with a `[phase-1]` entry**

Append after the existing `[phase-0]` section:

```markdown
## [phase-1] — <YYYY-MM-DD>

### Added

- **`origin-provider`** — canonical `Provider` trait + `ChatRequest`/`ChatResponse`/`Usage`/`ToolSchema`/`ProviderError`.
- **`origin-provider-anthropic`** — non-streaming `POST /v1/messages` implementation with wiremock-driven tests; auth-error → `ProviderError::Auth`; rate-limit → `ProviderError::RateLimit`.
- **`origin-tools`** — inventory-backed compile-time tool registry, `origin_tool!` macro, and five builtins:
  - `Read` (AutoAllowed)
  - `Glob` (AutoAllowed)
  - `Grep` (AutoAllowed; recursive)
  - `Edit` (RequiresPermission, Medium; errors on missing/ambiguous old_string)
  - `Bash` (RequiresPermission, High; PowerShell on Windows)
- **`origin-permission`** — tier-based check with pluggable `Prompter` trait; `AlwaysAllow`/`AlwaysDeny` test prompters.
- **`origin-daemon`** — `Session`, `run_loop`, `SessionStore`. Agent loop:
  1. Append user message.
  2. Call provider with current messages + tool schemas.
  3. Dispatch each `tool_use` block through permission + registry.
  4. Append `tool_result` blocks as a `Role::Tool` message.
  5. Repeat until no `tool_use`; cap at 25 turns.
- **`origin-cli`** — Ratatui baseline TUI with scrollback + prompt input; Ctrl-C / Esc to quit.

### Dogfood

- First end-to-end run against Anthropic claude-opus-4-7. Sample prompt `<your prompt>` produced N-turn reply: <summary>.

### Test coverage at phase exit

- N tests passing across the workspace.

### Known limitations (deferred)

- Non-streaming only (streaming arrives in P2 with the byte ring buffer).
- AlwaysAllow prompter — interactive prompts move to the TUI side panel in P4.
- Sessions stored inline in `body_inline` blobs (CAS arrives in P2).
- API key read from env var; KeyVault arrives in P8.
```

Replace `<YYYY-MM-DD>`, `<your prompt>`, `<summary>`, and `N` with actual values.

- [ ] **Step 4: Commit + tag**

```bash
git add CHANGELOG.md
git commit -m "docs: phase-1 CHANGELOG entry + dogfood notes"
git tag -a p1-complete -m "Phase 1: first end-to-end turn with Anthropic + 5 tools"
```

- [ ] **Step 5: Confirm tag exists at HEAD**

```bash
git log --oneline -3
git tag -l | grep p1-complete
```

---

## Self-Review (skill checklist)

**1. Spec coverage:** Every P1 task from the master plan has a corresponding numbered task here (P1.1 → P1.13). All five builtin tools have full code blocks. The agent loop, permission engine, session store, and TUI client are all spelled out. Spec mechanisms cited at the top of the document.

**2. Placeholder scan:** No "TBD"/"TODO"/"implement later"/"similar to Task N" sentences. Every code block is complete and paste-ready. The one structural shortcut — a hard-coded `match meta.name` dispatch table in `agent.rs` — is documented as a P1 simplification with a forward reference to the proc-macro upgrade in later phases. (This is an explicit, justified pragma, not a placeholder.)

**3. Type consistency:**
- `ToolMeta` shape used identically in macros, registry, permission, agent dispatch.
- `Block` variants (`Text`, `ToolUse`, `ToolResult`, `Thinking`) from `origin-core` referenced consistently in provider, agent, session_store.
- `Role` enum used the same way everywhere (`User`/`Assistant`/`Tool`/`System`; tool results coded as `Role::Tool` then mapped to Anthropic's "user" role at the wire boundary in `provider-anthropic::message_to_wire`).
- `Tier` enum (`AutoAllowed` / `RequiresPermission`), `Urgency` (`Low`/`Medium`/`High`), `SideEffects` (`Pure`/`Mutating`) consistent across `origin-tools` and `origin-permission`.
- `LoopOptions { max_turns: u32 }` and `LoopSummary { assistant_text, turns }` shape consistent across `agent.rs`, `loop_e2e.rs`, and daemon main.
- `PromptRequest`/`PromptReply` IPC types live in `origin-daemon::protocol` and are mirrored in CLI (small local copies; in a later phase we'd extract a shared `origin-protocol` crate, but P1 doesn't justify it).

No issues found.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-19-origin-phase-1.md`.**

Per your instruction, execution will use **superpowers:subagent-driven-development** with **superpowers:test-driven-development** discipline inside each task, and a **superpowers:verification-before-completion** gate at the end of every task. No task advances until verification is fully green.

Start it with `/subagent-driven-development start with P1.1` once you're ready. Tasks are labeled `P1.1`–`P1.13` and tracked via `- [ ]` checkboxes in this file.
