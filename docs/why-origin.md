# Why origin

A pitch / comparison doc covering (1) what problem `origin` solves and how it
differentiates from Claude Code, jcode, and opencode, and (2) how its workflow
system layers on top.

---

## Part 1 — Problem & differentiators

### What origin is

A Rust-native agentic coding harness — a CLI + supervised daemon that runs
LLM-driven coding sessions locally, on the same shape of "model + tools +
skills + hooks + permissions" as Claude Code, jcode, and opencode. It's a
Cargo workspace of 30+ crates with a clean split: `origin-daemon` hosts
sessions, `origin-cli` is a thin client, and they only talk through
`origin-ipc`.

### The problem it solves

Existing TypeScript/Node-based agentic harnesses (Claude Code, opencode, etc.)
are fine for prompting but leak in three places once you actually live in one
all day:

1. **Cold start and keystroke latency** — Node bootstraps a V8 isolate per
   invocation and re-parses skills/config every time.
2. **Memory growth** — long sessions balloon RSS because tool outputs,
   embeddings, and conversation IR are repeatedly serialized/deserialized
   through JSON.
3. **Provider lock-in and fragile auth** — most harnesses bind tightly to one
   vendor's wire format and stash tokens in plaintext dotfiles.

`origin` treats these as first-class engineering gates, not afterthoughts.

### Concrete differentiators (vs. Claude Code / opencode / jcode)

**Perf-as-gate, not aspiration.** Four KPIs (cold start, keystroke-to-pixel,
steady RSS, cache hit rate) are CI gates — the perf-gate workflow asserts
read-only tasks ≤ 80 ms wall time. No other harness in this space enforces
that in CI.

**Two-runtime daemon.** Control plane is a `current_thread` Tokio pinned to
one OS thread; workers run on `multi_thread`. A clippy-enforced
`spawn_in(class, fut)` helper with task classes
(Critical/Realtime/Sidecar/Background/Bulk) makes it impossible for a tool
exec or sidecar job to starve the renderer tick or IPC accept loop.

**Content-addressed everything (`origin-cas`).** Tool outputs, file reads,
embeddings, memory bodies, code-graph nodes — all deduped across turns,
sessions, *and* swarm workers. Storage is one namespace with Hot LRU / Warm
mmap / Cold zstd tiers, auto-promoted by access frequency. Re-importing your
`~/.claude` twice produces zero new bytes the second time.

**`rkyv`-archived IR end-to-end.** The same byte buffer flows through IPC,
SQLite blob columns, and ring buffers — ~200 ns to validate vs. ~20 µs to
JSON-decode. No serialize/deserialize hops on the hot path.

**~40 providers behind one catalog.** Anthropic, OpenAI (API + Codex OAuth),
Gemini (API + OAuth), Bedrock (SigV4), Ollama, GitHub Copilot (device flow),
plus a single `openai-compat` crate driving OpenRouter, DeepSeek, xAI,
Mistral, Moonshot, Qwen, Z.AI, vLLM, SGLang, LiteLLM, Vercel/Cloudflare
gateways, etc. The `CachePlanner` auto-disables cache markers per provider
when `cache_read_input_tokens` stays zero — adapts without config.

**Embedding-indexed lazy skill injection.** Claude Code loads every skill
into the system prompt at session start. `origin-skills` indexes
`(name + description + first-line-of-body)` into the same HNSW graph as
`origin-mem` and only materializes the top-K per turn.
**500 installed skills, zero session-start scan cost.**

**Hooks with a pre-spawned shell pool.** Dispatch latency drops from
5–200 ms (naive `spawn(/bin/sh)`) to ~200 µs by reusing pool workers — makes
`pre_prompt` hooks actually usable on every keystroke.

**KeyVault as the only crate that touches secrets.** OS-native (Credential
Manager / Keychain / Secret Service, with age-encrypted file fallback).
`Secret<T>` newtypes redact in `Debug`; a CI lint rejects any
`*key*`/`*token*`/`*password*` field emitting raw bytes through `tracing`.
Separate 30-day audit ring for every keychain access.

**Sandboxing per platform.** landlock + seccomp + namespaces on Linux,
`sandbox-exec` on macOS, AppContainer on Windows. Skills' `allowed-tools`
narrow the sandbox profile, not just the prompt — a skill that omits `Bash`
*can't* shell out.

**Migration in.** `origin import claude-code|jcode|opencode` is idempotent
(FastCDC + content-hash dedupe). You can run it weekly without duplicates.

**Browser router with bot-detection fallback.** Primary backend is
`agent-browser`; on Cloudflare / reCAPTCHA / hCaptcha / DataDome / 4xx it
transparently re-plays against a vendored `CloakBrowser` sidecar and sticks
to Cloak after two successes in a session.

**Remote IPC via QUIC + mTLS.** Self-signed Ed25519 certs with SHA-256
fingerprint pinning (no PKI). 6-digit single-use pairing codes mint
`orb_`-prefixed bearer tokens. `origin pair start` / `origin pair redeem` —
no other harness ships first-class remote.

**Deterministic replay.** `.origin-replay` bundles (zstd-tar) record
provider/IPC/CAS/clock/RNG frames with virtual clock + seeded SplitMix64.
Used for the offline `origin --tutorial`, regression tests, and bug reports.

**Zero `unsafe` in surface crates.** Workspace-wide
`unsafe_code = "forbid"`, overridden only in three audited crates
(`cas`, `tui`, `ipc`). CI workflow asserts this.

### Where to be honest

A few of these are "real but young" — the embeddings/memory pipeline ships
incrementally (Phase 6), `origin import` apply-mode still needs Store-handle
threading (1.0.x patch), and the fuzz crate is currently nightly-only because
of an `edition2024` transitive. But the bones — CAS, IR, two-runtime daemon,
planner, provider catalog, KeyVault, sandbox, replay — are landed and gated.

### TL;DR for a pitch

> "Claude Code's UX, opencode's openness, with a Rust daemon that treats cold
> start, latency, and RSS as CI gates, content-addresses every byte you've
> ever seen, and supports ~40 providers and remote pairing out of the box."

---

## Part 2 — Workflows

There are **two distinct workflow concepts** in the codebase — both real,
both shipped.

### 1. The default workflow (system-prompt level)

Lives in `crates/origin-daemon/src/default_workflow.rs:9`. A directive
prepended to every system prompt that tells the model to follow a
**brainstorm → plan → dispatch** flow without being asked:

1. `/brainstorming` first — clarify scope; dispatch Task subagents in
   parallel for WebFetch/WebSearch on unknowns.
2. `/writing-plans` next — produce a step-by-step plan with file paths, code
   per step, and a verification command per step. Save to
   `docs/superpowers/plans/`. Wait for approval.
3. `/dispatching-parallel-agents` to execute — one subagent per independent
   unit. Every subagent MUST run `/test-driven-development` (red → green) and
   `/verification-before-completion` (paste the verification output before
   claiming success).

Trivial requests (single lookups, one-line edits) bypass the flow. Disable
globally with `ORIGIN_DEFAULT_WORKFLOW=off`.

**Why this is unusual:** Claude Code, opencode, and jcode are reactive —
they do what you ask. Origin ships an opinionated *baseline orchestration*
baked into the daemon. The user gets TDD + planning + parallel dispatch by
default, not after wiring it up themselves.

### 2. User-defined workflows (`~/.origin/workflows.toml`)

Lives in `crates/origin-cli/src/workflows.rs` (storage) and
`crates/origin-daemon/src/workflows.rs` (loader). A workflow is a **named,
declarative chain of skills**:

```toml
schema_version = 1

[[workflows]]
name = "frontend-design"
description = "Two-step UI feature build: shape with frontend-design, then teach impeccable."

[[workflows.steps]]
skill = "frontend-design:frontend-design"

[[workflows.steps]]
skill = "impeccable"
args = "teach"
```

**Invocation surface:**

- User types `{workflow:frontend-design}` in the TUI.
- `input.rs:154 parse_workflow_command` recognizes the shape.
- CLI sends `ClientMessage::ActivateWorkflow { name }` over IPC.
- Daemon reloads `workflows.toml` fresh (so edits land without restart),
  activates the **first resolvable step's** skill, and replies with
  `StreamEvent::WorkflowStepActive { name, step_index, total_steps,
  skill, skipped }`. After each successful prompt the daemon's
  `workflow_progress` state machine advances one step at a time —
  deactivating the prior step's skill and activating the next — until
  it emits `StreamEvent::WorkflowComplete`.

**Autocomplete is wired:** `autocomplete.rs:43` handles `{workflow:<partial>`
and tab-completes against the names actually in your `workflows.toml`.

**Onboarding seeds it:** `welcome.rs:297 screen_workflows` is screen 4 of
post-init, and `workflows::seed_if_missing` writes the example workflow
above so the format is discoverable.

**Partial activation is a feature, not a bug:** if step 3 references a
skill you haven't installed, steps 1, 2, 4… still activate and the missing
one comes back in `skipped` — one ack frame, no multi-frame error loop.

### How this stacks against other harnesses

| Capability | Claude Code | opencode | jcode | **origin** |
|---|---|---|---|---|
| Skills (single-shot) | ✅ | ✅ | ✅ | ✅ |
| Default agent orchestration baked into system prompt | ❌ | ❌ | ❌ | ✅ |
| Declarative skill chains (`workflows.toml`) | ❌ | partial (`commands`) | ❌ | ✅ |
| Hot-reload on user edit | n/a | restart | n/a | ✅ (load-on-activate) |
| Tab-complete from on-disk file | ❌ | ❌ | ❌ | ✅ |
| Partial activation w/ skipped reporting | n/a | n/a | n/a | ✅ |
| Step-by-step gating (one skill active per prompt) | ❌ | ❌ | ❌ | ✅ |

### Where it's genuinely young

- The daemon's `workflows.rs` is a deliberate duplicate of the CLI's; a
  comment flags `origin-workflows` as a follow-up crate.
- Workflow steps can't yet carry their own permission tier or sandbox
  profile — they inherit from the skill they reference.

### TL;DR

Origin treats workflow orchestration as a **first-class daemon feature on
two layers**: a baked-in default (brainstorm → plan → dispatch → TDD →
verify) that ships in every system prompt, plus user-defined declarative
chains in `workflows.toml` with hot-reload, autocomplete, and
partial-activation reporting. Other harnesses leave that to the model to
figure out per session.
