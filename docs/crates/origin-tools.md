# origin-tools

> Tool registry, macros, and builtin tools for origin

## Purpose

`origin-tools` is the catalogue and runtime of everything the agent can *do*.
A compile-time inventory of `ToolMeta` entries describes each tool's name,
schema, permission tier, side-effect class, and sandbox profile; the `builtins`
module implements ~30 first-party tools (file IO, shell, search, code-graph,
memory, web, MCP). It also owns the cross-cutting machinery that every tool
relies on: per-session memoization, CAS hand-off of large results, schema
crushing, and the child-process supervisor behind `Bash`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `origin_tool!` | macro | Submits a `ToolMeta` into the `inventory` registry; arms cover optional `sandbox`, `token_budget`, and `hot`. |
| `ToolMeta` | struct | Per-tool metadata: `name`, `description`, `tier`, `urgency`, `side_effects`, `input_schema`, `sandbox_profile`, `token_budget`, `hot`. |
| `registry_iter` | fn | Walks all registered `ToolMeta` at runtime. |
| `Tier` / `Urgency` / `SideEffects` | enum | `AutoAllowed`/`RequiresPermission`; `Low`/`Medium`/`High`; `Pure`/`Mutating`. |
| `DynTool` | trait | Runtime tool object (`meta()` + async `invoke`) for tools with no inventory entry — MCP-discovered tools live here. |
| `Cache` / `NormalizedInput` / `CacheHit` | struct | Per-session `(tool, input)` memoization; `MEMOIZATION_SKIPLIST`. |
| `ToolError` / `ErrClass` | enum | Structured tool errors. |
| `SandboxProfile` / `ProfileOrdinal` | re-export | From `origin-sandbox`, so callers build `ToolMeta` without a direct dep. |
| `DEFAULT_TOKEN_BUDGET` | const | `25_000` advisory token cap per serialised result. |

Builtin modules (each registers one or more tools): `apply_patch`, `ask`,
`ask_user`, `author_workflow`, `bash`, `browser`, `diagnostics`, `edit`,
`glob_tool`, `gmail`, `graph_explain`, `graph_path`, `graph_query`,
`graph_rebuild`, `graph_summarize`, `grep_tool`, `lsp_nav`, `mem`, `monitor`,
`multi_edit`, `read`, `recall`, `run_workflow`, `task`, `tool_search`,
`web_fetch`, `web_search`, `write`.

## Key types

```rust
#[derive(Debug)]
pub struct ToolMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub urgency: Urgency,
    pub side_effects: SideEffects,
    pub input_schema: &'static str,
    pub sandbox_profile: SandboxProfile,
    pub token_budget: u32,
    /// "Hot" tools embed their full schema in the system prompt; "deferred"
    /// tools advertise only {name, description} and are fetched via ToolSearch.
    pub hot: bool,
}
inventory::collect!(ToolMeta);

#[async_trait::async_trait]
pub trait DynTool: Send + Sync + std::fmt::Debug {
    fn meta(&self) -> &ToolMeta;
    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, String>;
}
```

## A tour of the builtins

The registry mixes Anthropic-style PascalCase tools with snake_case ones; names
are exactly as registered in `origin_tool! { name: ... }`.

**Filesystem & editing**
- `Read` — read a file with `offset`/`limit`, and `as: text|image|pdf`.
- `Write` — atomic create/overwrite; refuses to clobber unread files.
- `Edit` — unique find-and-replace (CRLF-safe), `replace_all` for many.
- `MultiEdit` — a sequence of edits to one file, single read + single write.
- `ApplyPatch` — unified-diff *or* codex/opencode marker envelopes (Add/Delete/Update/Move), validated atomically before any write.

**Shell & processes**
- `Bash` — `bash_v2` runs under the `Shell` sandbox profile via `proc_supervisor`; supports `timeout` (default 120 s, max 600), `cwd`, `env`, and `run_in_background` (returns a `pid`).
- `Monitor` — tail output of a background process by `pid` with `since_byte`.

**Search & navigation**
- `Glob` — gitignore-aware glob, results sorted mtime-DESC.
- `Grep` — recursive regex (`files_with_matches`/`content`/`count`) plus the `agentgrep:` DSL (`outline:`, `refs:`); backed by `ra_bridge`.
- `LspNavigate` — `definition`/`references`/`incoming_calls`/`outgoing_calls` over the warm language server.
- `Diagnostics` — LSP diagnostics with a severity filter.

**Code graph & memory** — `graph_query`, `graph_path`, `graph_explain`,
`graph_summarize`, `graph_rebuild`, plus `mem_search` / `mem_save` /
`mem_forget` (`mem.rs`) and `Recall` (inflate a CAS handle).

**Web, mail & orchestration** — `WebFetch`, `WebSearch`, `Browser` (router),
`gmail`, `Task` (dispatch a sub-agent), `ask`, `ask_user`, `ToolSearch` (fetch
deferred schemas), `AuthorWorkflow`, `RunWorkflow`.

## How it works

`origin_tool!` runs at compile time, pushing each `ToolMeta` into a global
`inventory::collect!` slice. The daemon calls `registry_iter()` to build the
prompt-visible catalogue: `hot` tools get their full `input_schema` inlined;
deferred tools advertise only name + description and surface their schema via
`ToolSearch`. At call time, dispatch first consults `Cache` keyed on a BLAKE3
`NormalizedInput((tool, raw_input))`; side-effecting tools (`Bash`, `Edit`,
`Write`) sit on `MEMOIZATION_SKIPLIST` and never reuse a cached result. Large
results route through `result_cas`/`array_crush` to stay within budget.

```
origin_tool! ──submit──▶ inventory<ToolMeta> ──registry_iter──▶ prompt catalogue
                                                                     │
agent call ──▶ Cache.lookup(NormalizedInput) ─hit─▶ CAS handle      │ schemas
                     │miss                                          ▼
                     ▼                                          ToolSearch
              builtin/DynTool.invoke ──▶ result ─(crush/CAS)─▶ envelope
```

## Dependencies & features

No cargo features. Depends on `origin-core`, `origin-cas`, `origin-codegraph`,
`origin-plan`, `origin-sandbox`, `origin-swarm`, and `origin-browser`. Search
is built on `grep-matcher`/`grep-regex`/`grep-searcher`/`ignore`/`globset`/
`walkdir`; registration uses `inventory`; hashing uses `blake3`; multimodal
reads use `pdf-extract`/`image`; diffing uses `similar`. Dev-only deps
(`origin-gmail`, `origin-workflowgen`) back schema drift-guard tests only.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-mcp/Cargo.toml
crates/origin-permission/Cargo.toml
crates/origin-tools/Cargo.toml
crates/origin-tui/Cargo.toml
```

## Testing

Unit tests live beside each builtin; `tests/glob_v2.rs` stamps explicit mtimes
(via `filetime`) so the mtime-DESC sort assertion is deterministic. Drift-guard
tests in `builtins/gmail.rs` and `builtins/author_workflow.rs` assert the
inlined `&'static str` schema literals parse to the exact `serde_json::Value`
that `origin-gmail` and `origin-workflowgen` produce. `proptest` and `tempfile`
back property and filesystem tests.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
