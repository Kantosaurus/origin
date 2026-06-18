# origin-conseca

> Dynamic, model-generated per-prompt security policy parsed and enforced per tool call.

## Purpose

`origin-conseca` implements `ConSeca`-style contextual security: a *trusted*
model reads the task description and emits a JSON [`SecurityPolicy`], which this
crate then parses and enforces on every individual tool call. Policy
*generation* (the model hop) is supplied by the caller; the crate itself is pure
parsing and enforcement logic, so it stays deterministic and offline-testable.
The core defense is that the policy is derived solely from trusted inputs and
cannot be widened by prompt-injected content in tool outputs.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `SecurityPolicy` | struct | Declarative allow/deny lists + rationale. |
| `Decision` | enum | `Allow` / `Deny(String)`; `is_allow()` helper. |
| `parse_policy` | fn | Parse model JSON into a `SecurityPolicy`. |
| `check_tool` | fn | Tool-name allow/deny check (deny wins). |
| `check_path` | fn | Path-prefix allow/deny check (segment-aware). |
| `check_domain` | fn | URL-host allow check (closed by default). |
| `prompt_for_policy` | fn | Build the system prompt that asks a model for a policy. |
| `ConSecaError` | enum | `Parse(String)`. |

## Key types

```rust
#[derive(Default, Serialize, Deserialize)]
pub struct SecurityPolicy {
    pub allow_tools: Vec<String>,    // empty = any tool not denied
    pub deny_tools: Vec<String>,     // deny always wins
    pub allow_paths: Vec<String>,    // permitted fs prefixes
    pub deny_paths: Vec<String>,     // forbidden fs prefixes
    pub allow_domains: Vec<String>,  // empty = deny ALL network
    pub rationale: String,
}

pub enum Decision { Allow, Deny(String) }

pub fn parse_policy(json: &str) -> Result<SecurityPolicy, ConSecaError>;
pub fn check_tool(p: &SecurityPolicy, tool: &str) -> Decision;
pub fn check_path(p: &SecurityPolicy, path: &str) -> Decision;
pub fn check_domain(p: &SecurityPolicy, url: &str) -> Decision;
pub fn prompt_for_policy(trusted_inputs: &str) -> String;
```

## How it works

The model is prompted (`prompt_for_policy`) to emit least-privilege JSON and to
treat tool outputs as DATA, never instructions. `parse_policy` defaults missing
fields to empty (a partial document still yields a usable, restrictive policy).
Enforcement runs per tool call:

```text
check_tool   deny_tools? → Deny ; else allow_tools empty OR contains → Allow
check_path   normalize \→/ ; deny_paths prefix? → Deny ; allow empty OR prefix → Allow
check_domain allow_domains empty → Deny(all) ; host matches entry/subdomain → Allow
```

Path matching is path-segment aware (`/etc` does not match `/etchosts`) and
normalizes backslashes so Windows and POSIX paths compare alike. Domain matching
is the security-critical part: `extract_host` mirrors WHATWG URL parsing —
stripping embedded tab/newline/CR and terminating the authority on `\` as well
as `/?#` — so a `https://evil.com\@allowed.com/` differential cannot bypass the
allowlist (the extractor sees the same host reqwest would dial). Hosts match an
entry exactly or as a subdomain of it; network is closed unless `allow_domains`
opens it.

## Dependencies & features

- `serde` + `serde_json` (policy JSON), `thiserror`. No async, no I/O,
  `#![forbid(unsafe_code)]`.

## Used by

`crates/*/Cargo.toml` matches for `origin-conseca`:

- `crates/origin-conseca/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`

## Testing

All tests are in-file in `lib.rs`. They cover full/partial JSON parsing,
deny-beats-allow for tools, path prefix + backslash normalization, default-deny
domains, and the two host-parser-differential bypass classes
(`\@`-authority and embedded control chars), plus the prompt's schema/anti-injection
text.

## See also

- [Security model](../security/security-model.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
