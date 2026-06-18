# Tool Catalog

Reference for every **builtin** tool compiled into the `origin-tools` crate, plus
the runtime-discovered (MCP) and deferred (`ToolSearch`) surfaces. Each builtin
registers a `ToolMeta` record via the `origin_tool!` macro and the `inventory`
crate; `registry_iter()` enumerates them at runtime.

See also: [`../subsystems/tools.md`](../subsystems/tools.md) ·
[`../crates/origin-tools.md`](../crates/origin-tools.md) ·
[glossary.md](glossary.md) · [environment-variables.md](environment-variables.md)

---

## ToolMeta fields

Every builtin carries this metadata (`crates/origin-tools/src/registry.rs`):

| Field | Type | Meaning |
|-------|------|---------|
| `name` | `&str` | Wire name the model invokes. |
| `description` | `&str` | One-line purpose embedded in the prompt / fetched on demand. |
| `tier` | `Tier` | Permission tier — `AutoAllowed` or `RequiresPermission`. |
| `urgency` | `Urgency` | `Low` / `Medium` / `High` scheduling hint. |
| `side_effects` | `SideEffects` | `Pure` (read-only) or `Mutating`. |
| `input_schema` | `&str` | JSON Schema string for the arguments. |
| `sandbox_profile` | `SandboxProfile` | Confinement applied to child processes (see below). |
| `token_budget` | `u32` | Advisory result-size cap (default `25_000`). |
| `hot` | `bool` | `true` ⇒ full schema in the system prompt; `false` ⇒ deferred behind `ToolSearch`. |

### Permission tiers (`Tier`)

| Tier | Behaviour |
|------|-----------|
| `AutoAllowed` | Runs without a prompt. Read-only or otherwise low-risk. |
| `RequiresPermission` | Gated; in interactive mode the daemon emits `PermissionAsk` and waits for a `PermissionDecision`. In headless/swarm mode the policy engine decides. |

### Sandbox profiles (`SandboxProfile`, from `origin-sandbox`)

| Profile | Confinement |
|---------|-------------|
| `Inherit` | No extra confinement beyond the daemon's own. |
| `ReadFs` | Read-only filesystem view. |
| `WriteCwd` | Writes confined to the workspace / current working tree. |
| `Shell` | Full shell exec profile (network + write), most permissive. |

See [`../crates/origin-sandbox.md`](../crates/origin-sandbox.md) and
[`../security/security-model.md`](../security/security-model.md).

---

## Builtin tools

Hot tools (`hot: true`) always have their schema in the prompt: **Read, Write,
Edit, MultiEdit, Bash, Grep, Glob, Task, Recall, ApplyPatch, Diagnostics**
(among the always-loaded set). Deferred tools advertise only `{name,
description}` and are inflated via `ToolSearch`.

### Filesystem & editing

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `Read` | Read a file; optional `offset`/`limit`, `as: image\|pdf\|text`. | AutoAllowed | `ReadFs` | `file_path`, `offset`, `limit`, `as` |
| `Write` | Create/overwrite a UTF-8 file atomically; refuses overwrite of unread files unless `force`. | RequiresPermission | `WriteCwd` | `file_path`, `content`, `force` |
| `Edit` | Find-and-replace a unique string in a file; CRLF-safe. | RequiresPermission | `WriteCwd` | `file_path`, `old_string`, `new_string`, `replace_all` |
| `MultiEdit` | Apply a sequence of edits to one file atomically (single read + write). | RequiresPermission | `WriteCwd` | `file_path`, `edits[]` |
| `ApplyPatch` | Apply a unified diff or codex/opencode marker envelope across files; all-or-nothing. | RequiresPermission | `WriteCwd` | `patch` |
| `Glob` | Find files matching a glob; results sorted by mtime DESC, gitignore-aware. | AutoAllowed | `Inherit` | `pattern`, `path`, `head_limit` |
| `Grep` | Recursive regex search (`files_with_matches`/`content`/`count`); opt-in `agentgrep:` DSL for outlines/refs. | AutoAllowed | `Inherit` | `pattern`, `path`, `glob`, `type`, `output_mode`, `before`, `after`, `head_limit`, `line_numbers`, `multiline` |

### Shell & process

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `Bash` | Execute a shell command; foreground waits, `run_in_background` returns a pid. | RequiresPermission | `Shell` | `command`, `cwd`, `env`, `timeout`, `run_in_background` |
| `Monitor` | Tail output of a background process started by `Bash{run_in_background:true}`. | AutoAllowed | `Inherit` | `pid`, `since_byte`, `max_bytes`, `wait` |

### Code intelligence (LSP / rust-analyzer)

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `LspNavigate` | Semantic navigation via the warm LSP: `definition`, `references`, `incoming_calls`, `outgoing_calls`. | AutoAllowed | `Inherit` | `op`, `path`, `line`, `col` |
| `Diagnostics` | LSP diagnostics from warm rust-analyzer; severity filter. | AutoAllowed | `Inherit` | `path`, `severity` |

### Code graph

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `graph_query` | Typed code-graph query: `neighbors`/`path`/`communities`/`god_nodes`/`recent_changes`; returns a CAS handle. | AutoAllowed | `Inherit` | `kind`, `…` |
| `graph_path` | Find a path from one code entity to another by id. | AutoAllowed | `Inherit` | `from`, `to`, `max_hops` |
| `graph_summarize` | Summarize a community or node neighborhood; returns CAS-handled bullets. | AutoAllowed | `Inherit` | `community_id` \| `node` |
| `graph_explain` | Run a typed query then route the result through the sidecar with a tight NL template (the only NL-output graph tool). | AutoAllowed | `Inherit` | same as `graph_query` |
| `graph_rebuild` | Rebuild the code graph over `paths` (empty = full repo); async, returns a job handle. | RequiresPermission | `Inherit` | `paths[]` |

### Memory & knowledge

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `mem_search` | Semantic search over cross-session memory; top-k hits with previews. | AutoAllowed | `Inherit` | `query`, `k` |
| `mem_save` | Persist a memory across sessions; optional tags. | RequiresPermission | `Inherit` | `text`, `tags` |
| `mem_forget` | Permanently delete a memory by id. | RequiresPermission | `Inherit` | `id` |
| `ask` | Free-text question; a no-LLM classifier routes to code-graph, memory, or both. | AutoAllowed | `Inherit` | `question` |
| `Recall` | Inflate a CAS handle into the response; optional region (`lines`/`match`/`outline_only`). | AutoAllowed | `Inherit` | `handle`, `region` |

### Web & browser

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `WebFetch` | Fetch one or more URLs and return reader-mode markdown (single `url` or `urls[]` up to 20). | RequiresPermission | `Inherit` | `url`, `urls[]` |
| `WebSearch` | Search the web via Tavily (`TAVILY_API_KEY` or vault). | RequiresPermission | `Inherit` | `query`, `count` |
| `Browser` | Stateful browser with agent-detection fallback to CloakBrowser. | RequiresPermission | `Inherit` | verb + verb args |

### Mail

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `gmail` | Read-only Gmail: `search`, `get` (with `include_body`), `list_threads`. Requires Google creds + permission. | RequiresPermission | `Inherit` | `op`, `query`, `id`, `include_body` |

### Orchestration & workflows

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `Task` | Dispatch a concurrent sub-agent (swarm worker) confined to `allowed_tools`; returns a `CompletionReport`. | AutoAllowed | `Inherit` | `goal`, `allowed_tools[]`, `budget`, `model`, `mcp_servers[]` |
| `AuthorWorkflow` | Author a runnable workflow from an NL `goal`; persists TOML to the user's workflows file. | RequiresPermission | `Inherit` | `goal`, `name` |
| `RunWorkflow` | Run a named workflow; groups steps into dependency layers and fans out one sub-agent per step. | RequiresPermission | `Inherit` | `name` |

### User interaction

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `ask_user` | Put a structured single/multi-select question to the user; returns their choice(s). | AutoAllowed | `Inherit` | `question`, `options[]`, `multi_select`, `allow_custom` |

### Tool discovery

| Tool | Purpose | Tier | Sandbox | Key params |
|------|---------|------|---------|-----------|
| `ToolSearch` | Fetch full schemas for deferred tools — `select:Name,Name` for exact, keyword query to rank. | AutoAllowed | `Inherit` | `query`, `max_results` |

---

## Hot vs deferred

The model's system prompt stays small by embedding only **hot** tool schemas.
Hot builtins observed in the registry include `Task` (`hot: true`) and `Recall`
(`hot: true`) plus the always-loaded edit/read/shell set. Everything else
(`browser`, `gmail`, `graph_*`, `mem_*`, `web_*`, `author_workflow`,
`run_workflow`, `lsp_nav`, `ask`, `ask_user`) advertises only `{name,
description}` and is inflated on demand via `ToolSearch` — keeping the prompt
prefix stable for prompt-cache reuse (see the prompt-cache prefix planner in the
[glossary](glossary.md)).

## MCP-discovered tools

Tools provided by an [MCP](glossary.md) server are not compiled into the
inventory. They are represented at runtime by the `DynTool` trait object
(`crates/origin-tools/src/lib.rs`): `meta()` returns a synthesized `ToolMeta`,
and `invoke(args: Value) -> Result<Value, String>` performs the call. They are
dispatched through the same permission/sandbox path as builtins. See
[`../crates/origin-mcp.md`](../crates/origin-mcp.md).

## Result handling

Large results are content-addressed: a tool may return a **CAS handle** that the
model inflates with `Recall`. Array-heavy JSON is compacted by `SchemaCrush`
(`array_crush`), and the dispatch layer memoizes pure-tool results (see
`MEMOIZATION_SKIPLIST` in `dispatch.rs`). The `token_budget` field is advisory
metadata only — live bounds come from each builtin's own `head_limit` plus
`SchemaCrush` and the dispatch result cache.

---

_Last reviewed against workspace version 0.9.8._
