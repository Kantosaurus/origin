# Configuration

`origin` reads its configuration from a small set of TOML files under
`~/.origin/`, plus a handful of environment variables. Secrets never live in
these files — they are kept in your OS keychain by `origin-keyvault`. This guide
covers where the files are, what each setting does, the environment variables
`origin` honours, and how settings combine when more than one applies.

> All of the on-disk config paths honour the `ORIGIN_HOME` environment variable.
> When it is set, `<ORIGIN_HOME>/.origin/...` replaces `~/.origin/...`
> everywhere below. This is the supported way to run an isolated install.

---

## Config files at a glance

| File | Format | Written by | Purpose |
| --- | --- | --- | --- |
| `~/.origin/config.toml` | TOML | `origin init` | Provider/model role mapping + model aliases. |
| `~/.origin/governance.toml` | TOML | by hand | Permissions, policy layers, post-edit, notifications. |
| `~/.origin/providers.toml` | TOML | by hand | Custom provider catalog entries (see [providers-setup](providers-setup.md)). |
| `~/.origin/workflows.toml` | TOML | `origin workflow author` | Authored workflows. |
| `~/.origin/schedule.toml` | TOML | `origin schedule add` | Recurring triggers + `[profiles.*]`. |
| OS keychain | — | `origin init` / `origin keyring` | All secrets. |

---

## The main config file: `config.toml`

`~/.origin/config.toml` holds the role → (provider, account, model) mapping
captured by onboarding. It is **non-secret** — safe to commit to a dotfiles
repo. The file is schema-versioned (current `schema_version = 1`); a file
declaring a newer version than the binary supports is rejected at load rather
than silently misread.

A complete example with all three roles and an alias table:

```toml
schema_version = 1

[primary]
provider = "anthropic"
account = "default"
model = "claude-fable-5"

[backup]
provider = "openai"
account = "default"
model = "gpt-4o"

[subagent]
provider = "ollama"
account = "default"
model = "llama3.2"

[aliases]
fast = "anthropic/claude-haiku-4"
o = "gpt-4o"
```

### Role settings

| Key | Required? | Meaning |
| --- | --- | --- |
| `[primary]` | **yes** | The main provider/model the agent loop talks to. |
| `[backup]` | no | A fallback provider used when the primary errors. Omitted when unset. |
| `[subagent]` | no | A separate provider/model dedicated to subagent and swarm workers, so heavy parallel work can flow to a cheaper or faster model without disturbing the main turn. Omitted when unset. |

Each role table has three fields: `provider` (a catalog id such as `anthropic`),
`account` (the keychain account the secret is filed under — typically
`"default"`), and `model` (a model id).

### Model aliases (`[aliases]`)

The optional `[aliases]` table maps a short name to a model target — either
`"provider/model"` or a bare model id. When a requested model string exactly
matches an alias key, `origin` substitutes the target before sending the model
to the daemon. Resolution is **one hop only** (not transitive), so an alias
whose target is itself another alias name resolves to the literal target. An
empty table is omitted from the file entirely.

You can also define aliases ad hoc for a single invocation:

```sh
origin run --alias fast=anthropic/claude-haiku-4 "quick summary please"
```

Ad-hoc `--alias` entries are merged on top of the config `[aliases]` and win on
a name clash.

### How `config.toml` is written

`origin init` (and the interactive onboarding flow) write this file. The write
is atomic — `origin` writes to a `.toml.tmp` sibling and renames — so a crash
mid-write can't leave a half-written `config.toml`. Re-running `origin init`
overwrites the file; use `origin keyring` to change only credentials.

---

## Governance: permissions, policy, post-edit, notify

`~/.origin/governance.toml` is optional. When it is absent (the default),
`origin` behaves exactly as if no policy were configured. Each section is
independent; an absent or empty section contributes nothing. Unknown top-level
keys are **rejected** (so a typo'd section name can't silently disable
governance). Its path can be overridden with `ORIGIN_GOVERNANCE_PATH`.

### Permission rules

The simplest governance: pre-answer a tool-permission prompt. Each
`[[permission_rules]]` entry names a canonical tool and says whether a match
auto-approves or blocks, before the interactive prompter is consulted.

```toml
[[permission_rules]]
tool = "Bash"
allow = false        # block Bash outright

[[permission_rules]]
tool = "Read"
scope = "*"          # "*" (the default) = every session
allow = true         # auto-approve reads
```

| Key | Meaning |
| --- | --- |
| `tool` | Canonical tool name (`"Bash"`, `"Read"`, `"Write"`, …). |
| `scope` | Session scope the rule applies at; defaults to `"*"` (all sessions). |
| `allow` | `true` ⇒ auto-approve, `false` ⇒ block. |

### Policy layers

For multi-tier governance (e.g. an org-managed deny on top of user allows),
stack `[[policy_layers]]`. Each carries a `tier` and the flattened policy data
(`allowed_tools`, `denied_tools`, `max_spend_usd`, …). The five tiers, in
precedence order, are `user` → `project` → `managed` → `admin` → `system`; a
deny at a higher tier is final.

```toml
[[policy_layers]]
tier = "admin"
denied_tools = ["Bash"]
max_spend_usd = 5.0

[[policy_layers]]
tier = "user"
allowed_tools = ["Bash", "Read"]
```

A negative or non-finite `max_spend_usd` is rejected at load.

### Per-prompt security (`[conseca]`)

```toml
[conseca]
allow_tools = ["Read"]
rationale = "read-only session"
```

When present, this `ConSeca` security policy is applied to every loop. (Field
names mirror `origin_conseca::SecurityPolicy`.)

### Browser action cap (`[browser]`)

```toml
[browser]
max_actions_per_session = 5   # cap on Browser/WebFetch/WebSearch per run
```

Omitted ⇒ unlimited.

### Post-edit hooks (`[post_edit]`)

Run a formatter / linter / test suite after each successful edit, with a bounded
auto-repair budget.

```toml
[post_edit]
auto_lint = true
lint_command = "cargo clippy"
auto_test = false
max_repair_iters = 2          # default is 2

[post_edit.format_overrides]
rs = "leptosfmt"              # overrides the builtin formatter table for .rs
```

When `[post_edit]` is absent, `origin` consults only its builtin formatter table
(e.g. `gofmt` for `.go`) and runs no lint/test.

### Notifications (`[notify]`)

```toml
[notify]
quiet_start = 1380            # minutes since midnight (23:00)
quiet_end = 420              # minutes since midnight (07:00)
channel = "webhook"          # "desktop" (default) | "webhook" | "command"
webhook_url = "https://example.test/hook"
# For channel = "command":
# command_program = "notify-send"
# command_args = ["origin done"]
```

A quiet-hours window is built only when both `quiet_start` and `quiet_end` are
set; inside it, non-urgent completion notifications are suppressed. With no
`[notify]` section, the desktop toast fires unconditionally under
`ORIGIN_NOTIFY=1`.

---

## Environment variables

| Variable | Used for | Notes |
| --- | --- | --- |
| `ORIGIN_HOME` | Root for all `~/.origin/...` paths | Overrides the home directory; used by tests and isolated installs. |
| `ORIGINX_NO_UPDATE` | Disable npm auto-update | Set to `1` to stop the once/day background update check. |
| `ANTHROPIC_API_KEY` *(inferred)* | Anthropic provider key | Standard provider key; prefer storing it via `origin init` / `origin keyring`. Treat env-var pickup as **inferred** — the canonical source is the keychain. |
| `TAVILY_API_KEY` *(inferred)* | `WebSearch` (Tavily) | `origin init` stores the key in the vault under `tavily:default`; the env var is the conventional fallback. |
| `LITELLM_API_KEY` | LiteLLM proxy master key | Bearer token for the `litellm` gateway provider. |
| `ORIGIN_DB` | SQLite store path | Where sessions and migrated artifacts live; defaults to a temp-dir fallback. |
| `ORIGIN_MODEL` | Default model for `origin review --llm` | Resolves the model for the LLM review pass. |
| `ORIGIN_GOVERNANCE_PATH` | Override `governance.toml` path | — |
| `ORIGIN_BEARER_TTL_SECS` | Pairing bearer TTL (seconds) | Defaults to one day; non-numeric overrides ignored. |
| `ORIGIN_NOTIFY` | Enable desktop completion toast | Set to `1`; the `[notify]` section refines it. |
| `ORIGIN_LSP_AUTO` | `origin lsp ensure` extra output | Set to `1` to also print the launch the daemon would perform. |
| `ORIGIN_SELFDEV` | Gate `origin selfdev` | Supervised self-development is gated behind `ORIGIN_SELFDEV=1`. |
| `LC_ALL` / `LANG` | UI locale fallback | The `--lang` flag takes precedence when set. |

> **Marked inferred:** the precise env-var names for **provider API keys**
> (e.g. `ANTHROPIC_API_KEY`, `TAVILY_API_KEY`) as a *fallback resolution path*
> are documented as inferred. The verified, canonical credential source in
> `origin` is the OS keychain (populated by `origin init` / `origin keyring`);
> the catalog auth schemes describe *header shapes*, not env-var fallbacks.

---

## Precedence

When more than one source could decide a setting, `origin` resolves in this
order (highest wins):

1. **Explicit CLI flags** for the current invocation — e.g. `--model`,
   `--effort`, `--alias`, `--lang`. Ad-hoc `--alias` beats the config
   `[aliases]` table.
2. **User instructions / user config** — your `~/.origin/config.toml` and
   `user`-tier policy.
3. **Managed / org policy** — higher governance tiers (`managed`, `admin`,
   `system`). A *deny* at a higher tier is final and cannot be re-allowed below
   it.
4. **Built-in defaults** — the catalog default model, the builtin formatter
   table, unconditional desktop toast, etc.

In short: **user instructions take effect over managed policy where the policy
permits, and a higher-tier deny always wins over a lower-tier allow.** Anything
you don't configure falls through to the byte-identical built-in default.

---

## Opting out of auto-update

The npm distribution checks for updates roughly once a day in the background.
Turn it off by exporting:

```sh
export ORIGINX_NO_UPDATE=1
```

Add it to your shell profile to make it permanent. This affects both global and
project-local npm installs.

---

See **[Providers setup](providers-setup.md)** for `providers.toml` and
credential details, and the
[provider subsystem reference](../subsystems/providers.md) for the catalog
internals.

_Last reviewed against workspace version 0.9.8._
