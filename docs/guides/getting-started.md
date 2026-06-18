# Getting Started

This guide takes you from nothing installed to a working `origin` session in a
few minutes. `origin` is a Rust-native agentic coding harness: a thin CLI that
supervises a local daemon, which in turn runs LLM-driven coding sessions on your
machine. You bring a model provider (or run one locally); `origin` brings the
agent loop, tools, skills, code graph, memory, and permissions.

> New to `origin`? After installing, run the built-in 7-step tour with
> `origin --tutorial` — it's offline and replay-driven, so it costs you no
> provider tokens.

---

## Prerequisites

You only need a working terminal and one way to obtain the binary. The
toolchain matters only if you build from source.

| Requirement | When you need it | Notes |
| --- | --- | --- |
| A provider key or local model | For **live** sessions | e.g. `ANTHROPIC_API_KEY`, an OpenAI key, or a local Ollama/LM Studio server. Captured during `origin init` and stored in your OS keychain. The `--tutorial` tour needs none. |
| Rust **1.83** | Only for the **from-source** build | The pinned toolchain is selected automatically by [`rust-toolchain.toml`](../../rust-toolchain.toml); you do not pick it by hand. The MSRV is 1.83. |
| Node **≥ 18** | Only for the **browser sidecar** | Optional. Powers the `Browser`/`WebFetch` web-automation tools. Everything else works without Node. |
| A Tavily key | Only for `WebSearch` | Free tier (~1,000 searches/month). `origin init` walks you through grabbing one. |

The npm and binstall/brew channels ship a **prebuilt binary**, so you do *not*
need a Rust toolchain unless you are building `origin` yourself.

---

## Install

The installed command is always **`origin`**, regardless of channel.

### npm (fastest — no Rust toolchain)

```sh
npm install -g @kantosaurus/origin   # ships a prebuilt binary; the command is `origin`
origin                               # launches the TUI
```

The npm package is **`@kantosaurus/origin`** (scoped — the unscoped name was
unavailable). It pulls a single small prebuilt binary for your platform, with a
GitHub-release download fallback. It **auto-updates by default** (a background
npm check runs roughly once per day, for both global and project-local
installs). Disable that check by exporting `ORIGINX_NO_UPDATE=1`.

### cargo-binstall (prebuilt via Cargo)

```sh
cargo binstall origin-cli            # prebuilt binary via cargo-binstall
```

### Homebrew

```sh
brew install Kantosaurus/tap/origin  # Homebrew tap
```

### From source (developer build)

This is the path that needs Rust 1.83:

```sh
git clone https://github.com/Kantosaurus/origin.git
cd origin
cargo build --release

# The CLI supervises the daemon for you:
./target/release/origin --help
./target/release/origin            # start an interactive session
```

Alternatively, install the binary onto your `PATH` from a checkout:

```sh
cargo install --path crates/origin-cli
```

---

## First run

The first time you launch `origin` with no config present, it drops you into
**interactive first-time setup** — the same flow you can re-run any time with
`origin init`.

```sh
origin init
```

`origin init` walks you, role by role, through:

1. **Primary provider** — a menu of the full provider catalog, grouped by wire
   format, with an "Other (enter a catalog id)" escape hatch for custom
   providers.
2. **Credential** — the prompt adapts to the provider's auth scheme: paste an
   API key, run an OAuth flow, or enter AWS SigV4 keys. Providers that need no
   auth (Ollama, vLLM, …) skip this step.
3. **Connectivity probe** — `origin` issues a `GET` against the provider's
   `/models` endpoint to verify the credential. On an auth failure it offers a
   retry loop so a typo'd key is easy to fix.
4. **Model** — pick from the probed model list (the catalog default is
   pre-selected) or type a model id.

It then optionally repeats for a **backup** provider (used when the primary
errors) and a **subagent / swarm** provider (so heavy parallel work can flow to
a cheaper or faster model). Finally it captures a **Tavily** key for web search.

When the flow finishes, your non-secret selections are written to
`~/.origin/config.toml`, and your secrets are stored in your OS keychain (never
in the config file). You'll see:

```text
  ✔ Saved to /home/you/.origin/config.toml
  Secrets in OS keychain. Re-run `origin init` or use `origin keyring` to change.
```

> **First-chat skill discovery.** After `init`, the first time the TUI starts it
> auto-fires a one-time prompt that asks the agent to discover and import any
> `SKILL.md` files from other harnesses (`~/.claude/`, `~/.config/opencode/`,
> `~/.cursor/`, …). It runs exactly once and then deletes itself, so it never
> fires twice.

### The guided tour (offline, no tokens)

```sh
origin --tutorial
```

This runs a 7-step interactive tour of the agent loop, code knowledge graph,
cross-session memory, skills, and parallel workers. It is offline and
replay-driven — safe to run before you've even configured a provider.

---

## Your first session

Launch the TUI:

```sh
origin
```

You'll get an interactive session. Try a read-only task first:

```text
List the files in this directory and summarize what this project does.
```

`origin` streams the model's response, parses tool calls, and runs pure
(read-only) tools speculatively. Because tools are content-addressed and cached,
re-asking similar questions is cheap.

A few things worth trying in your first session:

- **Code graph:** `What calls the function handle_request?`
- **Memory:** `Remember that I prefer 2-space indents in Python.` — memories are
  auto-extracted at the end of a turn; you accept or reject them.
- **Skills:** `Use test-driven-development to add a failing test for X.` —
  matching skills are injected automatically.

### One-shot prompts

For scripting or a single question, skip the TUI with `origin run`:

```sh
origin run "Explain what crates/origin-cas does in two sentences."

# Machine-readable output for pipelines:
origin run "List open TODOs" --output-format json
origin run "List open TODOs" --json            # JSON-Lines of every IPC event
```

Useful `run` flags (see `origin run --help` for the full set):

| Flag | Purpose |
| --- | --- |
| `--model <id>` | Override the model for this turn. |
| `--effort <fast\|low\|medium\|high\|max>` | Reasoning-effort level. |
| `--attach <path>` | Attach an image or PDF as multimodal context (repeatable). |
| `--root <dir>` | Extra workspace root the agent may read/edit (repeatable). |
| `--alias name=provider/model` | Define an ad-hoc model alias for this run. |
| `--json-schema <path>` | Force the final answer to satisfy a JSON Schema. |

### Resuming sessions

Every session is persisted. List and resume them:

```sh
origin sessions ls                 # most-recent first
origin sessions resume <id>        # rehydrate the transcript and keep talking
origin --resume <id>               # same, launching the TUI
```

---

## Where state lives

`origin` keeps all of its per-user state under **`~/.origin/`**. You can point
this elsewhere by exporting `ORIGIN_HOME` (handy for tests and alternate-root
installs).

| Path | What it holds |
| --- | --- |
| `~/.origin/config.toml` | Your provider/model role mapping (primary/backup/subagent) + `[aliases]`. Non-secret — safe to check into a dotfiles repo. |
| `~/.origin/governance.toml` | Optional permissions / policy / post-edit / notify config. |
| `~/.origin/providers.toml` | Optional custom provider catalog entries. |
| `~/.origin/skills/` | Your installed skills (`<skill-name>/SKILL.md`). |
| `~/.origin/workflows.toml` | Authored workflows runnable via `origin workflow run`. |
| `~/.origin/schedule.toml` | Scheduled / recurring triggers. |
| `~/.origin/knowledge.json` | The local `/knowledge` semantic index. |
| `~/.origin/models-cache.json` | Discovered runtime model lists. |
| `~/.origin/plugins/` | Installed plugin bundles. |
| `~/.origin/scout/` | Default cache for `origin scout` clones. |
| OS keychain (not a file) | All secrets — API keys, OAuth tokens, SigV4 creds — via `origin-keyvault`. |

Session transcripts and migrated artifacts live in the daemon's SQLite store
(`ORIGIN_DB`, or a temp-dir fallback), **not** under `~/.origin`.

---

## Health check & next steps

Confirm your environment and see exactly what `origin` does over the network:

```sh
origin doctor            # environment + runtime diagnostics
origin doctor --privacy  # only the privacy / phone-home disclosure
origin providers ls      # every provider in the builtin catalog
origin usage             # tokens in/out per provider/model
```

From here:

- **[Configuration](configuration.md)** — the config files, env vars, and
  precedence.
- **[Providers setup](providers-setup.md)** — every provider and how to
  authenticate it.
- **[Authoring skills](authoring-skills.md)** — write your own `SKILL.md`.
- **[Migration](migration.md)** — bring sessions/skills/memories from another
  harness.

_Last reviewed against workspace version 0.9.8._
