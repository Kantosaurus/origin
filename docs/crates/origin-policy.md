# origin-policy

> Layered governance / managed-settings engine: RBAC, model allow-lists, spend caps, trusted folders.

## Purpose

`origin-policy` resolves a stack of governance layers into effective decisions.
Five precedence tiers (`System` > `Admin` > `Managed` > `Project` > `User`) each
contribute an optional [`PolicyLayer`] of rules: tool and model allow/deny lists,
a USD spend cap, trusted-folder roots, and an RBAC role. A [`PolicyEngine`]
combines them under fixed resolution rules. The crate is pure logic —
TOML-loadable layers in, decisions out, no I/O or async.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Tier` | enum | `User`/`Project`/`Managed`/`Admin`/`System`; `precedence()`. |
| `PolicyLayer` | struct | One tier's optional rules (TOML-deserializable). |
| `parse_layer` | fn | Parse a TOML layer and tag it with a `Tier`. |
| `PolicyEngine` | struct | Resolves a `Vec<PolicyLayer>` into decisions. |
| `PolicyEngine::is_tool_allowed` / `is_model_allowed` | fn | Allow/deny resolution. |
| `PolicyEngine::spend_cap_usd` / `within_spend` | fn | Effective cap = min across layers. |
| `PolicyEngine::folder_trusted` | fn | Union of trusted roots, segment-aware. |
| `PolicyEngine::effective_role` | fn | Role from the highest-precedence tier. |
| `PolicyError` | enum | `Toml`, `InvalidSpend(f64)`. |

## Key types

```rust
pub enum Tier { User, Project, Managed, Admin, System } // Ord = precedence

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyLayer {
    #[serde(skip)] pub tier: Tier,
    pub allowed_tools:  Option<Vec<String>>,
    pub denied_tools:   Option<Vec<String>>,
    pub allowed_models: Option<Vec<String>>,
    pub denied_models:  Option<Vec<String>>,
    pub max_spend_usd:  Option<f64>,
    pub trusted_folders: Option<Vec<String>>,
    pub role: Option<String>,
}

pub fn parse_layer(toml_src: &str, tier: Tier) -> Result<PolicyLayer, PolicyError>;

pub struct PolicyEngine { /* layers: Vec<PolicyLayer> */ }
impl PolicyEngine {
    pub const fn new(layers: Vec<PolicyLayer>) -> Self;
    pub fn is_tool_allowed(&self, tool: &str) -> bool;
    pub fn spend_cap_usd(&self) -> Option<f64>;
    pub fn folder_trusted(&self, path: &str) -> bool;
    pub fn effective_role(&self) -> Option<String>;
}
```

## How it works

Each layer is independent; order is irrelevant because precedence is taken from
the layer's `Tier`. Resolution rules:

```text
deny           any tier's deny is final and cannot be re-allowed below
allow-lists    INTERSECT across the tiers that set one (most restrictive)
within a layer deny beats allow
spend cap      MINIMUM of every max_spend_usd that is set (within_spend ≤ cap)
trusted folders UNION of all roots; trusted if path == root or nested under it
role           from the HIGHEST-precedence tier that sets one
```

`is_allowed` (shared by tools and models) first scans for any deny across all
layers, then requires the item to appear in *every* allow-list that is set.
`spend_cap_usd` folds the minimum; `within_spend` is inclusive and unconstrained
when no cap is set. `folder_trusted` normalizes `\`→`/`, trims trailing slashes,
and matches on a path-segment boundary (so `/srv/app` trusts `/srv/app/sub` but
not `/srv/apple`). `parse_layer` rejects negative or non-finite `max_spend_usd`
and ignores unknown TOML keys so newer policies degrade gracefully.

## Dependencies & features

- `serde` (layer struct), `toml` (`0.8`, layer parsing), `thiserror`. No async,
  no I/O, `#![forbid(unsafe_code)]`.

## Used by

`crates/*/Cargo.toml` matches for `origin-policy`:

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-policy/Cargo.toml`

## Testing

All tests are in-file in `lib.rs`. They cover tier precedence ordering, admin
deny over user allow, allow-list intersection, spend-cap minimum (inclusive
boundary), segment-aware folder trust, role precedence fall-through, TOML
parsing (including unknown-key tolerance and negative-spend rejection), and an
end-to-end multi-tier resolution.

## See also

- [Security model](../security/security-model.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
