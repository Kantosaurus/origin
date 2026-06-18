# origin-permission

> Tier-based permission engine with a pluggable prompter.

## Purpose

`origin-permission` is the gate that decides whether a tool invocation may
proceed. It keys its decision off each tool's `Tier` (from `origin-tools`):
`AutoAllowed` tools pass without interaction, while `RequiresPermission` tools
defer to a pluggable [`Prompter`]. Optional layers add user-configured
allow/deny rules fronted by a growable bloom filter, and an active-skill mask
that narrows the permitted tool set.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Outcome` | enum | `Allow` / `Deny` verdict. |
| `Decision` | struct | `{ outcome: Outcome, reason: String }` returned by every check. |
| `check` | async fn | Tier-only check: auto-allow or ask the prompter. |
| `check_with_rules` | async fn | Bloom pre-check + rule walk, then fall through to `check`. |
| `check_with_skills` | async fn | Enforce a `SkillRegistry` allowed-tools mask before `check`. |
| `prompt::Prompter` | trait | `async ask(meta, args_preview) -> bool`. |
| `prompt::AlwaysAllow` / `AlwaysDeny` | struct | Test prompters. |
| `rules::Rule` | struct | `{ tool_name, scope, allow }` with a canonical `key()`. |
| `bloom::BloomPreCheck` | struct | Growable bloom over the rule set's keys. |

## Key types

```rust
pub enum Outcome { Allow, Deny }

pub struct Decision {
    pub outcome: Outcome,
    pub reason: String,
}

pub async fn check(meta: &ToolMeta, args_preview: &str, prompter: &dyn Prompter) -> Decision;

pub async fn check_with_rules(
    meta: &ToolMeta,
    args_preview: &str,
    prompter: &dyn Prompter,
    scope: &str,
    rules: &[Rule],
    bloom: &BloomPreCheck,
) -> Decision;
```

```rust
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Ask the user to approve a tool invocation. Returns `true` for allow.
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool;
}

pub struct Rule { pub tool_name: String, pub scope: String, pub allow: bool }
impl Rule { pub fn key(&self) -> String { /* "{tool_name}@{scope}" */ } }
```

## How it works

The base `check` is a two-arm match on `meta.tier`:

```text
ToolMeta.tier
   ├── AutoAllowed ──────────────► Decision{ Allow, "tier=AutoAllowed" }
   └── RequiresPermission ──► Prompter::ask ──► Allow ("user-approved")
                                              └► Deny  ("user-denied")
```

`check_with_rules` adds a cheap front door. It builds the canonical key
`"{meta.name}@{scope}"` and consults `BloomPreCheck::maybe_contains` first; a
`false` (definitely-absent) answer skips the rule walk entirely. Only on a
possible-hit does it linearly scan `rules` for an exact `key()` match, where an
explicit `allow`/`deny` short-circuits with `reason = "rule:<name>@<scope>:..."`.
False positives merely cost a few extra hashes and never change correctness.

`check_with_skills` enforces the intersection mask returned by
`SkillRegistry::allowed_tools()`: if any skill is active and `meta.name` is not
in the mask, it denies with `reason = "skill-narrowed"` before any tier logic.

The bloom is built via `GrowableBloom::new(0.01, rules.len().max(64))`, sized
for the actual rule count plus headroom (≈95% rejection on the test mix).

## Dependencies & features

- `origin-tools` (supplies `Tier`, `ToolMeta`), `origin-skills` (supplies
  `SkillRegistry`).
- `async-trait` for the `Prompter` trait, `thiserror`, and
  `growable-bloom-filter` (workspace dep) for the rule pre-check.
- No cargo features; `tokio` is a dev-dependency only.

## Used by

`crates/*/Cargo.toml` matches for `origin-permission`:

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-permission/Cargo.toml`
- `crates/origin-tui/Cargo.toml`

## Testing

In-crate logic plus integration tests under `crates/origin-permission/tests/`:
`bloom.rs`, `check.rs`, `skill_narrow.rs`. These cover the tier verdicts, the
bloom pre-check + rule-walk path, and the skill-mask narrowing
(`reason = "skill-narrowed"`).

## See also

- [Security model](../security/security-model.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
