# Coding standards

These are the conventions every crate in the workspace follows. They are not
suggestions: most are enforced by CI (`-D warnings`, the unsafe-audit gate, the
`xtask` source lints). New code that violates them will not merge.

---

## Lint policy (verbatim)

The policy lives in the workspace manifest (`Cargo.toml`) and is inherited by
every crate via `[lints] workspace = true`:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"  # overridden in cas/tui/ipc

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery  = { level = "warn", priority = -1 }
unwrap_used = "deny"
panic = "warn"
```

What this means in practice:

| Lint | Level | Consequence under CI `-D warnings` |
| --- | --- | --- |
| `unsafe_code` | **forbid** | Any `unsafe` is a compile error (outside the three audited crates). |
| `clippy::pedantic` | warn | Becomes a hard error in CI. |
| `clippy::nursery` | warn | Becomes a hard error in CI. |
| `clippy::unwrap_used` | **deny** | A hard error regardless; `.unwrap()` is banned. |
| `clippy::panic` | warn | Becomes a hard error in CI; avoid `panic!`. |

- Prefer `?`, `expect("explains the invariant")`, or explicit error handling over
  `.unwrap()`. An `expect` message should state the invariant that makes the panic
  unreachable, not just restate the call.
- Production code should not `panic!`. Tests may, and `expect`/`unwrap` in
  `#[cfg(test)]` is fine.
- If a pedantic/nursery lint is genuinely wrong for one spot, scope an
  `#[allow(clippy::…)]` to the **smallest item** with a one-line justification.
  You'll see this pattern in the tree, e.g. `#[allow(clippy::module_name_repetitions)]`
  on `TaskClass`.

Run the gate exactly as CI does:

```sh
cargo clippy --workspace --all-targets --locked -- -D warnings
```

---

## `unsafe` and the audited exceptions

`unsafe_code = "forbid"` is workspace-wide. The **only** crates allowed to use
`unsafe` are:

| Crate | Why it needs `unsafe` |
| --- | --- |
| `origin-cas` | mmap-backed pack files and zero-copy reads in the content store. |
| `origin-tui` | the terminal grid / ANSI fast paths. |
| `origin-ipc` | the wire-frame transport and zero-copy archived-frame handling. |

Each re-enables `unsafe` locally with a reviewed justification. The unsafe-audit
CI gate asserts `unsafe` appears in **only** those three crates — introducing it
anywhere else fails the build. Do not add `unsafe` outside them; if you believe a
new crate needs it, raise it with the maintainer first (it is a security-review
decision, not a code-review one).

---

## SPDX headers

Every first-party `.rs` file starts with the SPDX identifier as the very first
line:

```rust
// SPDX-License-Identifier: Apache-2.0
```

The repo is REUSE-compliant (`REUSE.toml` + `LICENSES/Apache-2.0.txt`). New
source files must carry the header; missing headers are a review blocker.

---

## Module and item documentation

- **Module doc-comments** (`//!`) head each module and describe its role in one or
  two sentences, right under the SPDX line. Example from `origin-runtime`:

  ```rust
  // SPDX-License-Identifier: Apache-2.0
  //! `origin-runtime` — task-class budgeting + `spawn_in` helper.
  ```

- **Item docs** (`///`) on public items. Because pedantic is on, public functions
  that return `Result` carry a `# Errors` section, and `unsafe` fns (in the three
  audited crates) a `# Safety` section. Backtick crate/type names so rustdoc links
  them.
- Keep docs accurate to behavior — the docs site (`docs/site/`) and the per-crate
  reference (`docs/crates/<name>.md`) are published, and drift is a bug.

---

## Error handling: `thiserror` enums

Library crates model their failure modes as a `thiserror`-derived enum, not
stringly-typed errors or `anyhow` in the public API. The canonical shape (from
`origin-provider`):

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("api: {0}")]
    Api(String),
    #[error("auth")]
    Auth,
    #[error("rate limit; retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u32, message: String },
}
```

Guidelines:

- One error enum per crate boundary; variants name the failure, not the call site.
- `#[error("…")]` messages are lowercase, no trailing period, and never embed a
  secret (see [`Secret<T>`](#secrett-redaction)).
- Binaries and `xtask` may use `anyhow::Result` for top-level glue; libraries
  expose typed errors so callers can match.
- Use `#[from]` for clean `?` conversion where it does not hide meaning.

---

## The `spawn_in` task-class rule

Raw `tokio::spawn` / `tokio::task::spawn` / `tokio::task::spawn_blocking` are
**banned** outside the one sanctioned site. Every async task in the daemon is
spawned through `origin_runtime::spawn_in(class, fut)`, which acquires a per-class
semaphore permit before polling the future, bucketing work by priority and
enforcing the fairness rule (low-priority `Bulk` work parks while any `Critical`
task is in flight).

The task classes (`origin_runtime::TaskClass`):

| Class | Used for |
| --- | --- |
| `Critical` | Agent loop turns; provider HTTP/2; tool exec; swarm worker bodies. |
| `Realtime` | Renderer ticks; IPC event dispatch; per-stream relays. |
| `Sidecar` | Sidecar small-model jobs; MCP server clients; hook dispatch. |
| `Background` | CAS GC; SQLite vacuum; memory idle consolidation. |
| `Bulk` | Initial code-graph build; bulk MCP discovery. Paused when `Critical` is busy. |
| `Swarm` | Swarm sub-agent worker bodies (isolated permit pool). |

```rust
use origin_runtime::{spawn_in, TaskClass};

let handle = spawn_in(TaskClass::Background, async move {
    store.gc().await
});
```

The ban is enforced by `xtask lint-spawn` (run it locally with
`cargo run -p xtask -- lint-spawn`). A tiny, justified allowlist exists in
`xtask/src/lint_spawn_allowlist.rs` for `spawn_in` itself, the sidecar runtime, the
supervisor's process launchers, and a few provider keepalive tasks — do not add to
it without a justification comment. For the full concurrency model, see
[../architecture/runtime-and-concurrency.md](../architecture/runtime-and-concurrency.md).

---

## `Secret<T>` redaction

Secrets cross trust boundaries only through `origin_keyvault::Secret<T>` and the
`KeyVault` façade. `Secret<T>` is intentionally minimal:

- zeroizes its inner value on drop;
- redacts in `Debug` (prints `Secret<redacted>`);
- has no `Clone`, no `Display`, no `Serialize`/`Deserialize`.

```rust
use origin_keyvault::Secret;

let token = Secret::new(raw_string);   // zeroized on drop
tracing::debug!(?token);                // prints `Secret<redacted>`, never the value
let bytes = token.expose();             // explicit, audited access point
```

### The no-secret-through-tracing lint

`xtask lint-secrets` walks the workspace AST and flags any `#[derive(Debug)]`
struct whose field name matches `(?i)(key|token|password|auth|secret|credential)`
**unless** the field is wrapped in `Secret<…>` or carries a `#[redact]` attribute.
The point is to stop a secret leaking through a derived `Debug` impl into a
`tracing` line. It pre-filters to string-like fields and ignores `*_url`/`url`
names. Run it before pushing:

```sh
cargo run -p xtask -- lint-secrets
```

If a flagged field is genuinely not a secret, rename it or add `#[redact]` with a
comment; do not weaken the regex.

---

## Testing conventions

- **TDD.** Write a failing test first; bug fixes ship with a regression test.
- **Unit tests** live in a `#[cfg(test)] mod tests { use super::*; … }` block at
  the bottom of the module under test. Tests may `expect`/`unwrap` freely.
- **Integration tests** live in `crates/<name>/tests/*.rs`.
- **Property tests** use `proptest` for parsers and wire boundaries.
- Live-network tests **self-skip** when the relevant key is absent rather than
  failing.
- Name tests for the behavior they pin (`stamp_substitutes_version_and_sha`,
  `generates_at_least_origin_1`), not the function name.

---

## Cross-cutting invariants

- The daemon ↔ CLI boundary is `origin-ipc` only — rkyv-archived frames, no side
  channel. Changing the frame format is a wire-protocol change (coordinate first).
- Types that flow through IPC, storage, and the ring buffer are `rkyv::Archive`
  from day one — keep new IR types archiveable.
- Keep `--locked` builds green; transitive deps are pinned for MSRV 1.83. Adding a
  dependency must clear `cargo deny` (rustls-only TLS, crates.io-only sources).

_Last reviewed against workspace version 0.9.8._
