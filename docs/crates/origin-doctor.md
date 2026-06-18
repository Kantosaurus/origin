# origin-doctor

> Environment/runtime diagnostics with injected probes plus a privacy phone-home disclosure

## Purpose

`origin-doctor` is the pure verdict engine behind a `doctor:runtime` health
checklist and a `verify:privacy` disclosure. It performs **no real I/O**: every
fact about the environment (toolchain version, config presence, daemon
reachability, configured providers, home writability, connectivity) arrives
through an injected `DoctorInputs` value, so the CLI does the probing and this
crate does deterministic verdict logic. Alongside the checks it always emits the
fixed list of outbound ("phone-home") behaviours the tool can perform.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `MIN_RUST_VERSION` | const `(u64,u64)` | Workspace MSRV `(1, 83)`; older fails, unknown warns. |
| `Health` | enum | `Ok` < `Warn` < `Fail` (ordered for `worst`). |
| `Health::label()` | fn → `&'static str` | `OK` / `WARN` / `FAIL`. |
| `Check` | struct | `{ name, health, detail }` for one diagnostic line. |
| `DoctorInputs` | struct | Injected environment facts (see below). |
| `DoctorReport` | struct | `{ checks, phone_home }`. |
| `DoctorReport::worst()` | fn → `Health` | Most severe verdict (or `Ok` when empty). |
| `DoctorReport::to_text()` | fn → `String` | Aligned terminal rendering + privacy section. |
| `DoctorReport::to_json()` | fn → `Result<String, DoctorError>` | Pretty JSON. |
| `phone_home_disclosures()` | fn → `Vec<String>` | The constant outbound-behaviour list. |
| `diagnose(&DoctorInputs)` | fn → `DoctorReport` | Run all six checks + disclosure. |
| `DoctorError` | enum | `Serialize(String)`. |

## Key types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Health { Ok, Warn, Fail }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorInputs {
    pub rust_version: Option<String>,
    pub config_present: bool,
    pub daemon_running: bool,
    pub providers_configured: Vec<String>,
    pub writable_home: bool,
    pub network_ok: Option<bool>,
}
```

## How it works

`diagnose` runs six pure check functions in a stable order and pairs them with
`phone_home_disclosures()`:

```
DoctorInputs ─► [toolchain, config, daemon, providers, home, network] ─► DoctorReport
                                                          │
DoctorReport::worst() = max(check.health)  (Ok < Warn < Fail)
```

Verdict rules:

- **toolchain** — parses leading `major.minor` (tolerating `-nightly`/`+`
  suffixes): `>= MSRV` is `Ok`, older is `Fail`, unparseable or missing is `Warn`.
- **config / daemon** — present/running is `Ok`, otherwise `Warn` (defaults /
  start-on-demand).
- **providers** — empty is `Fail` (cannot send requests), otherwise `Ok`.
- **home** — unwritable is `Fail` (sessions cannot persist).
- **network** — `Some(true)` `Ok`, `Some(false)` `Fail`, `None` `Warn`.

The phone-home list is a hard-coded constant so the disclosure cannot silently
drift from actual behaviour: the npm auto-update check (disabled with
`ORIGINX_NO_UPDATE=1`), model/provider API requests to configured endpoints, and
optional opt-in telemetry. `to_text` renders the worst verdict, each `[LABEL]
name: detail` line, and a `privacy — outbound behaviours:` section.

## Dependencies & features

- `serde` / `serde_json` — `DoctorReport` JSON round-trip.
- `thiserror` — `DoctorError`.
- `#![forbid(unsafe_code)]`; no cargo features. No probing dependencies by
  design — the caller injects all facts.

## Used by

`Grep "origin-doctor" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-doctor/Cargo.toml` (self)

## Testing

Inline tests assert: all-`Ok` inputs yield six `Ok` checks; missing config warns
without failing; no providers fail; an old toolchain fails while an unknown
version only warns and `-nightly` parses; unwritable home and a failed network
probe both fail (and an un-attempted probe warns); the phone-home list always
lists the auto-update behaviour with `ORIGINX_NO_UPDATE=1`; JSON round-trips; the
text output includes the verdict + privacy section; `worst` ordering holds and
an empty report is `Ok`; and `parse_major_minor` handles suffixes.

## See also

- [Observability subsystem](../subsystems/observability.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
