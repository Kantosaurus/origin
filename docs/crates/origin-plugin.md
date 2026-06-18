# origin-plugin

> Plugin packaging, manifest parsing, dependency resolution, and live cross-tool skill discovery for origin.

## Purpose

`origin-plugin` handles everything around third-party plugin bundles: it parses a
plugin manifest, resolves install order topologically, estimates the
context-window token cost of a plugin's declared surface, installs a bundle into
the plugins root (defensively, against path traversal), and discovers live
`.claude` and `.agents` skills on disk. The manifest parser understands a
deliberately small TOML subset so the crate stays dependency-light and
MSRV-compatible.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Manifest` | struct | Plugin surface: `commands`/`agents`/`skills`/`hooks`/`mcp`/`lsp`/`deps`. |
| `parse_manifest` | fn | TOML-subset source → `Manifest`; unknown keys tolerated. |
| `resolve_order` | fn | Deterministic topological install order (Kahn's algorithm). |
| `context_cost_estimate` | fn | Kind-weighted token estimate of a manifest's surface. |
| `DiscoveredSkill` | struct | `{ name, path, source }` for a `SKILL.md` found on disk. |
| `discover_skills` | fn | Scan roots' `.claude`/`.agents` skill trees. |
| `validate_manifest_at` | fn | Validate a manifest file or bundle directory. |
| `install_into` | fn | Copy a bundle into `plugins_root/<name>`, idempotently. |
| `MANIFEST_NAMES` | const | `["plugin.toml", "manifest.toml"]` candidate names. |
| `PluginError` | enum | `Toml` / `Cycle` / `Missing` / `Io`. |

## Key types

```rust
pub struct Manifest {
    pub name: String, pub version: String,
    pub commands: Vec<String>, pub agents: Vec<String>, pub skills: Vec<String>,
    pub hooks: Vec<String>, pub mcp: Vec<String>, pub lsp: Vec<String>,
    pub deps: Vec<String>,
}

pub struct DiscoveredSkill { pub name: String, pub path: String, pub source: String }

// Per-kind token weights used by context_cost_estimate:
//   COMMAND 40 · AGENT 120 · SKILL 90 · HOOK 25 · MCP 150 · LSP 60
```

## How it works

```
toml ──parse_manifest──► Manifest ──context_cost_estimate──► u32 tokens
[Manifest] ──resolve_order──► [name] (deps before dependents, ties alphabetical)
src dir ──install_into──► plugins_root/<safe name>   (.git skipped, idempotent)
roots   ──discover_skills──► [DiscoveredSkill]        (.claude + .agents SKILL.md)
```

`parse_manifest` hand-tokenizes a small TOML subset — blank lines, `#` comments,
top-level `key = "string"` and `key = ["a", "b"]` (arrays may span lines).
Unknown keys are ignored so newer manifests stay forward-compatible; type
mismatches and unterminated literals are `PluginError::Toml`.

`resolve_order` runs Kahn's algorithm over `BTreeMap`/`BTreeSet` for deterministic
output: every dependency lands before its dependents, ties break alphabetically.
A dependency on an absent plugin is `PluginError::Missing`; a cycle is
`PluginError::Cycle` (naming the remaining nodes).

`context_cost_estimate` sums declared surface items weighted by kind and is
monotonic (adding any surface item never lowers it); dependencies are excluded
because their cost is attributed to their own manifests.

`install_into` validates first (fail-fast before any filesystem effect), then
copies the tree verbatim — skipping `.git` — into `plugins_root/<name>`,
overwriting any prior install idempotently. The destination name is derived from
the **untrusted** manifest, so it is gated by `is_safe_plugin_name` (single safe
path component, no `..`/separators/leading dot, ≤64 chars) plus a defense-in-depth
check that the destination's parent is exactly the plugins root — a hostile
`name = "../../etc"` is rejected with no side effect.

`discover_skills` scans `<root>/.claude/skills/*/SKILL.md` and the `.agents`
equivalent under each root, returning results sorted by `(source, name, path)`;
a missing directory contributes nothing rather than erroring.

## Dependencies & features

`serde` (Manifest/DiscoveredSkill de/serialization), `thiserror`, and workspace
`walkdir` (skill discovery). `#![forbid(unsafe_code)]`. No Cargo features. The
CLI surfaces this as `origin plugin ls|info|install`.

## Used by

`Grep "origin-plugin" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-plugin/Cargo.toml` (self)

## Testing

Extensive inline `#[cfg(test)]` coverage: full-manifest parsing, type-mismatch
and unterminated-literal rejection, unknown-key tolerance; `resolve_order`
topological chain, cycle detection, and missing-dependency reporting; monotonic
cost estimate (deps excluded); `discover_skills` across both sources plus
missing-root tolerance; `validate_manifest_at` accepting well-formed and
rejecting malformed/nameless/empty dirs; idempotent `install_into` (with `.git`
skip and stale-file cleanup) and the path-traversal-name security regression that
leaves the plugins root untouched; and the `is_safe_plugin_name` allow/deny set.

## See also

- [skills subsystem](../subsystems/skills.md)
- [runtime-and-concurrency architecture](../architecture/runtime-and-concurrency.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
