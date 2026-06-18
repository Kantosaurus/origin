# Adding a crate

This is the checklist for adding a new member crate to the `origin` workspace. New
crates go under `crates/` and are picked up automatically by the
`members = ["crates/*", "xtask"]` glob in the root `Cargo.toml` — but they must
inherit the workspace metadata and lint policy, carry SPDX headers, ship docs, and
clear the same gates as everything else.

Decide first whether you actually need a new crate. The workspace already has ~77;
a crate boundary is justified when it (a) is a reusable layer with a clean public
API, (b) needs different dependencies or feature flags, or (c) needs an audited
exception (only `cas`/`tui`/`ipc` use `unsafe`). Otherwise add a module to an
existing crate.

---

## Checklist

1. **Pick a name.** Lowercase, `origin-` prefixed, hyphenated:
   `origin-<thing>`. The Rust crate name (underscores) is derived automatically.
2. **Create the directory and manifest** at `crates/origin-<thing>/Cargo.toml`
   using the [template](#cargotoml-template). Inherit `version`/`edition`/
   `rust-version`/`license`/`repository` from the workspace and set
   `[lints] workspace = true` plus a `description` (required for crates.io).
3. **Create `src/lib.rs`** (or `src/main.rs` for a binary) starting with the SPDX
   header and a `//!` module doc — see the [skeleton](#libsrs-skeleton).
4. **Wire dependencies.** Reference internal crates by `path`, and shared
   third-party crates via the workspace (`dep = { workspace = true }`). New
   external deps must clear `cargo deny` (rustls-only TLS, crates.io-only).
5. **Add tests.** A `#[cfg(test)] mod tests` block, and `tests/*.rs` integration
   tests if the crate has a meaningful public surface. TDD: write them first.
6. **Add the consumer wiring.** Add the new crate as a `path` dependency in
   whatever crate will use it, and call it.
7. **Document it.** Add `docs/crates/origin-<thing>.md` (follow the existing
   one-pager shape: title, one-line summary, Purpose, Public API surface table)
   and add it to the crate index / mdBook summary if listed there.
8. **Update `CHANGELOG.md`** under `## Unreleased` (Added) describing the new
   crate.
9. **Run the gates** (see [building-and-testing.md](building-and-testing.md)):

   ```sh
   cargo build -p origin-<thing>
   cargo test -p origin-<thing>
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo fmt --all -- --check
   cargo run -p xtask -- lint-spawn
   cargo run -p xtask -- lint-secrets
   cargo deny check advisories bans sources
   ```

10. **Mind the layering.** A new crate must not create an upward dependency (a
    lower layer depending on a higher one). See
    [workspace-layout.md](workspace-layout.md#the-layered-crate-dependency-story).

---

## `Cargo.toml` template

Copy this to `crates/origin-<thing>/Cargo.toml` and edit the name, description,
and dependencies. The `version`/`edition`/`rust-version`/`license`/`repository`
keys **must** inherit from the workspace so the single source of truth (workspace
version `0.9.8`, MSRV `1.83`) stays authoritative.

```toml
[package]
name = "origin-thing"
description = "One-line description of what origin-thing does (shows on crates.io)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
# publish = false   # uncomment for internal-only crates (e.g. bench/tools)

[lints]
workspace = true

[dependencies]
# Internal crates: reference by path.
origin-core = { path = "../origin-core" }

# Shared third-party crates: inherit the pinned version from the workspace.
serde = { version = "1", features = ["derive"] }
thiserror = "1"
tracing = "0.1"

# Optional feature wiring example:
# origin-replay = { path = "../origin-replay", optional = true }

# [features]
# recorder = ["dep:origin-replay"]

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Notes:

- Prefer adding a shared third-party crate to `[workspace.dependencies]` in the
  root manifest and referencing it as `dep = { workspace = true }` when more than
  one crate uses it; that keeps versions pinned in one place for MSRV 1.83.
- Do **not** add `[lints]` overrides that relax the workspace policy. If you need
  `#[allow(clippy::…)]`, scope it to the smallest item with a justification.
- `unsafe` is forbidden. Only `origin-cas`, `origin-tui`, and `origin-ipc` may
  re-enable it, and only via a security-review decision — a new crate does not get
  to opt in.

---

## `lib.rs` skeleton

Copy this to `crates/origin-<thing>/src/lib.rs`. Start with the SPDX header (first
line, no exceptions) and a module doc-comment describing the crate's role.

```rust
// SPDX-License-Identifier: Apache-2.0
//! `origin-thing` — one-sentence statement of what this crate is responsible for.
//!
//! A slightly longer paragraph if useful: the public entry points, the invariant
//! it upholds, and where it sits in the layering.

use thiserror::Error;

/// The crate's public error type. One enum per crate boundary; variants name the
/// failure, messages are lowercase with no trailing period and never embed a
/// secret.
#[derive(Debug, Error)]
pub enum ThingError {
    #[error("invalid input: {0}")]
    Invalid(String),
}

/// The crate's primary public type.
#[derive(Debug, Clone)]
pub struct Thing {
    name: String,
}

impl Thing {
    /// Construct a `Thing`.
    ///
    /// # Errors
    /// Returns [`ThingError::Invalid`] if `name` is empty.
    pub fn new(name: impl Into<String>) -> Result<Self, ThingError> {
        let name = name.into();
        if name.is_empty() {
            return Err(ThingError::Invalid("name must not be empty".into()));
        }
        Ok(Self { name })
    }

    /// The thing's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_name() {
        assert!(Thing::new("").is_err());
    }

    #[test]
    fn keeps_the_name() {
        let t = Thing::new("origin").expect("non-empty name is valid");
        assert_eq!(t.name(), "origin");
    }
}
```

For a **binary** crate, use `src/main.rs` with the same SPDX header; binaries may
use `anyhow::Result` for top-level glue while libraries expose typed errors.

If your crate spawns async tasks, do not call `tokio::spawn` — use
`origin_runtime::spawn_in(TaskClass::…, fut)` and pick the right class. The
`lint-spawn` gate enforces this (see
[coding-standards.md](coding-standards.md#the-spawn_in-task-class-rule)).

---

## `docs/crates/origin-<thing>.md` template

```markdown
# origin-thing

> One-line summary of the crate.

## Purpose

A paragraph: what problem this crate solves and where it sits in the workspace
layering.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Thing` | struct | The primary type. |
| `Thing::new` | fn | Construct a validated `Thing`. |
| `ThingError` | enum | The crate's error type. |

## Notes

Anything a consumer or maintainer should know (feature flags, invariants,
test coverage).
```

---

## Common pitfalls

| Symptom | Cause / fix |
| --- | --- |
| Crate not built | Missing under `crates/` (the glob), or excluded — check `[workspace] exclude`. |
| Clippy fails in CI but not locally | You didn't run `-D warnings`; run `cargo clippy --workspace --all-targets --locked -- -D warnings`. |
| `cargo deny` failure | New dep pulls non-rustls TLS or a non-crates.io source; pick another, or pin a rustls-only feature set. |
| MSRV breakage | A dep needs `edition2024`/rustc > 1.83; pin an older 1.83-safe version in `Cargo.lock`. |
| `lint-secrets` failure | A `#[derive(Debug)]` field named like a secret isn't `Secret<…>`; wrap it or add `#[redact]`. |
| Missing SPDX header | First line of every `.rs` must be `// SPDX-License-Identifier: Apache-2.0`. |

_Last reviewed against workspace version 0.9.8._
