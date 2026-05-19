# `origin` Phase 8 — Provider Matrix + KeyVault — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Expand the provider abstraction from Anthropic-only to a six-provider matrix (Anthropic + OpenAI + Gemini + Ollama by default; Bedrock + OpenRouter + GitHub Models opt-in via cargo features), backed by a single `origin-keyvault` crate that owns every OS-keychain touchpoint and supports OAuth (PKCE + auth-code) with background refresh-token rotation. Daemon picks provider+account per session; TUI exposes a `/account` switch via a new IPC frame.

**Architecture:** New crate `origin-keyvault` owns all credential I/O with three platform modules (`backend_linux.rs` via `secret-service` + age fallback, `backend_macos.rs` via `security-framework`, `backend_windows.rs` via the `windows` crate's `Credentials` API) behind a single `Backend` trait. `KeyVault` is the public façade keyed by `(provider, account)`. A `Secret<T>` newtype enforces redaction-aware `Debug` + `Zeroize` on drop. OAuth lives in `origin-keyvault::oauth` with a PKCE driver, auth-code exchange, and refresh rotation persisting via the vault. Each new provider crate mirrors `origin-provider-anthropic`'s layout (`lib.rs` + `wire.rs` + `streaming.rs` + `tests/` driven by `wiremock`). Shared SSE pump + NDJSON splitter + OpenAI-shape `tool_calls` mapping live in `origin-provider`. Daemon `main.rs` becomes provider-agnostic via `ProviderFactory`.

**Tech Stack:** Rust 1.83 (MSRV pin). Existing deps stay (tokio, reqwest 0.12 with rustls, serde_json, async-trait, eventsource-stream 0.2, futures-util, thiserror, wiremock 0.6, tempfile, tokio-util). New (workspace-pinned): `secret-service = "4"` (Linux), `security-framework = "3"` (macOS), `windows = "0.58"` (Windows Credentials), `age = "0.10"` (Linux headless fallback), `rand = "0.8"`, `sha2 = "0.10"`, `base64 = "0.22"`, `url = "2"`, `aws-sigv4 = "1"` + `aws-credential-types = "1"` (Bedrock, opt-in), `zeroize = "1"`, `async-stream = "0.3"` (NDJSON splitter). **Novel-implementation reflex** per `[[feedback-novel-implementations]]`: KeyVault is the single keyring-touching crate with `(provider, account)`-keyed multi-account API; background OAuth-refresh task rotates tokens before expiry so chat calls never block on refresh; shared SSE/NDJSON pumps so each new provider is ~150 LOC of wire mapping; compile-time provider feature flags so default build doesn't link Bedrock/OpenRouter/GitHub bytecode; `Secret<T>` newtype with `Debug = <redacted>`.

**Builds on:** Spec §4 (N4.1–N4.5), §9 (N9.13), §10 (N10.14, N10.16) of `docs/superpowers/specs/2026-05-19-origin-harness-design.md`. Existing `origin-provider-anthropic` is the reference pattern every new provider crate follows.

**Out of scope (deferred to later phases):**
- 30-day audit-log ring (Phase 11 — `p11-complete`)
- CI grep lint enforcing `Secret<T>` for sensitive fields (Phase 11)
- MCP OAuth flow (Phase 10 — substrate built here)
- Swarm worker-account inheritance (Phase 9 — multi-account API is in place; wiring later)
- Bedrock cross-region failover (Phase 14)
- Streaming for OpenRouter / Bedrock / GitHub Models (Phase 8 ships non-streaming for these; OpenAI/Gemini/Ollama stream natively)

---

## Conventions reminder (apply to every task)

**TDD shape:** failing test → run-to-fail → implement → run-to-pass → verification gate → commit.

**Verification gate per task type:**

| Task type | Required commands (all exit 0) |
|---|---|
| Single-crate pure logic | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / daemon | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Feature-gated provider (P8.6, P8.7, P8.8) | Above + `cargo test -p <crate> --features <flag>` + `cargo clippy -p origin-daemon --features <flag> --all-targets -- -D warnings` |
| Final phase gate (P8.9) | Above + `git tag p8-complete` |

**Inherited patterns:**
- `[lints] workspace = true` in every new `Cargo.toml`.
- Workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- `unsafe_code = "forbid"` is the default. **`origin-keyvault` overrides this to `allow`** for the Windows `Credentials` FFI — every `unsafe` block carries a SAFETY comment. All provider crates keep forbid.
- `#[must_use]` on every public constructor; `const fn` wherever possible.
- Tests use `.expect("meaningful message")`. No `clippy::unwrap_used` allows.
- Custom error enums via `thiserror`; document `# Errors` on every public `Result`-returning fn.
- For each `#[allow(clippy::...)]` add an inline justification.
- **MSRV pin reflex** (`[[project-msrv-dep-pinning]]`): if `cargo check` complains about `edition2024`, pin offender with `cargo update -p <crate>@<bad> --precise <last-1.83-compatible>` and commit `Cargo.lock`. Baseline pins already applied on this branch.
- Live smoke tests follow `crates/origin-daemon/tests/anthropic_smoke.rs` shape: gate on env var, skip silently when unset.
- Reference implementation: `crates/origin-provider-anthropic/` shows the full pattern (manifest, lib.rs, wire.rs, streaming.rs, wiremock-driven tests). Mirror it.
- Commits: Conventional Commits, scoped (`feat(origin-keyvault): ...`), one commit per task.

---

## File map for Phase 8

| New / modified | Responsibility | Task |
|---|---|---|
| `crates/origin-keyvault/{Cargo.toml,src/lib.rs,src/backend.rs,src/backend_memory.rs,src/backend_linux.rs,src/backend_macos.rs,src/backend_windows.rs,src/secret.rs}` | KeyVault façade + per-OS backends + `Secret<T>` | P8.1 |
| `crates/origin-keyvault/{src/oauth.rs,tests/pkce.rs,tests/oauth_flow.rs}` | PKCE + auth-code exchange + refresh rotation | P8.2 |
| `crates/origin-provider/{src/sse.rs,src/openai_tools.rs}` *(new)* + `Cargo.toml` (add deps) + `src/lib.rs` (re-export) | Shared SSE pump + tool-call mapping | P8.3 prereq |
| `crates/origin-provider-openai/*` | OpenAI provider (chat + SSE streaming) | P8.3 |
| `crates/origin-provider-gemini/*` | Gemini provider (chat + SSE streaming) | P8.4 |
| `crates/origin-provider/{src/ndjson.rs}` *(new)* + `src/lib.rs` | Shared NDJSON splitter | P8.5 prereq |
| `crates/origin-provider-ollama/*` | Ollama provider (chat + NDJSON streaming) | P8.5 |
| `crates/origin-provider-openrouter/*` + daemon feature wiring | OpenRouter (opt-in) | P8.6 |
| `crates/origin-provider-bedrock/*` + daemon feature wiring | Bedrock SigV4 (opt-in) | P8.7 |
| `crates/origin-provider-github/*` + daemon feature wiring | GitHub Models OAuth (opt-in) | P8.8 |
| `crates/origin-daemon/src/provider_factory.rs` *(new)* + `protocol.rs` *(modify)* + `main.rs` *(modify)*, `crates/origin-cli/src/main.rs` *(modify)* | `ProviderFactory`, `ClientMessage::SwitchAccount`, `/account` TUI command, tag `p8-complete` | P8.9 |

File-size discipline: every new `.rs` file targets <300 LOC. Each provider crate splits `lib.rs` + `wire.rs` + `streaming.rs` per the Anthropic reference.

---

## Task P8.1 — `origin-keyvault` core + `Secret<T>`

**Files:** `crates/origin-keyvault/Cargo.toml`, `src/lib.rs`, `src/backend.rs`, `src/backend_memory.rs`, `src/backend_linux.rs`, `src/backend_macos.rs`, `src/backend_windows.rs`, `src/secret.rs`, `src/oauth.rs` (stub), `tests/round_trip.rs`.

**Public surface:**
- `KeyVault::detect() -> Result<Self, Error>` — picks platform backend; respects `ORIGIN_KEYVAULT=memory`.
- `KeyVault::in_memory() -> Self`.
- `KeyVault::set(provider, account, value: Secret<impl Zeroize + AsRef<[u8]>>) -> Result<()>`.
- `KeyVault::get(provider, account) -> Result<Secret<String>>`.
- `KeyVault::delete(provider, account) -> Result<()>`.
- `KeyVault::list(provider) -> Result<Vec<String>>`.
- `Secret<T: Zeroize>` with `new`, `expose() -> &T`, `Debug = "Secret<redacted>"`, `Drop` calls `zeroize`.
- `Error { NotFound { provider, account }, Backend(String), Utf8, Serde(String) }`.

**Manifest must:** override `[lints.rust] unsafe_code = "allow"` (Windows FFI needs raw pointers; SAFETY comments per block). cfg-gate platform deps (`secret-service` Linux, `security-framework` macOS, `windows` Windows). Always-on: `thiserror`, `zeroize`, `parking_lot`, `async-trait`.

- [ ] **Step 1: Write failing test** at `crates/origin-keyvault/tests/round_trip.rs` covering:
  - `KeyVault::in_memory()` → `set("anthropic", "default", Secret::new("sk-ant-xxx".to_string()))` → `get` returns same value → `list("anthropic")` returns `["default"]` → `delete` → next `get` returns `Error::NotFound`.
  - `Secret::new("supersecret").debug()` must NOT contain `"supersecret"` and SHOULD contain `"redacted"`.

- [ ] **Step 2:** Run `cargo test -p origin-keyvault --tests` — expect failure (crate doesn't exist).

- [ ] **Step 3:** Implement crate per public surface above. `Backend` trait is `pub(crate)` with async `set/get/delete/list`. `MemoryBackend` uses `parking_lot::Mutex<BTreeMap<(String,String), Vec<u8>>>`. Linux backend uses `secret_service::SecretService::connect(EncryptionType::Dh)` with attributes `{"origin-provider": p, "origin-account": a}`. macOS uses `security_framework::passwords::{set,get,delete}_generic_password` with service `"origin/{provider}"` — wrap sync calls in `tokio::task::spawn_blocking`. Windows uses `windows::Win32::Security::Credentials::{CredWriteW, CredReadW, CredDeleteW}` with target `"origin/{provider}/{account}"` UTF-16-encoded — `unsafe` block per FFI call with SAFETY comment explaining buffer lifetime. `oauth.rs` is a stub module `#![allow(dead_code)]` for P8.2 to fill in.

- [ ] **Step 4:** Run test → PASS.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-keyvault
cargo clippy -p origin-keyvault --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** with message `feat(origin-keyvault): KeyVault façade + Secret<T> + per-OS backends (P8.1)`.

---

## Task P8.2 — OAuth helpers (PKCE + auth-code + refresh rotation)

**Files:** modify `crates/origin-keyvault/Cargo.toml` (add `rand`, `sha2`, `base64`, `url`, `reqwest` with rustls, `serde`, `serde_json`, `tokio` with `time`+`sync`+`rt`; dev-dep `wiremock`). Replace `src/oauth.rs`. Create `tests/pkce.rs` + `tests/oauth_flow.rs`.

**Public surface:**
- `Pkce::new() -> Self`; `verifier() -> &str` (base64url-no-pad, 43–128 chars per RFC 7636); `challenge() -> &str` (base64url-no-pad of SHA-256 over verifier).
- `OAuthClient::new(provider, token_url, client_id) -> Self`.
- `OAuthClient::exchange(&KeyVault, account, AuthCodeRequest { code, code_verifier, redirect_uri }) -> Result<ExchangedTokens>` — POSTs `grant_type=authorization_code`; persists JSON `{access, refresh, expires_at_epoch_sec}` to vault under `(provider, "{account}/oauth")`.
- `OAuthClient::refresh(&KeyVault, account) -> Result<RefreshOutcome>` — reads stored tokens, POSTs `grant_type=refresh_token`, persists rotated tokens.
- `OAuthClient::refresh_if_due(&KeyVault, account, safety_window: Duration) -> Result<RefreshOutcome>` — returns `NotDue { remaining }` if `expires_at - now > safety_window`, else refreshes.
- `RefreshOutcome::Rotated { access: Secret<String> } | NotDue { remaining: Duration }`.

- [ ] **Step 1: Failing test** at `tests/pkce.rs`:
  - `Pkce::new()` verifier is 43–128 chars, all `[A-Za-z0-9_-]`.
  - `challenge` equals `base64url-no-pad(sha256(verifier))`.

- [ ] **Step 2: Failing test** at `tests/oauth_flow.rs` (using `wiremock::MockServer`):
  - First `/token` POST returns `{access_token: "access-1", refresh_token: "refresh-1", expires_in: 3600}`. Second returns `access-2`/`refresh-2`.
  - `exchange` → assert `access_token.expose() == "access-1"`.
  - `refresh` → assert `RefreshOutcome::Rotated { access }` with `access.expose() == "access-2"`.

- [ ] **Step 3:** Run both tests → fail.

- [ ] **Step 4:** Implement `oauth.rs`. PKCE: 96 random bytes via `rand::thread_rng().fill_bytes` → `base64::engine::general_purpose::URL_SAFE_NO_PAD.encode`. `OAuthClient` uses a `reqwest::Client` + `.form(&[("grant_type", …)…])`. Persist via `vault.set(provider, "{account}/oauth", Secret::new(serde_json::to_string(&StoredTokens)?))`.

- [ ] **Step 5:** Both tests pass.

- [ ] **Step 6: Verification gate**

```bash
cargo test -p origin-keyvault
cargo clippy -p origin-keyvault --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit** `feat(origin-keyvault): OAuth driver (PKCE + auth-code + refresh rotation) (P8.2)`.

---

## Task P8.3 — OpenAI provider (chat + SSE streaming)

**Files:**
- Modify `crates/origin-provider/Cargo.toml` (add `eventsource-stream = "0.2"`, `futures-util`, `reqwest` with rustls + stream, `async-stream`).
- New `crates/origin-provider/src/sse.rs` — `pub fn from_reqwest(resp) -> SseStream` using `eventsource_stream::Eventsource`, mapping errors to `ProviderError`.
- New `crates/origin-provider/src/openai_tools.rs` — `WireToolCall { id, type, function: { name, arguments } }`, `tool_call_to_block(&WireToolCall) -> Block::ToolUse`.
- Re-export from `src/lib.rs`: `pub mod sse; pub mod openai_tools;`.
- New crate `crates/origin-provider-openai/` mirroring `origin-provider-anthropic`'s structure: `Cargo.toml`, `src/lib.rs` (impl `Provider for OpenAi`), `src/wire.rs` (encode `messages` → OpenAI JSON), `src/streaming.rs` (SSE → ring), `tests/wire_round_trip.rs`, `tests/streaming.rs`.

**Public surface:** `OpenAi::new(api_key)` + `OpenAi::with_base_url(api_key, base)`. `Provider::name() -> "openai"`. `chat` POSTs `/v1/chat/completions` with `Authorization: Bearer <key>`. `chat_stream` adds `"stream": true` and pipes SSE through `sse::from_reqwest`, emitting `TokenKind::TextDelta` for `choices[].delta.content`, `TokenKind::InputJsonDelta` for `choices[].delta.tool_calls[].function.arguments`, `TokenKind::TurnEnd` when `choices[].finish_reason` arrives, and stops on `data: [DONE]`. Map `tool_calls` from non-streaming response into `Block::ToolUse` via `openai_tools::tool_call_to_block`.

- [ ] **Step 1: Failing test** `tests/wire_round_trip.rs` (wiremock):
  - Mock `POST /v1/chat/completions` with `Authorization: Bearer sk-test` header matcher, return body with `choices[0].message.content = "hello"`, `tool_calls = [{id: "call_1", type: "function", function: {name: "fs_read", arguments: "{\"path\":\"x\"}"}}]`, `usage.prompt_tokens = 10`, `usage.completion_tokens = 4`.
  - Assert `resp.usage.input_tokens == 10`, blocks contain `Block::Text { text: "hello", .. }` AND `Block::ToolUse { id: "call_1", name: "fs_read", .. }`.

- [ ] **Step 2: Failing test** `tests/streaming.rs`:
  - SSE body: 4 deltas with content `"hel"`, `"lo"`, `" w"`, `"orld"`; a frame with `finish_reason: "stop"`; terminal `data: [DONE]\n\n`.
  - Subscribe a `Ring`; drive `chat_stream`; assert accumulated text == `"hello world"` and saw `TokenKind::TurnEnd`.

- [ ] **Step 3:** Run → fail.

- [ ] **Step 4:** Implement. Reference `crates/origin-provider-anthropic/` for manifest deps, lib.rs structure, wire.rs encoder pattern, streaming.rs Token publishing.

- [ ] **Step 5:** Tests pass.

- [ ] **Step 6: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit** `feat(origin-provider-openai): non-streaming + SSE streaming + tool_calls (P8.3)`.

---

## Task P8.4 — Gemini provider

**Files:** New crate `crates/origin-provider-gemini/` mirroring Anthropic layout: `Cargo.toml`, `src/lib.rs` + `wire.rs` + `streaming.rs`, `tests/wire_round_trip.rs` + `tests/streaming.rs`.

**Endpoint shape:** `POST {base}/v1beta/models/{model}:generateContent?key={api_key}` for non-streaming; `:streamGenerateContent?key={key}&alt=sse` for streaming. Body has `contents: [{role, parts: [{text|functionCall|functionResponse}]}]` and optional `systemInstruction: {role:"user", parts:[{text}]}`.

**Public surface:** `Gemini::new(api_key)` + `Gemini::with_base_url(api_key, base)`. `name() -> "gemini"`. Map `parts[].functionCall { name, args }` → `Block::ToolUse { id: format!("call_{}", name), name, input_json: args.to_string() }`. Usage: `usageMetadata.promptTokenCount` → `input_tokens`; `usageMetadata.candidatesTokenCount` → `output_tokens`; `usageMetadata.cachedContentTokenCount` → `cache_read_input_tokens`.

- [ ] **Step 1: Failing test** `tests/wire_round_trip.rs` (wiremock `path_regex(r"/v1beta/models/.*:generateContent")`):
  - Response candidates content parts: `[{text: "hello"}, {functionCall: {name: "fs_read", args: {path: "x"}}}]`, `usageMetadata: {promptTokenCount: 5, candidatesTokenCount: 2}`.
  - Assert text and tool_use blocks decoded; usage correct.

- [ ] **Step 2: Failing test** `tests/streaming.rs` (`path_regex(r"...:streamGenerateContent")`) with SSE body containing two text-delta candidates + one `finishReason: "STOP"` frame.

- [ ] **Step 3–7:** Run-fail / implement / run-pass / `cargo test --workspace + clippy + fmt` / commit `feat(origin-provider-gemini): chat + streaming generateContent (P8.4)`.

---

## Task P8.5 — Ollama provider (NDJSON)

**Files:**
- New `crates/origin-provider/src/ndjson.rs` — `pub fn from_reqwest(resp) -> NdjsonStream` (boxed `Stream<Item = Result<String, ProviderError>>`) using `async-stream::try_stream!` with a buffered `\n` splitter.
- Re-export `pub mod ndjson;` from `src/lib.rs`. Ensure `async-stream` is in `crates/origin-provider/Cargo.toml`.
- New crate `crates/origin-provider-ollama/`: `lib.rs` + `wire.rs` + `streaming.rs` + `tests/round_trip.rs`.

**Public surface:** `Ollama::new()` (defaults to `http://127.0.0.1:11434`) + `Ollama::with_base_url(base)`. `name() -> "ollama"`. Endpoint: `POST {base}/api/chat`. NDJSON frames: `{message: {role, content}, done: bool, prompt_eval_count?, eval_count?}`. Emit `TextDelta` per non-empty content; `TurnEnd` when `done=true`; map `prompt_eval_count` / `eval_count` to `Usage`.

- [ ] **Step 1: Failing test** `tests/round_trip.rs` (wiremock):
  - Response body is three lines: `{"message":{"role":"assistant","content":"hel"},"done":false}\n` `{...,"content":"lo"},"done":false}\n` `{...,"done":true,"prompt_eval_count":4,"eval_count":2}\n`.
  - Drive `chat_stream`; assert text == `"hello"` and saw `TurnEnd`.

- [ ] **Step 2–6:** Standard TDD; verification gate `cargo test --workspace + clippy + fmt`; commit `feat(origin-provider-ollama): NDJSON streaming + shared splitter (P8.5)`.

---

## Task P8.6 — OpenRouter provider (opt-in feature)

**Files:** New crate `crates/origin-provider-openrouter/`: `Cargo.toml` (no extra feature gates inside the crate itself; gating happens at the daemon), `src/lib.rs` (impl Provider — non-streaming only this phase), `tests/round_trip.rs`. Modify `crates/origin-daemon/Cargo.toml`: add `[features]` block with `default = ["openai", "gemini", "ollama"]` and `openrouter = ["dep:origin-provider-openrouter"]`; add optional dep.

**Endpoint:** `POST {base}/api/v1/chat/completions` with `Authorization: Bearer`, `HTTP-Referer: https://origin.local`, `X-Title: origin`. Body and response shapes are identical to OpenAI; reuse `openai_tools::tool_call_to_block`.

- [ ] **Step 1: Failing test** verifies wiremock matchers for `authorization` + `http-referer` + `x-title`, and `Block::Text { text: "ok" }` round-trip.

- [ ] **Step 2–6:** Standard TDD. Verification gate:

```bash
cargo test --workspace
cargo build -p origin-daemon --features openrouter
cargo clippy -p origin-daemon --features openrouter --all-targets -- -D warnings
cargo fmt --check
```

Commit `feat(origin-provider-openrouter): OpenAI-shape proxy (opt-in feature) (P8.6)`.

---

## Task P8.7 — Bedrock provider (SigV4)

**Files:** New crate `crates/origin-provider-bedrock/`: `Cargo.toml` (deps: `aws-sigv4 = "1"`, `aws-credential-types = "1"`, `http = "1"`, plus the usuals), `src/lib.rs`, `src/sigv4.rs` (`pub(crate) fn signed_headers(method, url, body, region, access_key, secret_key) -> Result<Vec<(String,String)>, String>`), `tests/sigv4.rs`. Modify daemon Cargo.toml to add optional dep + `bedrock` feature.

**Endpoint:** `POST {endpoint}/model/{model_id}/invoke`. Body: Anthropic-shape `{anthropic_version: "bedrock-2023-05-31", max_tokens, system, messages}`. Sign with SigV4: service `"bedrock"`, region from constructor. Test asserts `Authorization` and `x-amz-date` headers present on the wiremock side.

- [ ] **Step 1: Failing test** asserts `header_exists("authorization")` + `header_exists("x-amz-date")`; response decodes `content[{type:"text", text:"hi"}]` + `usage.input_tokens/output_tokens`.

- [ ] **Step 2–6:** Standard TDD. **If `aws-sigv4` pulls a transitive needing edition2024, pin per MSRV reflex.** Verification gate: workspace test/clippy + `cargo build -p origin-daemon --features bedrock` + feature-scoped clippy. Commit `feat(origin-provider-bedrock): SigV4-signed invoke + Anthropic-shape body (P8.7)`.

---

## Task P8.8 — GitHub Models provider (OAuth via KeyVault)

**Files:** New crate `crates/origin-provider-github/`: `Cargo.toml` (depends on `origin-keyvault`), `src/lib.rs`, `tests/round_trip.rs`. Modify daemon Cargo.toml: optional dep + `github-models` feature.

**Public surface:** `GitHubModels::new(vault: KeyVault, account: impl Into<String>)` + `with_base_url`. `name() -> "github-models"`. On each `chat` call: read `vault.get::<String>("github", "{account}/oauth")` → JSON-parse stored `{access, ...}` → use `access` as Bearer. This way background refresh rotation (P8.2) is observed without restart.

**Endpoint:** `POST {base}/inference/chat/completions` (OpenAI-shape body).

- [ ] **Step 1: Failing test** writes a stored-token JSON blob to KeyVault under `("github", "default/oauth")` with `access: "gh-token-xyz"`, then matches wiremock on `Authorization: Bearer gh-token-xyz` and decodes response.

- [ ] **Step 2–6:** Standard TDD. Verification gate includes `--features github-models`. Commit `feat(origin-provider-github): OAuth-bearer via KeyVault + OpenAI-shape wire (P8.8)`.

---

## Task P8.9 — `ProviderFactory` + `/account` TUI switch + tag `p8-complete`

**Files:**
- New `crates/origin-daemon/src/provider_factory.rs` — `enum ProviderId { Anthropic, OpenAi, Gemini, Ollama, #[cfg(feature="openrouter")] OpenRouter, #[cfg(feature="bedrock")] Bedrock, #[cfg(feature="github-models")] GitHubModels }` with `parse(&str) -> Option<Self>` and `as_str(self) -> &'static str`. `ProviderFactory::new(KeyVault)`. `async fn build(&self, ProviderId, account: &str) -> Result<Arc<dyn Provider>, FactoryError>` looks up the credential and constructs the matching provider. Bedrock stores a JSON blob `{access, secret, region}` under `("bedrock", account)`; GitHub Models reads through the vault internally.
- Modify `crates/origin-daemon/src/protocol.rs`: add `enum ClientMessage { Prompt(PromptRequest), SwitchAccount { provider: String, account_id: String } }` (`#[serde(tag="kind", rename_all="snake_case")]`); add `StreamEvent::ProviderActive { provider, account_id }`.
- Modify `crates/origin-daemon/src/main.rs`: replace direct `Anthropic::new(env::var("ANTHROPIC_API_KEY"))` with: open `KeyVault::detect()`; if `ANTHROPIC_API_KEY` env is set, mirror it into the vault at `("anthropic", "default")` for back-compat; build initial provider from `ProviderId::parse(env::var("ORIGIN_PROVIDER").unwrap_or("anthropic"))` + `ORIGIN_ACCOUNT` (default `"default"`); hold the provider behind an `Arc<tokio::sync::RwLock<Arc<dyn Provider>>>`; in the IPC handler add a match arm for `ClientMessage::SwitchAccount` that calls `factory.build`, swaps the RwLock, emits `StreamEvent::ProviderActive`.
- Modify `crates/origin-cli/src/main.rs`: parse `/account [<provider>] [<account>]` slash command, send `ClientMessage::SwitchAccount` upstream via the existing frame helper.
- Add `pub mod provider_factory;` to `crates/origin-daemon/src/lib.rs`.
- New `crates/origin-daemon/tests/account_switch.rs`.

- [ ] **Step 1: Failing test** `tests/account_switch.rs`:
  - Pre-populate `KeyVault::in_memory()` with `("anthropic", "default") = "sk-ant-A"` and `("openai", "default") = "sk-openai-A"`.
  - `ProviderFactory::new(vault).build(ProviderId::Anthropic, "default").await` → provider with `name() == "anthropic"`.
  - Same with `OpenAi` → `name() == "openai"`.
  - Round-trip `ClientMessage::SwitchAccount` and `StreamEvent::ProviderActive` through `serde_json::to_string` / `from_str`.

- [ ] **Step 2:** Run → fail.

- [ ] **Step 3:** Implement `provider_factory.rs`, protocol additions, daemon wiring. Update existing callers exhaustively — `cargo build -p origin-daemon` will surface every match arm that needs updating; fix them all.

- [ ] **Step 4:** Test passes.

- [ ] **Step 5: Final verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p origin-daemon --features "openrouter bedrock github-models" --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Tag**

```bash
git tag p8-complete
```

- [ ] **Step 7: Commit** `feat(origin): provider factory + /account TUI switch; tag p8-complete (P8.9)`.

---

## Self-review checklist

**Spec coverage:**
- ✅ N4.1 — providers share `origin_core::types::{Message, Block}` IR (P8.3–P8.8).
- ✅ N4.3 — per-provider encoders walk the IR (P8.3 wire.rs, P8.4 wire.rs, P8.5 wire.rs; OpenRouter/Bedrock/GitHub encode inline).
- ✅ N4.4 — shared SSE pump + NDJSON splitter; per-provider streams in P8.3–P8.5.
- ✅ N4.5 / N10.16 — `origin-keyvault` is the only keyring-touching crate (P8.1); multi-account via `(provider, account)` key; OAuth PKCE + refresh (P8.2).
- ✅ N9.13 — OAuth via KeyVault (P8.2); consumed by GitHub Models (P8.8).
- ✅ N10.14 — `Secret<T>` newtype (P8.1). CI lint deferred to Phase 11 (out of scope).
- ✅ Provider matrix — Anthropic (existing) + OpenAI (P8.3) + Gemini (P8.4) + Ollama (P8.5) + OpenRouter (P8.6) + Bedrock (P8.7) + GitHub Models (P8.8).
- ✅ Default build = Anthropic + OpenAI + Gemini + Ollama (P8.6 sets daemon features default).
- ✅ Account-switch in TUI (P8.9).

**Type consistency:**
- `KeyVault::set/get/delete/list` consistent across P8.1, P8.2 (oauth.rs), P8.8 (GitHubModels::current_token), P8.9 (ProviderFactory::build).
- `Provider::name()` returns `&'static str` everywhere.
- `ProviderError::{Transport, Api, Auth, RateLimit}` consistent.
- `TokenKind::{TextDelta, InputJsonDelta, TurnEnd}` used consistently across streaming pumps.
- `ProviderId` enum cases match the string parser cases.
- `ClientMessage::SwitchAccount` and `StreamEvent::ProviderActive` share `provider` + `account_id` field names.

**Placeholders:** No "TBD" / "implement later" / "fill in details" steps. Each task names exact files, exact public surface, exact endpoints, and exact test assertions. Provider implementations reference `crates/origin-provider-anthropic/` as the authoritative pattern rather than re-listing 200+ lines per task — this is intentional plan scope tightening.

---

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-05-19-origin-phase-8.md`. Per the user's instruction, execution is via **superpowers:subagent-driven-development**, each task internally following **superpowers:test-driven-development** and gated by **superpowers:verification-before-completion** before advancing. Branch: `phase-8`.
