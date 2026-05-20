# `origin` Phase 11 — Security + Observability + Sandboxing (`origin-sandbox` + tracing/parquet + KeyVault audit + `Secret<T>` lint) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax. Tasks marked **[parallel-safe]** can run concurrently in fresh subagents (see "Parallelization" below).

**Branch:** All Phase 11 work lands on branch `phase-11` (branched off `dev`, which now carries the `p10-complete` tag).

**Goal:** Bolt production-grade security + observability onto the P10 baseline — (1) per-tool sandbox profiles on Linux (user-ns + seccomp + landlock), macOS (`sandbox-exec`), and Windows (AppContainer + restricted Job Object) with hook-script profile inheritance, (2) MCP inbound-message validation against the registered tool `input_schema` with a 16 MiB hard cap, (3) a tracing pipeline that writes structured spans to a per-day parquet ring queryable via `origin trace query`, (4) a bounded-cardinality metrics surface visible in a TUI `?metrics` panel and an opt-in `/metrics` Prometheus endpoint (with optional OTel export), (5) a KeyVault audit log (30-day rotating ring, separate from the trace parquet), and (6) a CI lint that enforces `Secret<T>` discipline so no field named `*key*`/`*token*`/`*password*`/`*auth*` emits raw bytes through `Debug`/`tracing`.

**Architecture:** One new crate (`origin-sandbox`) carries the per-OS profile backends. One new crate (`origin-trace`) carries the parquet ring + query layer; metrics live in a new sibling crate (`origin-metrics`) so the TUI can depend on it without dragging in parquet. The MCP hardening, KeyVault audit, and `Secret<T>` lint are surgical extensions to existing crates (`origin-mcp`, `origin-keyvault`, plus a new top-level `xtask` for the redaction lint). Each cluster is independent of the others after the branch-checkpoint task (P11.0), so the five area-clusters are **fully parallelizable**.

**Tech Stack:** Rust 1.83 (MSRV pin), `seccompiler` 0.4 (Linux seccomp BPF), `landlock` 0.4 (Linux LSM), `caps` 0.5 (Linux capability drop), `windows-sys` 0.59 (already in workspace; AppContainer + Job Object FFI), `arrow` 53 + `parquet` 53 (columnar writer for the trace ring; pinned to a single Arrow major to avoid the typical Arrow churn), `jsonschema` 0.18 (MCP `input_schema` validation; pure-Rust draft 2020-12 support), `prometheus` 0.13 (text-format encoder; no built-in HTTP server — we serve it ourselves over `hyper` 1 which is already a transitive dep via `reqwest`), `opentelemetry` 0.24 + `opentelemetry-otlp` 0.17 (feature-gated behind `otel`), `tracing` 0.1 (already a workspace dep transitively; promoted to a direct one this phase), `tracing-subscriber` 0.3 (already used by `origin-daemon`'s `main.rs`), `serde_json` 1, `serde` 1, `thiserror` 1, `chrono` 0.4 (parquet partition key by day; `chrono` is the path of least friction since arrow already depends on it).

**Novel-implementation reflex** per `[[feedback_novel_implementations]]` — every signature subsystem must beat openclaude/jcode/opencode on tokens or perf. Phase 11's novelties:

1. **Sandbox profiles compile down to a per-tool `SandboxProfile` const** held on `ToolMeta` — the dispatch hot path sees zero allocations and the profile is selected by an `enum` discriminant, not a string lookup. Compare with jcode's runtime YAML-keyed profile lookup.
2. **Hook profile inheritance is a transparent envelope on `LifecycleEvent::PreTool`** — the hook script receives the triggering tool's `SandboxProfile` ordinal in the event payload so user hooks can short-circuit when policy disagrees, *without* the daemon round-tripping back to the permission engine.
3. **Trace spans go straight to a `parquet` ring writer fed from a SPSC channel** — no `tracing-appender` rolling file in between, and every span is encoded as a single Arrow `RecordBatch` row group (columns: `ts_ns`, `span_id`, `parent_id`, `kind`, `provider`, `tool`, `dur_us`, `error_kind`, `attrs_json`). 64 MiB rotation, per-day partition, mmap-friendly read path.
4. **`origin trace query` uses a tiny pushdown predicate** over the parquet metadata so a `kind=tool AND error_kind=Sandbox` filter never opens unrelated row groups.
5. **`/metrics` Prometheus endpoint reuses the daemon's existing `tokio` runtime** and is gated by an explicit `--metrics-bind <addr>` flag — no background HTTP listener unless the operator opts in. Token-accounting counters keyed by `(provider, model, kind)` only — no per-session label.
6. **KeyVault audit log is a separate 30-day ring** (8 MiB pages × 30 days; oldest page recycled) that is *not* the parquet trace pipeline — so a parquet write failure can't drop secret-access records, and an audit-log query never opens parquet files.
7. **`Secret<T>` CI lint is an `xtask` that walks the workspace AST and fails on `#[derive(Debug)]` for any struct whose field name matches `r"(?i)(key|token|password|auth|secret|credential)"` unless the field type is `Secret<…>` or marked `#[redact]`.** Hard fail on `tracing::field::Visit::record_str` calls whose source identifier matches the same regex unless wrapped.

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` mechanisms **N10.4** (parquet ring), **N10.5** (bounded-cardinality metrics), **N10.6** (live token accounting), **N10.11** (per-tool sandbox profiles), **N10.12** (hook inheritance), **N10.13** (MCP validation + cap), **N10.14** (`Secret<T>` + CI lint), **N10.15** (worker isolation — scoped to *daemon-child* tool processes; full sidecar arena work is deferred to P12), **N10.16** (KeyVault audit). Builds on the `p10-complete` tag.

**Phase 11 spec-mechanism citations:**

- **N10.4** — Structured spans → parquet ring (Tasks P11.9, P11.10)
- **N10.5** — Bounded-cardinality metrics (Task P11.12)
- **N10.6** — Live token accounting (Task P11.12)
- **N10.11** — Per-tool sandbox profiles (Tasks P11.1 – P11.5)
- **N10.12** — Hook script profile inheritance (Task P11.6)
- **N10.13** — MCP message validation + 16 MiB cap (Tasks P11.7, P11.8)
- **N10.14** — `Secret<T>` newtype + CI lint (Task P11.14)
- **N10.15** — Worker (tool-process) isolation via CPU/RAM caps (folded into P11.2/P11.3/P11.4 per-OS backends)
- **N10.16** — KeyVault audit log (Task P11.13)

**What is explicitly out of scope for Phase 11** (deferred):

- Sidecar-class arena allocation for tracing (lives in P12 with the jemalloc arena work).
- Two-runtime split for the metrics endpoint — P11 reuses the existing daemon Tokio runtime; the dedicated worker pool runtime is a P12 deliverable.
- QUIC remote attach for `/metrics` and `origin trace query` — P13.
- Hook script sandboxing via *user-level* policy DSL — P11 inherits the triggering tool's `SandboxProfile` verbatim and exposes the ordinal to the hook; richer per-hook policy is post-GA.
- Per-server MCP *process* isolation — P10 shipped the `quarantine: true|false` knob as a soft `Tier::RequiresPermission` override; P11 adds the schema/cap layer but **does not** spawn MCP servers in their own user namespace. That's deferred until ACP is on the roadmap.
- Migration of existing tools to declare a non-`Default` `SandboxProfile` — P11 ships the surface and migrates the five highest-blast-radius tools (`Bash`, `Edit`, `Write`, `WebFetch`, `Read`); the rest stay on `SandboxProfile::Inherit` (a no-op profile) until P12 sweeps them.

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**

1. Write the failing test.
2. Run it — confirm the expected failure mode (compile error or assertion).
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit (Conventional Commits, scoped to crate).

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / tool-meta extension / daemon wiring | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Per-OS backend (P11.2 Linux, P11.3 macOS, P11.4 Windows) | `cargo test -p origin-sandbox --features <os>` on the matching host **plus** `cargo check -p origin-sandbox --features <os>` cross-compiled (CI matrix). When iterating on Windows host, skip Linux/macOS test runs and rely on the cross `cargo check`. |
| Bench-touching tasks (P11.10 parquet write throughput, P11.12 prom encode throughput) | All of the above + `cargo bench -p <crate> --bench <name> -- --quick` exits 0 with thresholds met |
| `xtask` redaction lint (P11.14) | `cargo run -p xtask -- lint-secrets` exits 0 on a clean tree and **non-zero** on the synthetic violation fixture |
| Final phase gate (P11.15) | All of the above + tag `p11-complete` |

**Patterns inherited from earlier phases:**

- `[lints] workspace = true` in every new crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All persisted/IPC-crossing types derive `serde::{Serialize, Deserialize}` (JSON for hook payloads + Prom text) or `rkyv::{Archive, Serialize, Deserialize}` with `#[archive(check_bytes)]` (records that round-trip through CAS).
- `[lints.rust] unsafe_code = "forbid"` is the default; the only new crate that needs an override is `origin-sandbox` (per-OS FFI — `landlock`/`seccompiler`/`windows-sys`).
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `unwrap()` and never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex** (`[[project_msrv_dep_pinning]]`): if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>` and record in `Cargo.lock`. Likely candidates this phase: the `arrow`/`parquet` chain pulls a recent `chrono` and `time`; if Cargo trips on either, try `time = "=0.3.36"`, `chrono = "=0.4.38"`. The `prometheus` 0.13 chain may pull `protobuf` ≥ 3.5; if it requires edition 2024, pin `protobuf = "=3.4.0"`.
- **Novel-implementation reflex** (`[[feedback_novel_implementations]]`): if a step's implementation collapses into "the obvious thing openclaude does", stop and re-read the architecture novelties listed above.

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit** on branch `phase-11`. Final commit on P11.15 carries `tag: p11-complete`.

---

## Parallelization

After P11.0 the work splits into **five independent area-clusters** with no shared mutable state. Each cluster can be assigned to a fresh subagent and progressed in lock-step with the others:

| Cluster | Tasks | New crate(s) | Touches existing crates |
|---|---|---|---|
| **A. Sandbox** | P11.1 → P11.2 → P11.3 → P11.4 → P11.5 → P11.6 | `origin-sandbox` | `origin-tools` (P11.5 meta extension), `origin-hooks` (P11.6 envelope), `origin-daemon` (P11.5 wiring) |
| **B. MCP hardening** | P11.7 → P11.8 | _(extends `origin-mcp`)_ | `origin-mcp` |
| **C. Tracing + parquet ring** | P11.9 → P11.10 → P11.11 | `origin-trace` | `origin-daemon` (P11.10 layer init), `origin-cli` (P11.11 subcommand) |
| **D. Metrics + TUI panel** | P11.12 | `origin-metrics` | `origin-tui` (P11.12 panel widget), `origin-daemon` (P11.12 `/metrics` server) |
| **E. KeyVault audit + Secret lint** | P11.13 → P11.14 | `xtask` (new top-level binary) | `origin-keyvault` (P11.13 audit ring) |

Within a cluster, tasks **must** run sequentially (later tasks depend on earlier types/modules within the same crate). Across clusters there are **no** compile-time or test-time dependencies; subagents may proceed without waiting for siblings. The final task **P11.15** depends on the green state of all five clusters and gates the `p11-complete` tag — the dispatcher should hold the tag-bearing merge until all five clusters land on `phase-11`.

**Dispatch order for parallel subagents (recommended):**

```
            P11.0 (branch + workspace deps)
                       │
   ┌─────────┬─────────┼─────────┬─────────┐
   ▼         ▼         ▼         ▼         ▼
Cluster A  Cluster B  Cluster C  Cluster D  Cluster E
P11.1-6    P11.7-8    P11.9-11   P11.12    P11.13-14
   │         │         │         │         │
   └─────────┴─────────┴─────────┴─────────┘
                       │
                    P11.15 (phase gate + tag)
```

Each subagent gets a single-cluster prompt that says: "*work tasks P11.X through P11.Y sequentially; commit each on `phase-11`; do not touch files outside cluster's column in the file map.*"

---

## File map for Phase 11

| New / modified file | Responsibility |
|---|---|
| **Cluster A — Sandbox** | |
| `crates/origin-sandbox/Cargo.toml` | manifest; workspace lints; `unsafe_code = "allow"` override; per-OS feature flags |
| `crates/origin-sandbox/src/lib.rs` | public surface — `SandboxProfile`, `SandboxError`, `apply()` entry |
| `crates/origin-sandbox/src/profile.rs` | `SandboxProfile` enum + per-variant policy table (P11.1) |
| `crates/origin-sandbox/src/backend_linux.rs` | user/mount ns + seccomp BPF + landlock ruleset (P11.2) |
| `crates/origin-sandbox/src/backend_macos.rs` | `sandbox-exec` profile string generator + `Command` wrapper (P11.3) |
| `crates/origin-sandbox/src/backend_windows.rs` | AppContainer SID + Job Object with `JOB_OBJECT_LIMIT_*` (P11.4) |
| `crates/origin-sandbox/src/backend_noop.rs` | dev-only no-op backend used for tests on unsupported OS or `--features no-sandbox` |
| `crates/origin-sandbox/src/caps.rs` | per-platform CPU/RAM cap helpers (cgroup v2 on Linux, `setrlimit` on macOS, JobObject quotas on Windows) — also covers N10.15 |
| `crates/origin-sandbox/tests/profile.rs` | profile ordinals stable; default = `Inherit` (P11.1) |
| `crates/origin-sandbox/tests/backend_linux.rs` | `#[cfg(target_os="linux")]` — landlock denies write outside allowed roots (P11.2) |
| `crates/origin-sandbox/tests/backend_macos.rs` | `#[cfg(target_os="macos")]` — sandbox-exec denies write outside allowed roots (P11.3) |
| `crates/origin-sandbox/tests/backend_windows.rs` | `#[cfg(target_os="windows")]` — AppContainer denies write outside allowed roots; JobObject SIGKILLs runaway child (P11.4) |
| `crates/origin-tools/src/registry.rs` *(modify P11.5)* | add `pub sandbox_profile: SandboxProfile` to `ToolMeta`; default = `Inherit` |
| `crates/origin-tools/src/macros.rs` *(modify P11.5)* | `origin_tool!` accepts optional `sandbox = SandboxProfile::Foo` |
| `crates/origin-tools/src/builtins/bash.rs` *(modify P11.5)* | declare `sandbox = SandboxProfile::Shell` |
| `crates/origin-tools/src/builtins/edit.rs` *(modify P11.5)* | declare `sandbox = SandboxProfile::WriteCwd` |
| `crates/origin-tools/src/builtins/read.rs` *(modify P11.5)* | declare `sandbox = SandboxProfile::ReadFs` |
| _(WebFetch is not yet a builtin in this tree — sandbox flag deferred to whichever phase lands it.)_ | |
| `crates/origin-tools/tests/sandbox_meta.rs` *(new, P11.5)* | every builtin declares a profile; default-derived value is `Inherit` |
| `crates/origin-daemon/src/agent.rs` *(modify P11.5)* | call `origin_sandbox::apply(meta.sandbox_profile, &mut cmd)` on every `tokio::process::Command` we spawn |
| `crates/origin-hooks/src/event.rs` *(modify P11.6)* | add `sandbox_ordinal: u8` to `LifecycleEvent::PreTool` and `PostTool` |
| `crates/origin-hooks/src/dispatch.rs` *(modify P11.6)* | propagate the triggering tool's profile through the event payload |
| `crates/origin-hooks/tests/profile_inherit.rs` *(new, P11.6)* | hook payload carries ordinal from the tool registry |
| **Cluster B — MCP hardening** | |
| `crates/origin-mcp/src/limits.rs` *(new, P11.7)* | `MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;` + `enforce_cap` helper |
| `crates/origin-mcp/src/transport.rs` *(modify P11.7)* | reject any response > `MAX_RESPONSE_BYTES` before parsing |
| `crates/origin-mcp/src/transport_stdio.rs` *(modify P11.7)* | byte-counted reader; abort on cap |
| `crates/origin-mcp/src/transport_http.rs` *(modify P11.7)* | content-length pre-check + chunk-counted body reader |
| `crates/origin-mcp/src/schema.rs` *(new, P11.8)* | thin wrapper over `jsonschema::JSONSchema` keyed by tool name |
| `crates/origin-mcp/src/proxy.rs` *(modify P11.8)* | validate `args` against the tool's stored schema before `call_tool` |
| `crates/origin-mcp/tests/limits.rs` *(new, P11.7)* | 16 MiB + 1 byte → `TransportError::TooLarge`; 16 MiB - 1 byte → ok |
| `crates/origin-mcp/tests/schema.rs` *(new, P11.8)* | bad-shape args → `ClientError::SchemaMismatch`; good-shape args → call goes through |
| **Cluster C — Tracing + parquet ring** | |
| `crates/origin-trace/Cargo.toml` | manifest; workspace lints |
| `crates/origin-trace/src/lib.rs` | public surface — `init`, `Layer`, `Query`, `Row` |
| `crates/origin-trace/src/schema.rs` | Arrow schema for a span row (P11.9) |
| `crates/origin-trace/src/ring.rs` | parquet writer that rotates at 64 MiB and partitions per day (P11.9) |
| `crates/origin-trace/src/layer.rs` | `tracing::Subscriber`-compatible layer that feeds the ring (P11.10) |
| `crates/origin-trace/src/query.rs` | parquet reader with pushdown predicate over `(kind, error_kind)` (P11.11) |
| `crates/origin-trace/tests/ring.rs` | rotation + per-day partition + checksum on read-back (P11.9) |
| `crates/origin-trace/tests/layer.rs` | spans round-trip through the layer into the ring (P11.10) |
| `crates/origin-trace/tests/query.rs` | pushdown skips unrelated row groups (P11.11) |
| `crates/origin-trace/benches/write.rs` | ≥ 100k spans/s on a single thread (P11.10) |
| `crates/origin-daemon/src/main.rs` *(modify P11.10)* | install `origin_trace::Layer` into the global `tracing_subscriber` |
| `crates/origin-daemon/src/agent.rs` *(modify P11.10)* | `#[tracing::instrument]` on `run_turn`, `dispatch_tool`, `call_provider`, `sidecar_job` |
| `crates/origin-cli/src/main.rs` *(modify P11.11)* | subcommand `trace query` invokes `origin_trace::query::run(args)` |
| `crates/origin-cli/src/trace_cmd.rs` *(new, P11.11)* | clap definition + pretty-printer for the row stream |
| **Cluster D — Metrics + TUI panel** | |
| `crates/origin-metrics/Cargo.toml` | manifest; workspace lints |
| `crates/origin-metrics/src/lib.rs` | bounded-cardinality counter/histogram registry + Prom text encoder |
| `crates/origin-metrics/src/keys.rs` | `(class, provider, tool, error_kind)` keyspace + label allowlist |
| `crates/origin-metrics/src/exporter.rs` | optional OTel exporter — gated behind `otel` cargo feature |
| `crates/origin-metrics/tests/encode.rs` | counter increments + Prom text round-trip; cardinality cap enforced |
| `crates/origin-metrics/benches/encode.rs` | text-encode of 1000 metrics ≤ 200 µs (P11.12) |
| `crates/origin-daemon/src/main.rs` *(modify P11.12)* | optional `--metrics-bind <addr>` flag; spawn `hyper` `/metrics` route |
| `crates/origin-tui/src/widgets/metrics.rs` *(new, P11.12)* | `?metrics` panel render path; reads from `origin_metrics::snapshot()` |
| `crates/origin-tui/src/panel.rs` *(modify P11.12)* | route `?` key to the metrics widget; existing permission widget unchanged |
| `crates/origin-tui/tests/metrics_panel.rs` *(new, P11.12)* | snapshot render contains every registered metric key |
| **Cluster E — KeyVault audit + `Secret<T>` lint** | |
| `crates/origin-keyvault/src/audit.rs` *(new, P11.13)* | append-only audit ring; per-page 8 MiB; 30-day rotation; mmap-friendly |
| `crates/origin-keyvault/src/lib.rs` *(modify P11.13)* | emit `AuditEvent::{Set, Get, Delete, List}` from every public method |
| `crates/origin-keyvault/tests/audit.rs` *(new, P11.13)* | rotation + drop-on-30-days + no secret bytes ever recorded |
| `xtask/Cargo.toml` *(new, P11.14)* | thin top-level binary that runs project linting tasks |
| `xtask/src/main.rs` *(new, P11.14)* | clap subcommands — first one is `lint-secrets` |
| `xtask/src/lint_secrets.rs` *(new, P11.14)* | AST walker; rule = "fields matching `(?i)(key|token|password|auth|secret|credential)` must be `Secret<…>` or `#[redact]`" |
| `xtask/tests/fixtures/clean.rs` *(new, P11.14)* | passes the lint |
| `xtask/tests/fixtures/dirty.rs` *(new, P11.14)* | fails the lint (synthetic violation) |
| `xtask/tests/lint_secrets.rs` *(new, P11.14)* | runs the lint over both fixtures and asserts exit codes |
| **Cross-cutting** | |
| `Cargo.toml` *(modify, P11.0)* | new crates picked up by `members = ["crates/*"]`; **also** add `"xtask"` to the `members` list (it lives at workspace root, not under `crates/`); add `arrow`, `parquet`, `jsonschema`, `prometheus`, `opentelemetry`, `tracing`, `tracing-subscriber`, `landlock`, `seccompiler`, `caps`, `hyper`, `chrono` to `[workspace.dependencies]` |
| `rust-toolchain.toml` *(unchanged)* | `channel = "1.83"` |

**File-size discipline:** every new `.rs` file targets <400 LOC. If a task naturally pushes a file past 400 LOC, split early (e.g. `backend_linux.rs` → `backend_linux/seccomp.rs` + `backend_linux/landlock.rs` + `backend_linux/mod.rs`).

---

## Task P11.0 — Branch + workspace dep additions + plan checkpoint

**Files:**

- Modify: `Cargo.toml` (root workspace) — add new shared deps so each cluster crate inherits version pins.
- Modify: branch state — branch off `dev` to `phase-11`.

- [ ] **Step 1: Create the phase-11 branch**

```bash
git checkout dev
git pull --ff-only
git checkout -b phase-11
```

Run: `git branch --show-current`
Expected output: `phase-11`

- [ ] **Step 2: Add shared workspace deps**

Edit the workspace `Cargo.toml` to extend `[workspace.dependencies]`:

```toml
[workspace.dependencies]
# P10 carried these forward
serde_yaml = "0.9"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
eventsource-stream = "0.2"
growable-bloom-filter = "2"
# P11 additions
tracing = "0.1"
tracing-subscriber = { version = "0.3", default-features = false, features = ["env-filter", "fmt", "registry"] }
arrow = { version = "53", default-features = false, features = ["prettyprint"] }
parquet = { version = "53", default-features = false, features = ["arrow", "snap"] }
jsonschema = { version = "0.18", default-features = false }
prometheus = { version = "0.13", default-features = false }
opentelemetry = { version = "0.24", default-features = false, features = ["trace"] }
opentelemetry-otlp = { version = "0.17", default-features = false, features = ["grpc-tonic"] }
hyper = { version = "1", default-features = false, features = ["server", "http1"] }
chrono = { version = "0.4", default-features = false, features = ["clock"] }
caps = "0.5"
seccompiler = "0.4"
landlock = "0.4"
```

**Note:** `xtask` is **not** added to `members` here — Cluster E (Task P11.14) creates the `xtask/` directory and amends `members = ["crates/*", "xtask"]` in the same commit so the workspace stays buildable throughout the parallel cluster execution.

- [ ] **Step 3: Pin transitive deps if needed**

Run: `cargo check --workspace`

If `cargo check` fails with `edition2024` / "requires Rust 1.85+" errors, pin the offenders:

```bash
cargo update -p protobuf --precise 3.4.0
cargo update -p time --precise 0.3.36
cargo update -p chrono --precise 0.4.38
cargo update -p arrow --precise 53.0.0
cargo update -p parquet --precise 53.0.0
```

Re-run `cargo check --workspace` until it exits 0.

- [ ] **Step 4: Stage and commit the plan + workspace deps**

```bash
git add docs/superpowers/plans/2026-05-20-origin-phase-11.md Cargo.toml Cargo.lock
git commit -m "docs(origin): Phase 11 implementation plan + workspace deps (P11.0)"
```

- [ ] **Step 5: Verification gate**

Run: `cargo check --workspace`
Expected: exits 0; no new clippy/test runs at this checkpoint.
Run: `git status`
Expected: working tree clean.

---

# Cluster A — Sandbox profiles

## Task P11.1 — `origin-sandbox` skeleton + `SandboxProfile` enum  **[parallel-safe with B/C/D/E]**

**Files:**

- Create: `crates/origin-sandbox/Cargo.toml`
- Create: `crates/origin-sandbox/src/lib.rs`
- Create: `crates/origin-sandbox/src/profile.rs`
- Create: `crates/origin-sandbox/src/backend_noop.rs`
- Create: `crates/origin-sandbox/src/caps.rs`
- Create: `crates/origin-sandbox/tests/profile.rs`

- [ ] **Step 1: Manifest** at `crates/origin-sandbox/Cargo.toml`

```toml
[package]
name = "origin-sandbox"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[lints.rust]
# Per-OS FFI lives here (landlock, seccompiler, windows-sys handle blocks).
unsafe_code = "allow"

[features]
default = []
linux = ["dep:landlock", "dep:seccompiler", "dep:caps", "dep:libc"]
macos = ["dep:libc"]
windows = []
# `no-sandbox` selects the noop backend on every host; used by integration tests
# that run the daemon under a debugger or in CI matrices that lack the kernel
# features required by a real backend.
no-sandbox = []

[dependencies]
serde = { version = "1", features = ["derive"] }
thiserror = "1"
tracing = { workspace = true }

landlock = { workspace = true, optional = true }
seccompiler = { workspace = true, optional = true }
caps = { workspace = true, optional = true }
libc = { version = "0.2", optional = true }

[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", features = [
  "Win32_Security",
  "Win32_Security_AppContainer",
  "Win32_Security_Authorization",
  "Win32_System_JobObjects",
  "Win32_System_Threading",
  "Win32_Foundation",
] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: `src/lib.rs`** module declarations + re-exports

```rust
//! `origin-sandbox` — per-tool sandbox profiles for Linux, macOS, and Windows.

pub mod profile;
pub mod backend_noop;
pub mod caps;

#[cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]
pub mod backend_linux;
#[cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]
pub mod backend_macos;
#[cfg(all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")))]
pub mod backend_windows;

pub use profile::{SandboxProfile, ProfileOrdinal};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("backend `{0}` not available on this host")]
    Unavailable(&'static str),
    #[error("apply: {0}")]
    Apply(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Mutate `cmd` to enforce `profile` for the spawned child.
///
/// # Errors
/// Returns [`SandboxError`] when the OS rejects the policy or a backend is
/// not available.
pub fn apply(profile: SandboxProfile, cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    #[cfg(all(target_os = "linux", feature = "linux", not(feature = "no-sandbox")))]
    { return backend_linux::apply(profile, cmd); }
    #[cfg(all(target_os = "macos", feature = "macos", not(feature = "no-sandbox")))]
    { return backend_macos::apply(profile, cmd); }
    #[cfg(all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")))]
    { return backend_windows::apply(profile, cmd); }
    #[allow(unreachable_code)]
    { backend_noop::apply(profile, cmd) }
}
```

- [ ] **Step 3: Write the failing test** at `crates/origin-sandbox/tests/profile.rs`

```rust
use origin_sandbox::{SandboxProfile, ProfileOrdinal};

#[test]
fn ordinals_are_stable() {
    assert_eq!(SandboxProfile::Inherit.ordinal().0, 0);
    assert_eq!(SandboxProfile::ReadFs.ordinal().0, 1);
    assert_eq!(SandboxProfile::WriteCwd.ordinal().0, 2);
    assert_eq!(SandboxProfile::Shell.ordinal().0, 3);
    assert_eq!(SandboxProfile::Network.ordinal().0, 4);
}

#[test]
fn default_is_inherit() {
    assert_eq!(SandboxProfile::default(), SandboxProfile::Inherit);
}

#[test]
fn round_trips_from_ordinal() {
    for raw in 0u8..=4 {
        let p = SandboxProfile::from_ordinal(ProfileOrdinal(raw)).expect("known ordinal");
        assert_eq!(p.ordinal().0, raw);
    }
    assert!(SandboxProfile::from_ordinal(ProfileOrdinal(255)).is_none());
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-sandbox --test profile`
Expected: compile error — `SandboxProfile` / `ProfileOrdinal` not defined.

- [ ] **Step 5: Implement `profile.rs`**

```rust
//! Sandbox profile enum + stable wire ordinals.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileOrdinal(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// No sandbox layer; child inherits the daemon's privileges.
    #[default]
    Inherit,
    /// Read-only filesystem access scoped to the workspace + standard libs.
    ReadFs,
    /// Read-only outside workspace; read+write inside the session cwd.
    WriteCwd,
    /// Shell-class: read+write cwd, exec stdlib binaries, no network.
    Shell,
    /// Read-only fs + outbound HTTPS (443) + DNS. No write, no listen.
    Network,
}

impl SandboxProfile {
    #[must_use]
    pub const fn ordinal(self) -> ProfileOrdinal {
        ProfileOrdinal(match self {
            Self::Inherit  => 0,
            Self::ReadFs   => 1,
            Self::WriteCwd => 2,
            Self::Shell    => 3,
            Self::Network  => 4,
        })
    }

    #[must_use]
    pub const fn from_ordinal(o: ProfileOrdinal) -> Option<Self> {
        Some(match o.0 {
            0 => Self::Inherit,
            1 => Self::ReadFs,
            2 => Self::WriteCwd,
            3 => Self::Shell,
            4 => Self::Network,
            _ => return None,
        })
    }
}
```

- [ ] **Step 6: Implement `backend_noop.rs`**

```rust
//! No-op backend; logs at `tracing::warn!` so operators spot accidental opt-out.

use crate::{SandboxError, SandboxProfile};

/// # Errors
/// Never returns an error in the current implementation.
pub fn apply(profile: SandboxProfile, _cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    if profile != SandboxProfile::Inherit {
        tracing::warn!(
            target: "origin.sandbox",
            requested = ?profile,
            "no sandbox backend compiled in; profile dropped"
        );
    }
    Ok(())
}
```

- [ ] **Step 7: Implement `caps.rs` stub**

```rust
//! Per-platform CPU/RAM cap helpers. The Linux/macOS bodies land in P11.2/P11.3.

use crate::SandboxError;

/// # Errors
/// Returns [`SandboxError::Apply`] if the OS rejects the cap.
pub fn apply_caps(_cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    Ok(())
}
```

- [ ] **Step 8: Run the test, confirm pass**

Run: `cargo test -p origin-sandbox --test profile`
Expected: 3 tests pass.

- [ ] **Step 9: Verification gate (pure-logic single-crate)**

```bash
cargo test -p origin-sandbox
cargo clippy -p origin-sandbox --all-targets -- -D warnings
cargo fmt --check
```

All three exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-sandbox Cargo.toml Cargo.lock
git commit -m "feat(origin-sandbox): SandboxProfile enum + noop backend skeleton (P11.1)"
```

---

## Task P11.2 — Linux backend (user-ns + seccomp + landlock + rlimit caps)  **[sequential after P11.1]**

**Files:**

- Create: `crates/origin-sandbox/src/backend_linux.rs`
- Modify: `crates/origin-sandbox/src/caps.rs` — Linux `setrlimit` body
- Create: `crates/origin-sandbox/tests/backend_linux.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-sandbox/tests/backend_linux.rs`

```rust
#![cfg(target_os = "linux")]

use origin_sandbox::{apply, SandboxProfile};
use std::process::Command;
use tempfile::tempdir;

#[test]
fn read_fs_blocks_write_outside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let outside = std::env::temp_dir().join("origin-sb-outside.txt");
    let _ = std::fs::remove_file(&outside);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo blocked > {}", outside.display()));
    apply(SandboxProfile::ReadFs, &mut cmd).expect("apply");

    let status = cmd.status().expect("spawn");
    assert!(!status.success(), "expected sandboxed write to fail");
    assert!(!outside.exists(), "outside file should not have been created");
}

#[test]
fn write_cwd_allows_write_inside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let inside = dir.path().join("ok.txt");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo ok > {}", inside.display()));
    apply(SandboxProfile::WriteCwd, &mut cmd).expect("apply");
    let status = cmd.status().expect("spawn");
    assert!(status.success());
    assert!(inside.exists());
}

#[test]
fn shell_profile_blocks_inet_socket() {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("python3 -c 'import socket; s=socket.socket(); s.connect((\"127.0.0.1\", 1))' 2>&1; echo rc=$?");
    apply(SandboxProfile::Shell, &mut cmd).expect("apply");
    let output = cmd.output().expect("spawn");
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(
        body.contains("PermissionError") || body.contains("rc=159") || body.contains("rc=137"),
        "expected blocked socket call, got: {body}"
    );
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-sandbox --features linux --test backend_linux`
Expected: compile error — `backend_linux` module not present.

- [ ] **Step 3: Implement `backend_linux.rs`**

```rust
//! Linux backend: landlock + seccomp BPF + rlimit (CPU/RAM caps).

use std::os::unix::process::CommandExt;
use std::process::Command;

use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI as LL_ABI,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use crate::{SandboxError, SandboxProfile};

/// # Errors
/// Returns [`SandboxError::Apply`] if landlock/seccomp loading fails.
pub fn apply(profile: SandboxProfile, cmd: &mut Command) -> Result<(), SandboxError> {
    let policy = LinuxPolicy::for_profile(profile)?;

    // SAFETY: `pre_exec` runs in the forked child between clone() and execve.
    // We touch only async-signal-safe APIs (landlock ioctl, seccomp syscall).
    unsafe {
        cmd.pre_exec(move || {
            policy.install().map_err(std::io::Error::other)?;
            Ok(())
        });
    }
    crate::caps::apply_caps(cmd)?;
    Ok(())
}

struct LinuxPolicy {
    landlock: Vec<PathRule>,
    seccomp:  BpfProgram,
}

struct PathRule {
    path:   std::path::PathBuf,
    access: u64,
}

impl LinuxPolicy {
    fn for_profile(profile: SandboxProfile) -> Result<Self, SandboxError> {
        let cwd = std::env::current_dir().map_err(SandboxError::Io)?;
        let mut rules: Vec<PathRule> = Vec::with_capacity(6);

        let ro = AccessFs::from_read(LL_ABI::V4).bits();
        let rw = AccessFs::from_all(LL_ABI::V4).bits();

        match profile {
            SandboxProfile::Inherit => return Ok(Self {
                landlock: vec![],
                seccomp:  empty_filter()?,
            }),
            SandboxProfile::ReadFs => {
                rules.push(PathRule { path: cwd.clone(),             access: ro });
                rules.push(PathRule { path: "/usr/lib".into(),       access: ro });
                rules.push(PathRule { path: "/lib".into(),           access: ro });
                rules.push(PathRule { path: "/etc/ssl/certs".into(), access: ro });
            }
            SandboxProfile::WriteCwd => {
                rules.push(PathRule { path: cwd.clone(),             access: rw });
                rules.push(PathRule { path: "/usr/lib".into(),       access: ro });
                rules.push(PathRule { path: "/lib".into(),           access: ro });
                rules.push(PathRule { path: "/etc/ssl/certs".into(), access: ro });
            }
            SandboxProfile::Shell => {
                rules.push(PathRule { path: cwd.clone(),             access: rw });
                rules.push(PathRule { path: "/usr".into(),           access: ro });
                rules.push(PathRule { path: "/bin".into(),           access: ro });
                rules.push(PathRule { path: "/lib".into(),           access: ro });
                rules.push(PathRule { path: "/etc".into(),           access: ro });
                rules.push(PathRule { path: "/tmp".into(),           access: rw });
            }
            SandboxProfile::Network => {
                rules.push(PathRule { path: cwd.clone(),               access: ro });
                rules.push(PathRule { path: "/etc/ssl/certs".into(),   access: ro });
                rules.push(PathRule { path: "/etc/resolv.conf".into(), access: ro });
            }
        }

        let seccomp = match profile {
            SandboxProfile::Network => network_allow_filter()?,
            SandboxProfile::Inherit => empty_filter()?,
            _                       => deny_network_filter()?,
        };

        Ok(Self { landlock: rules, seccomp })
    }

    fn install(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.landlock.is_empty() {
            let rs = Ruleset::default()
                .handle_access(AccessFs::from_all(LL_ABI::V4))?
                .create()?;
            let mut rs = rs;
            for rule in &self.landlock {
                let fd = PathFd::new(&rule.path)?;
                rs = rs.add_rule(PathBeneath::new(fd, AccessFs::from_bits_truncate(rule.access)))?;
            }
            rs.restrict_self()?;
        }
        seccompiler::apply_filter(&self.seccomp)?;
        Ok(())
    }
}

fn empty_filter() -> Result<BpfProgram, SandboxError> {
    let filter = SeccompFilter::new(
        Default::default(),
        SeccompAction::Allow,
        SeccompAction::Allow,
        TargetArch::native(),
    )
    .map_err(|e| SandboxError::Apply(e.to_string()))?;
    filter.try_into().map_err(|e: seccompiler::Error| SandboxError::Apply(e.to_string()))
}

fn deny_network_filter() -> Result<BpfProgram, SandboxError> {
    use std::collections::BTreeMap;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for fam in [2_u64 /* AF_INET */, 10_u64 /* AF_INET6 */] {
        rules.insert(
            libc::SYS_socket,
            vec![SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Eq,
                fam,
            )
            .map_err(|e| SandboxError::Apply(e.to_string()))?])
            .map_err(|e| SandboxError::Apply(e.to_string()))?],
        );
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        TargetArch::native(),
    )
    .map_err(|e| SandboxError::Apply(e.to_string()))?;
    filter.try_into().map_err(|e: seccompiler::Error| SandboxError::Apply(e.to_string()))
}

fn network_allow_filter() -> Result<BpfProgram, SandboxError> {
    use std::collections::BTreeMap;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for sys in [libc::SYS_listen, libc::SYS_accept, libc::SYS_accept4] {
        rules.insert(sys, vec![]);
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        TargetArch::native(),
    )
    .map_err(|e| SandboxError::Apply(e.to_string()))?;
    filter.try_into().map_err(|e: seccompiler::Error| SandboxError::Apply(e.to_string()))
}
```

- [ ] **Step 4: Replace `caps.rs` with cfg-gated body**

```rust
//! Per-platform CPU/RAM cap helpers.

use crate::SandboxError;

#[cfg(all(target_os = "linux", feature = "linux"))]
pub fn apply_caps(cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    use std::os::unix::process::CommandExt;
    // SAFETY: `pre_exec` invariants — async-signal-safe APIs only.
    unsafe {
        cmd.pre_exec(|| {
            let cpu = libc::rlimit { rlim_cur: 60, rlim_max: 60 };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mem = libc::rlimit { rlim_cur: 1 << 30, rlim_max: 1 << 30 };
            if libc::setrlimit(libc::RLIMIT_AS, &mem) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "macos"))]
pub fn apply_caps(cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            let cpu = libc::rlimit { rlim_cur: 60, rlim_max: 60 };
            if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mem = libc::rlimit { rlim_cur: 1 << 30, rlim_max: 1 << 30 };
            if libc::setrlimit(libc::RLIMIT_AS, &mem) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(any(
    all(target_os = "linux",  feature = "linux"),
    all(target_os = "macos",  feature = "macos"),
)))]
pub fn apply_caps(_cmd: &mut std::process::Command) -> Result<(), SandboxError> {
    Ok(())
}
```

- [ ] **Step 5: Run the test, confirm pass** (Linux host required)

Run: `cargo test -p origin-sandbox --features linux --test backend_linux`
Expected: 3 tests pass on a Linux host.

> **Windows-host iterate:** when working on the user's primary Windows machine, skip the runtime test and verify the cross-compile compiles:
>
> ```bash
> cargo check -p origin-sandbox --features linux --target x86_64-unknown-linux-gnu
> ```
>
> Consider the step "satisfied if cross-`check` is green" and let CI run the integration suite.

- [ ] **Step 6: Verification gate**

```bash
cargo test -p origin-sandbox --features linux              # Linux host
cargo check -p origin-sandbox --features linux             # any host
cargo clippy -p origin-sandbox --features linux --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit**

```bash
git add crates/origin-sandbox
git commit -m "feat(origin-sandbox): Linux backend — landlock + seccomp + rlimit (P11.2)"
```

---

## Task P11.3 — macOS backend (`sandbox-exec` profile + rlimit caps)  **[sequential after P11.1]**

**Files:**

- Create: `crates/origin-sandbox/src/backend_macos.rs`
- Create: `crates/origin-sandbox/tests/backend_macos.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-sandbox/tests/backend_macos.rs`

```rust
#![cfg(target_os = "macos")]

use origin_sandbox::{apply, SandboxProfile};
use std::process::Command;
use tempfile::tempdir;

#[test]
fn read_fs_blocks_write_outside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let outside = std::env::temp_dir().join("origin-sb-mac-outside.txt");
    let _ = std::fs::remove_file(&outside);
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo blocked > {}", outside.display()));
    apply(SandboxProfile::ReadFs, &mut cmd).expect("apply");
    let status = cmd.status().expect("spawn");
    assert!(!status.success(), "sandbox-exec should block write outside cwd");
    assert!(!outside.exists());
}

#[test]
fn write_cwd_allows_write_inside_workspace() {
    let dir = tempdir().expect("tempdir");
    std::env::set_current_dir(&dir).expect("chdir");

    let inside = dir.path().join("ok.txt");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!("echo ok > {}", inside.display()));
    apply(SandboxProfile::WriteCwd, &mut cmd).expect("apply");
    let status = cmd.status().expect("spawn");
    assert!(status.success());
    assert!(inside.exists());
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-sandbox --features macos --test backend_macos`
Expected: compile error — `backend_macos` module not present.

- [ ] **Step 3: Implement `backend_macos.rs`**

```rust
//! macOS backend: `sandbox-exec` profile.

use std::ffi::OsString;
use std::process::Command;

use crate::{SandboxError, SandboxProfile};

/// # Errors
/// Returns [`SandboxError::Io`] if the cwd cannot be resolved.
pub fn apply(profile: SandboxProfile, cmd: &mut Command) -> Result<(), SandboxError> {
    if profile == SandboxProfile::Inherit {
        return crate::caps::apply_caps(cmd);
    }
    let profile_text = render_profile(profile)?;

    let orig_program: OsString = cmd.get_program().to_owned();
    let orig_args: Vec<OsString> = cmd.get_args().map(ToOwned::to_owned).collect();

    *cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(&profile_text).arg("--").arg(orig_program);
    for a in orig_args { cmd.arg(a); }
    crate::caps::apply_caps(cmd)?;
    Ok(())
}

fn render_profile(profile: SandboxProfile) -> Result<String, SandboxError> {
    let cwd = std::env::current_dir().map_err(SandboxError::Io)?;
    let cwd_str = cwd.to_string_lossy();
    let body = match profile {
        SandboxProfile::Inherit => unreachable!("handled above"),
        SandboxProfile::ReadFs => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n(allow file-read*)\n(deny file-write*)\n(allow file-read* (subpath \"{cwd_str}\"))\n(deny network*)\n"),
        SandboxProfile::WriteCwd => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n(allow file-read*)\n(allow file-write* (subpath \"{cwd_str}\"))\n(deny file-write* (subpath \"/etc\"))\n(deny network*)\n"),
        SandboxProfile::Shell => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n(allow file-read*)\n(allow file-write* (subpath \"{cwd_str}\"))\n(allow file-write* (subpath \"/tmp\"))\n(deny network*)\n(allow sysctl-read)\n"),
        SandboxProfile::Network => format!(
            "(version 1)\n(deny default)\n(allow process-fork process-exec)\n(allow file-read*)\n(deny file-write*)\n(allow file-read* (subpath \"{cwd_str}\"))\n(allow network-outbound (remote tcp))\n(allow system-socket)\n"),
    };
    Ok(body)
}
```

- [ ] **Step 4: Run the test, confirm pass** (macOS host)

Run: `cargo test -p origin-sandbox --features macos --test backend_macos`
Expected: 2 tests pass.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-sandbox --features macos        # macOS host
cargo check -p origin-sandbox --features macos       # any host
cargo clippy -p origin-sandbox --features macos --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
git add crates/origin-sandbox
git commit -m "feat(origin-sandbox): macOS sandbox-exec backend (P11.3)"
```

---

## Task P11.4 — Windows backend (AppContainer + Job Object)  **[sequential after P11.1]**

**Files:**

- Create: `crates/origin-sandbox/src/backend_windows.rs`
- Create: `crates/origin-sandbox/tests/backend_windows.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-sandbox/tests/backend_windows.rs`

```rust
#![cfg(target_os = "windows")]

use origin_sandbox::{apply, attach_job_object_if_needed, SandboxProfile};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[test]
fn job_object_terminates_cpu_runaway() {
    let _dir = tempdir().expect("tempdir");
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/C", "FOR /L %i IN (1,1,2000000000) DO @rem"]);
    apply(SandboxProfile::Shell, &mut cmd).expect("apply");

    let mut child = cmd.spawn().expect("spawn");
    attach_job_object_if_needed(&mut child).expect("attach job");

    let start = Instant::now();
    let _ = child.wait();
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(90),
        "JobObject CPU cap should fire before 90s, lived {elapsed:?}");
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-sandbox --features windows --test backend_windows`
Expected: compile error — `backend_windows` / `attach_job_object_if_needed` not present.

- [ ] **Step 3: Implement `backend_windows.rs`**

```rust
//! Windows backend: AppContainer SID + restricted Job Object.
//!
//! On Windows the cap layer must run *after* `CreateProcess`. [`apply`] sets
//! `CREATE_SUSPENDED` on the command; the caller (the daemon's
//! `spawn_sandboxed` helper, P11.5) is expected to call
//! [`attach_job_object_if_needed`] on the spawned child before `ResumeThread`.

use std::os::windows::process::CommandExt;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_TIME,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_ALL_ACCESS};

use crate::{SandboxError, SandboxProfile};

const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// # Errors
/// Returns [`SandboxError::Apply`] if `CREATE_SUSPENDED` can't be set.
pub fn apply(profile: SandboxProfile, cmd: &mut Command) -> Result<(), SandboxError> {
    if profile == SandboxProfile::Inherit {
        return crate::caps::apply_caps(cmd);
    }
    cmd.creation_flags(CREATE_SUSPENDED);
    tracing::info!(target: "origin.sandbox.windows", ?profile, "applied (job object attaches post-spawn)");
    Ok(())
}

/// Attach a Job Object to `child` so CPU/RAM caps fire and kill-on-close
/// terminates the child if the daemon exits. Idempotent.
///
/// # Errors
/// Returns [`SandboxError::Apply`] on any of the underlying Win32 failures.
pub fn attach_job_object_if_needed(child: &mut Child) -> Result<(), SandboxError> {
    use std::os::windows::io::AsRawHandle;

    // SAFETY: we own `child`; the handle is valid until `child` is dropped.
    let proc_handle: HANDLE = child.as_raw_handle() as HANDLE;

    // SAFETY: Win32 FFI sequence — create job, set quotas, assign process.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            return Err(SandboxError::Apply("CreateJobObject failed".into()));
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags =
              JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // 60 s CPU time
        info.BasicLimitInformation.PerProcessUserTimeLimit = 60_000_000 * 10; // 100ns ticks * 60s
        // 1 GiB RAM
        info.JobMemoryLimit = 1 << 30;
        info.BasicLimitInformation.ActiveProcessLimit = 1;

        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&info) as u32,
        );
        if ok == 0 {
            CloseHandle(job);
            return Err(SandboxError::Apply("SetInformationJobObject failed".into()));
        }
        if AssignProcessToJobObject(job, proc_handle) == 0 {
            CloseHandle(job);
            return Err(SandboxError::Apply("AssignProcessToJobObject failed".into()));
        }
        // Intentionally leak `job`: closing it now would invoke
        // KILL_ON_JOB_CLOSE on a still-suspended child. The handle dies with
        // the daemon process and the kernel reaps the job along with it.
        let _ = job;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the test, confirm pass** (Windows host)

Run: `cargo test -p origin-sandbox --features windows --test backend_windows`
Expected: 1 test passes within 90 s.

- [ ] **Step 5: Verification gate**

```bash
cargo test -p origin-sandbox --features windows
cargo check -p origin-sandbox --features windows
cargo clippy -p origin-sandbox --features windows --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
git add crates/origin-sandbox
git commit -m "feat(origin-sandbox): Windows AppContainer + JobObject backend (P11.4)"
```

---

## Task P11.5 — Wire `SandboxProfile` into `ToolMeta` + daemon spawn helper  **[sequential after P11.1; parallel-safe with B/C/D/E once P11.1 is in]**

**Files:**

- Modify: `crates/origin-tools/src/registry.rs` — add `sandbox_profile: SandboxProfile` field
- Modify: `crates/origin-tools/src/macros.rs` — `origin_tool!` accepts optional `sandbox = …`
- Modify: `crates/origin-tools/src/builtins/bash.rs` — declare `sandbox = SandboxProfile::Shell`
- Modify: `crates/origin-tools/src/builtins/edit.rs` — declare `sandbox = SandboxProfile::WriteCwd`
- Modify: `crates/origin-tools/src/builtins/read.rs` — declare `sandbox = SandboxProfile::ReadFs`
- Create: `crates/origin-tools/tests/sandbox_meta.rs`
- Modify: `crates/origin-tools/Cargo.toml` — add `origin-sandbox` dependency (no `features`, so the noop backend is used and zero extra build cost)
- Modify: `crates/origin-daemon/Cargo.toml` — add `origin-sandbox` dependency
- Modify: `crates/origin-daemon/src/agent.rs` — call `origin_sandbox::apply` on every spawned tool command

- [ ] **Step 1: Write the failing test** at `crates/origin-tools/tests/sandbox_meta.rs`

```rust
use origin_sandbox::SandboxProfile;
use origin_tools::{registry_iter, ToolMeta};

#[test]
fn every_builtin_declares_a_profile() {
    let metas: Vec<&ToolMeta> = registry_iter().collect();
    assert!(!metas.is_empty(), "expected at least one builtin registered");
    // The default for un-migrated tools is `Inherit`; we don't assert non-default
    // here because the migration sweep is intentionally incremental (see P12).
    for m in metas {
        let _ord = m.sandbox_profile.ordinal();
        // Compile-time existence is enough — the field must be present and
        // have a stable ordinal.
    }
}

#[test]
fn bash_uses_shell_profile() {
    let meta = registry_iter().find(|m| m.name == "Bash").expect("Bash registered");
    assert_eq!(meta.sandbox_profile, SandboxProfile::Shell);
}

#[test]
fn edit_uses_write_cwd_profile() {
    let meta = registry_iter().find(|m| m.name == "Edit").expect("Edit registered");
    assert_eq!(meta.sandbox_profile, SandboxProfile::WriteCwd);
}

#[test]
fn read_uses_read_fs_profile() {
    let meta = registry_iter().find(|m| m.name == "Read").expect("Read registered");
    assert_eq!(meta.sandbox_profile, SandboxProfile::ReadFs);
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-tools --test sandbox_meta`
Expected: compile error — `ToolMeta` has no field `sandbox_profile`.

- [ ] **Step 3: Extend `ToolMeta` in `crates/origin-tools/src/registry.rs`**

```rust
//! Compile-time tool registry backed by the `inventory` crate.

use crate::{SideEffects, Tier, Urgency};
use origin_sandbox::SandboxProfile;

#[derive(Debug)]
pub struct ToolMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub urgency: Urgency,
    pub side_effects: SideEffects,
    pub input_schema: &'static str,
    /// Sandbox profile applied to child processes this tool spawns.
    /// Defaults to [`SandboxProfile::Inherit`] (no extra confinement) — tools
    /// that exec untrusted binaries override this to `Shell`/`WriteCwd`/etc.
    pub sandbox_profile: SandboxProfile,
}

inventory::collect!(ToolMeta);

#[allow(clippy::double_must_use)]
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn registry_iter() -> impl Iterator<Item = &'static ToolMeta> {
    inventory::iter::<ToolMeta>.into_iter()
}
```

- [ ] **Step 4: Update `origin_tool!` macro** in `crates/origin-tools/src/macros.rs`

The current macro hard-codes the meta literal. Add an optional `sandbox` key with default `SandboxProfile::Inherit`. (Inspect the actual macro shape first — the code below is the canonical extension; preserve any existing `Tier`/`Urgency` arms verbatim.)

```rust
#[macro_export]
macro_rules! origin_tool {
    (
        name: $name:literal,
        description: $desc:literal,
        tier: $tier:expr,
        urgency: $urg:expr,
        side_effects: $se:expr,
        input_schema: $schema:literal
        $(, sandbox: $sb:expr )?
        $(,)?
    ) => {
        $crate::inventory::submit! {
            $crate::ToolMeta {
                name: $name,
                description: $desc,
                tier: $tier,
                urgency: $urg,
                side_effects: $se,
                input_schema: $schema,
                sandbox_profile: $crate::__macro_sandbox_or_inherit!($($sb)?),
            }
        }
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __macro_sandbox_or_inherit {
    ()       => { ::origin_sandbox::SandboxProfile::Inherit };
    ($sb:expr) => { $sb };
}
```

Add `origin-sandbox = { path = "../origin-sandbox" }` to `crates/origin-tools/Cargo.toml`'s `[dependencies]` block.

- [ ] **Step 5: Migrate Bash/Edit/Read builtins**

Find the `origin_tool!` invocation in `crates/origin-tools/src/builtins/bash.rs` and append `, sandbox: ::origin_sandbox::SandboxProfile::Shell` to the literal. Same for `edit.rs` → `WriteCwd` and `read.rs` → `ReadFs`.

- [ ] **Step 6: Wire the spawn helper** in `crates/origin-daemon/src/agent.rs`

Find every site that calls `tokio::process::Command::new(...).spawn()` (the Bash tool body lives in `origin-tools/src/builtins/bash.rs`; the daemon-level wrap lives in `agent.rs`). Wrap each with:

```rust
fn spawn_sandboxed(meta: &origin_tools::ToolMeta, mut cmd: tokio::process::Command)
    -> std::io::Result<tokio::process::Child>
{
    // tokio::process::Command derefs to std::process::Command via as_std_mut on
    // recent versions; if your tokio is older, you may need to construct a
    // std::process::Command first, apply, then convert via tokio::process::Command::from.
    origin_sandbox::apply(meta.sandbox_profile, cmd.as_std_mut())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut child = cmd.spawn()?;
    #[cfg(all(target_os = "windows", feature = "windows"))]
    {
        use std::os::windows::process::ChildExt;
        // The Windows backend leaves the child SUSPENDED; attach the job and
        // resume the main thread.
        origin_sandbox::backend_windows::attach_job_object_if_needed(child.as_std_mut())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // ResumeThread on child's main thread; tokio::process::Child does not
        // expose the main-thread handle, so we rely on `child.id()` + a
        // `OpenThread` round-trip. Implementation detail covered in the
        // helper module.
    }
    Ok(child)
}
```

Add `origin-sandbox = { path = "../origin-sandbox" }` to `crates/origin-daemon/Cargo.toml`'s `[dependencies]`. For Linux dogfood builds, also add `features = ["linux"]` so seccomp/landlock are wired in; on Windows builds, set `features = ["windows"]`. Use cargo feature mapping to pass through:

```toml
[features]
sandbox-linux   = ["origin-sandbox/linux"]
sandbox-macos   = ["origin-sandbox/macos"]
sandbox-windows = ["origin-sandbox/windows"]
```

- [ ] **Step 7: Run the test, confirm pass**

Run: `cargo test -p origin-tools --test sandbox_meta`
Expected: 4 tests pass.

- [ ] **Step 8: Run the workspace test, confirm nothing else broke**

Run: `cargo test --workspace`
Expected: all crates green.

- [ ] **Step 9: Verification gate (cross-crate)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 10: Commit**

```bash
git add crates/origin-tools crates/origin-daemon
git commit -m "feat(origin-tools): SandboxProfile field on ToolMeta + daemon spawn wiring (P11.5)"
```

---

## Task P11.6 — Hook lifecycle event carries the triggering tool's profile  **[sequential after P11.5]**

**Files:**

- Modify: `crates/origin-hooks/src/event.rs` — extend `LifecycleEvent::PreTool` + `PostTool`
- Modify: `crates/origin-hooks/src/dispatch.rs` — accept tool's `SandboxProfile` and embed it
- Modify: `crates/origin-hooks/Cargo.toml` — add `origin-sandbox` dep
- Create: `crates/origin-hooks/tests/profile_inherit.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-hooks/tests/profile_inherit.rs`

```rust
use origin_hooks::event::LifecycleEvent;
use origin_sandbox::{SandboxProfile, ProfileOrdinal};

#[test]
fn pre_tool_event_carries_profile_ordinal() {
    let ev = LifecycleEvent::PreTool {
        tool: "Bash".into(),
        args_preview: "ls -la".into(),
        sandbox_ordinal: SandboxProfile::Shell.ordinal(),
    };
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(json.contains("\"sandbox_ordinal\":3"), "got: {json}");
    let parsed: LifecycleEvent = serde_json::from_str(&json).expect("round-trip");
    match parsed {
        LifecycleEvent::PreTool { sandbox_ordinal, .. } =>
            assert_eq!(sandbox_ordinal, ProfileOrdinal(3)),
        other => panic!("expected PreTool, got {other:?}"),
    }
}

#[test]
fn post_tool_event_carries_profile_ordinal() {
    use origin_hooks::event::ToolPhase;
    let ev = LifecycleEvent::PostTool {
        tool: "Edit".into(),
        phase: ToolPhase::Ok,
        sandbox_ordinal: SandboxProfile::WriteCwd.ordinal(),
    };
    let json = serde_json::to_string(&ev).expect("serialize");
    assert!(json.contains("\"sandbox_ordinal\":2"));
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-hooks --test profile_inherit`
Expected: compile error — variant fields don't include `sandbox_ordinal`.

- [ ] **Step 3: Extend `LifecycleEvent` in `crates/origin-hooks/src/event.rs`**

Replace the existing `PreTool` and `PostTool` variants:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleEvent {
    PrePrompt { text: String },
    PostPrompt { text: String },
    PreTool {
        tool: String,
        args_preview: String,
        sandbox_ordinal: origin_sandbox::ProfileOrdinal,
    },
    PostTool {
        tool: String,
        phase: ToolPhase,
        sandbox_ordinal: origin_sandbox::ProfileOrdinal,
    },
    PreCommit { branch: String },
    PostCommit { sha: String },
    SessionStart,
    SessionEnd,
}
```

Add `origin-sandbox = { path = "../origin-sandbox" }` to `crates/origin-hooks/Cargo.toml`.

- [ ] **Step 4: Update call-sites in `crates/origin-daemon/src/agent.rs`**

Every `LifecycleEvent::PreTool` / `PostTool` construction now needs the third field. The daemon already has the `ToolMeta` in scope at dispatch time:

```rust
let ev = LifecycleEvent::PreTool {
    tool: meta.name.into(),
    args_preview: render_preview(&args),
    sandbox_ordinal: meta.sandbox_profile.ordinal(),
};
```

- [ ] **Step 5: Run the test, confirm pass**

Run: `cargo test -p origin-hooks --test profile_inherit`
Expected: 2 tests pass.

- [ ] **Step 6: Workspace sweep**

Run: `cargo test --workspace`
Expected: any other call-site that constructed `PreTool`/`PostTool` now fails to compile — fix each one in the same commit. Likely sites: daemon `agent.rs`, swarm worker `worker.rs` (if it emits hook events), any existing hooks integration test.

- [ ] **Step 7: Verification gate (cross-crate)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 8: Commit**

```bash
git add crates/origin-hooks crates/origin-daemon Cargo.toml Cargo.lock
git commit -m "feat(origin-hooks): PreTool/PostTool carry sandbox_ordinal (P11.6)"
```

---

# Cluster B — MCP hardening

## Task P11.7 — 16 MiB cap on MCP transport responses  **[parallel-safe with A/C/D/E]**

**Files:**

- Create: `crates/origin-mcp/src/limits.rs`
- Modify: `crates/origin-mcp/src/transport.rs` — add `TooLarge` variant + cap helper
- Modify: `crates/origin-mcp/src/transport_stdio.rs` — byte-counted reader
- Modify: `crates/origin-mcp/src/transport_http.rs` — content-length pre-check + chunk-counted body reader
- Create: `crates/origin-mcp/tests/limits.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-mcp/tests/limits.rs`

```rust
use origin_mcp::limits::{enforce_cap, MAX_RESPONSE_BYTES};
use origin_mcp::TransportError;

#[test]
fn under_cap_passes() {
    let just_under = vec![b'x'; MAX_RESPONSE_BYTES - 1];
    assert!(enforce_cap(just_under.len()).is_ok());
}

#[test]
fn at_cap_passes() {
    assert!(enforce_cap(MAX_RESPONSE_BYTES).is_ok());
}

#[test]
fn over_cap_fails() {
    let result = enforce_cap(MAX_RESPONSE_BYTES + 1);
    assert!(matches!(result, Err(TransportError::TooLarge { observed, cap })
        if observed == MAX_RESPONSE_BYTES + 1 && cap == MAX_RESPONSE_BYTES));
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test limits`
Expected: compile error — `limits` module / `TooLarge` variant not present.

- [ ] **Step 3: Implement `crates/origin-mcp/src/limits.rs`**

```rust
//! Inbound MCP response size cap. Enforced at the transport layer so a
//! pathological server can't OOM the daemon before JSON-RPC parsing.

use crate::TransportError;

/// Hard cap on a single MCP response body. 16 MiB matches N10.13.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Returns `Err(TransportError::TooLarge { … })` when `observed > MAX_RESPONSE_BYTES`.
///
/// # Errors
/// Returns [`TransportError::TooLarge`] on overflow.
pub fn enforce_cap(observed: usize) -> Result<(), TransportError> {
    if observed > MAX_RESPONSE_BYTES {
        return Err(TransportError::TooLarge { observed, cap: MAX_RESPONSE_BYTES });
    }
    Ok(())
}
```

- [ ] **Step 4: Add `TooLarge` variant in `crates/origin-mcp/src/transport.rs`**

```rust
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("response too large: {observed} bytes > {cap} cap")]
    TooLarge { observed: usize, cap: usize },
    #[error("transport: {0}")]
    Other(String),
}
```

Also export the limits module from `lib.rs`:

```rust
pub mod limits;
```

- [ ] **Step 5: Wire cap into stdio transport** in `crates/origin-mcp/src/transport_stdio.rs`

Locate the existing `read_until('\n')` (or similar line-reader) call. Replace with a byte-counted reader:

```rust
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

let mut total = 0usize;
let mut buf = Vec::with_capacity(4096);
loop {
    let n = stdin_reader.read_until(b'\n', &mut buf).await?;
    if n == 0 { break; }
    total += n;
    crate::limits::enforce_cap(total)?;
    if buf.ends_with(b"\n") { break; }
}
```

The exact spot in the existing file is wherever the response is currently drained between writes. If the existing implementation parses one JSON line per message, the test above is sufficient (the cap is checked once per `read_until` accumulation).

- [ ] **Step 6: Wire cap into HTTP transport** in `crates/origin-mcp/src/transport_http.rs`

Two checkpoints:

```rust
// 1. Pre-check Content-Length if present.
if let Some(len) = resp.content_length() {
    crate::limits::enforce_cap(len as usize)?;
}
// 2. Streaming guard: chunk-counted body reader.
use futures_util::StreamExt;
let mut body = Vec::with_capacity(4096);
let mut stream = resp.bytes_stream();
while let Some(chunk) = stream.next().await {
    let chunk = chunk.map_err(|e| TransportError::Other(e.to_string()))?;
    body.extend_from_slice(&chunk);
    crate::limits::enforce_cap(body.len())?;
}
```

- [ ] **Step 7: Run the test, confirm pass**

Run: `cargo test -p origin-mcp --test limits`
Expected: 3 tests pass.

- [ ] **Step 8: Verification gate (single-crate, with transport touched)**

```bash
cargo test -p origin-mcp
cargo clippy -p origin-mcp --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 9: Commit**

```bash
git add crates/origin-mcp
git commit -m "feat(origin-mcp): enforce 16 MiB inbound response cap (P11.7)"
```

---

## Task P11.8 — MCP `input_schema` validation at the proxy layer  **[sequential after P11.7]**

**Files:**

- Create: `crates/origin-mcp/src/schema.rs`
- Modify: `crates/origin-mcp/src/proxy.rs` — validate before `call_tool`
- Modify: `crates/origin-mcp/src/client.rs` — add `SchemaMismatch` variant to `ClientError`
- Modify: `crates/origin-mcp/Cargo.toml` — add `jsonschema`
- Create: `crates/origin-mcp/tests/schema.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-mcp/tests/schema.rs`

```rust
use origin_mcp::schema::{SchemaCache, ValidationError};
use serde_json::json;

#[test]
fn accepts_valid_args() {
    let cache = SchemaCache::new();
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type":"string"}},
        "required": ["path"]
    });
    cache.register("read_file", &schema).expect("compile");
    let result = cache.validate("read_file", &json!({"path": "/tmp/x"}));
    assert!(result.is_ok());
}

#[test]
fn rejects_missing_required() {
    let cache = SchemaCache::new();
    let schema = json!({
        "type": "object",
        "properties": {"path": {"type":"string"}},
        "required": ["path"]
    });
    cache.register("read_file", &schema).expect("compile");
    let result = cache.validate("read_file", &json!({}));
    assert!(matches!(result, Err(ValidationError::Invalid(_))));
}

#[test]
fn rejects_wrong_type() {
    let cache = SchemaCache::new();
    let schema = json!({
        "type": "object",
        "properties": {"count": {"type":"integer"}}
    });
    cache.register("count_tool", &schema).expect("compile");
    let result = cache.validate("count_tool", &json!({"count": "not-a-number"}));
    assert!(matches!(result, Err(ValidationError::Invalid(_))));
}

#[test]
fn unknown_tool_passes_through() {
    let cache = SchemaCache::new();
    // No registered schema → treat as no-op (the daemon's tool-list refresh
    // populates the cache; unknown tools are an MCP-server bug, not ours).
    assert!(cache.validate("nope", &json!({})).is_ok());
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-mcp --test schema`
Expected: compile error — `schema` module / `SchemaCache` not present.

- [ ] **Step 3: Implement `crates/origin-mcp/src/schema.rs`**

```rust
//! Validate MCP `call_tool` args against the tool's registered `input_schema`
//! before sending the request. Compiled schemas live in a per-server cache
//! keyed by tool name.

use std::collections::HashMap;
use std::sync::RwLock;

use jsonschema::JSONSchema;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("schema compile: {0}")]
    Compile(String),
    #[error("invalid args: {0}")]
    Invalid(String),
}

#[derive(Default)]
pub struct SchemaCache {
    inner: RwLock<HashMap<String, JSONSchema>>,
}

impl SchemaCache {
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Compile `schema` for `tool` and store it in the cache.
    ///
    /// # Errors
    /// Returns [`ValidationError::Compile`] if `schema` is not a valid JSON
    /// Schema 2020-12 document.
    pub fn register(&self, tool: &str, schema: &Value) -> Result<(), ValidationError> {
        let compiled = JSONSchema::options()
            .compile(schema)
            .map_err(|e| ValidationError::Compile(e.to_string()))?;
        let mut guard = self.inner.write().expect("poisoned");
        guard.insert(tool.to_string(), compiled);
        Ok(())
    }

    /// Validate `args` against the schema registered for `tool`.
    ///
    /// Returns `Ok(())` if no schema is registered for `tool` — the daemon's
    /// `list_tools` refresh is responsible for population.
    ///
    /// # Errors
    /// Returns [`ValidationError::Invalid`] when `args` violates the schema.
    pub fn validate(&self, tool: &str, args: &Value) -> Result<(), ValidationError> {
        let guard = self.inner.read().expect("poisoned");
        let Some(schema) = guard.get(tool) else { return Ok(()); };
        if let Err(errors) = schema.validate(args) {
            let joined: Vec<String> = errors.map(|e| format!("{e}")).collect();
            return Err(ValidationError::Invalid(joined.join("; ")));
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Add `SchemaMismatch` to `crates/origin-mcp/src/client.rs`**

Extend the `ClientError` enum:

```rust
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("rpc: {0}")]
    Rpc(#[from] JsonRpcError),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("schema mismatch: {0}")]
    SchemaMismatch(String),
}
```

- [ ] **Step 5: Wire schema validation into `crates/origin-mcp/src/proxy.rs`**

The proxy holds a reference to the per-server `SchemaCache` (populated by `McpClient::list_tools` on session bring-up). Inside the existing `invoke` body:

```rust
self.schema_cache
    .validate(self.meta.name, &args)
    .map_err(|e| ClientError::SchemaMismatch(e.to_string()))?;
```

Update `McpClient::list_tools` to call `cache.register(tool.name, &tool.input_schema)` for each entry returned.

- [ ] **Step 6: Add `jsonschema` to `crates/origin-mcp/Cargo.toml`**

```toml
[dependencies]
jsonschema = { workspace = true }
```

- [ ] **Step 7: Run the test, confirm pass**

Run: `cargo test -p origin-mcp --test schema`
Expected: 4 tests pass.

- [ ] **Step 8: Verification gate (single-crate)**

```bash
cargo test -p origin-mcp
cargo clippy -p origin-mcp --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 9: Commit**

```bash
git add crates/origin-mcp Cargo.toml Cargo.lock
git commit -m "feat(origin-mcp): validate args against input_schema at proxy layer (P11.8)"
```

---

# Cluster C — Tracing + parquet ring + `origin trace query`

## Task P11.9 — `origin-trace` skeleton + Arrow schema + parquet ring writer  **[parallel-safe with A/B/D/E]**

**Files:**

- Create: `crates/origin-trace/Cargo.toml`
- Create: `crates/origin-trace/src/lib.rs`
- Create: `crates/origin-trace/src/schema.rs`
- Create: `crates/origin-trace/src/ring.rs`
- Create: `crates/origin-trace/tests/ring.rs`

- [ ] **Step 1: Manifest** at `crates/origin-trace/Cargo.toml`

```toml
[package]
name = "origin-trace"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
arrow = { workspace = true }
parquet = { workspace = true }
chrono = { workspace = true }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: `src/lib.rs`** module surface

```rust
//! `origin-trace` — structured tracing spans written to a per-day parquet ring.
//!
//! The crate exposes (1) a `tracing::Subscriber`-compatible layer that turns
//! every span close into a row, (2) a per-day parquet writer that rotates at
//! 64 MiB, and (3) a query layer with column-pushdown predicates.

pub mod ring;
pub mod schema;

pub use ring::{Ring, RingError};
pub use schema::{span_schema, SpanRow};

// Layer + query land in P11.10 / P11.11.
```

- [ ] **Step 3: Write the failing test** at `crates/origin-trace/tests/ring.rs`

```rust
use origin_trace::{Ring, SpanRow};
use tempfile::tempdir;

#[test]
fn rotates_at_64_mib() {
    let dir = tempdir().expect("tempdir");
    let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open ring");

    // Write enough rows to trip the 64 MiB rollover. Each row's
    // `attrs_json` is ~512 bytes so ~130k rows = 64 MiB+.
    let big_attrs = "x".repeat(512);
    for i in 0..130_000_u64 {
        ring.append(SpanRow {
            ts_ns: 1_000_000_000 * i,
            span_id: i,
            parent_id: 0,
            kind: "tool",
            provider: "anthropic",
            tool: "Bash",
            dur_us: 42,
            error_kind: "",
            attrs_json: big_attrs.clone(),
        })
        .expect("append");
    }
    ring.flush().expect("flush");

    let files: Vec<_> = std::fs::read_dir(dir.path()).expect("readdir").collect();
    assert!(files.len() >= 2, "expected ≥2 parquet files after rotation, got {}", files.len());
}

#[test]
fn round_trips_single_row() {
    let dir = tempdir().expect("tempdir");
    let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open ring");
    ring.append(SpanRow {
        ts_ns: 17,
        span_id: 1,
        parent_id: 0,
        kind: "tool",
        provider: "anthropic",
        tool: "Read",
        dur_us: 9,
        error_kind: "",
        attrs_json: r#"{"path":"/x"}"#.into(),
    })
    .expect("append");
    ring.flush().expect("flush");

    // We don't read it back here (that's P11.11's `Query`); we just assert a
    // file was produced and its parquet footer parses.
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert!(!files.is_empty(), "expected at least one parquet file");
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-trace --test ring`
Expected: compile error — `Ring`, `SpanRow` not defined.

- [ ] **Step 5: Implement `crates/origin-trace/src/schema.rs`**

```rust
//! Arrow schema for a single span row.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};

#[derive(Debug, Clone)]
pub struct SpanRow {
    pub ts_ns:      u64,
    pub span_id:    u64,
    pub parent_id:  u64,
    pub kind:       &'static str,
    pub provider:   &'static str,
    pub tool:       &'static str,
    pub dur_us:     u64,
    pub error_kind: &'static str,
    pub attrs_json: String,
}

#[must_use]
pub fn span_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_ns",      DataType::UInt64, false),
        Field::new("span_id",    DataType::UInt64, false),
        Field::new("parent_id",  DataType::UInt64, false),
        Field::new("kind",       DataType::Utf8,   false),
        Field::new("provider",   DataType::Utf8,   false),
        Field::new("tool",       DataType::Utf8,   false),
        Field::new("dur_us",     DataType::UInt64, false),
        Field::new("error_kind", DataType::Utf8,   false),
        Field::new("attrs_json", DataType::Utf8,   false),
    ]))
}
```

- [ ] **Step 6: Implement `crates/origin-trace/src/ring.rs`**

```rust
//! Per-day parquet ring writer with 64 MiB rotation.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{StringBuilder, UInt64Builder};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use thiserror::Error;

use crate::schema::{span_schema, SpanRow};

#[derive(Debug, Error)]
pub enum RingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("arrow: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

const BATCH_ROWS: usize = 4096;

pub struct Ring {
    dir:       PathBuf,
    cap_bytes: usize,
    // In-memory builders flushed to parquet every `BATCH_ROWS` rows or on
    // explicit `flush()` / `Drop`.
    ts_ns:        UInt64Builder,
    span_id:      UInt64Builder,
    parent_id:    UInt64Builder,
    kind:         StringBuilder,
    provider:     StringBuilder,
    tool:         StringBuilder,
    dur_us:       UInt64Builder,
    error_kind:   StringBuilder,
    attrs_json:   StringBuilder,
    rows_in_buf:  usize,
    bytes_in_file: usize,
    current:      Option<ArrowWriter<File>>,
    current_path: PathBuf,
}

impl Ring {
    /// Open (or create) the ring under `dir`. New files are created lazily.
    ///
    /// # Errors
    /// Returns [`RingError::Io`] if `dir` cannot be created.
    pub fn open<P: AsRef<Path>>(dir: P, cap_bytes: usize) -> Result<Self, RingError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            cap_bytes,
            ts_ns:      UInt64Builder::with_capacity(BATCH_ROWS),
            span_id:    UInt64Builder::with_capacity(BATCH_ROWS),
            parent_id:  UInt64Builder::with_capacity(BATCH_ROWS),
            kind:       StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            provider:   StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            tool:       StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            dur_us:     UInt64Builder::with_capacity(BATCH_ROWS),
            error_kind: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            attrs_json: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 64),
            rows_in_buf:   0,
            bytes_in_file: 0,
            current:       None,
            current_path:  PathBuf::new(),
        })
    }

    /// Append one row.
    ///
    /// # Errors
    /// Returns [`RingError`] on parquet/arrow failure.
    pub fn append(&mut self, row: SpanRow) -> Result<(), RingError> {
        self.ts_ns.append_value(row.ts_ns);
        self.span_id.append_value(row.span_id);
        self.parent_id.append_value(row.parent_id);
        self.kind.append_value(row.kind);
        self.provider.append_value(row.provider);
        self.tool.append_value(row.tool);
        self.dur_us.append_value(row.dur_us);
        self.error_kind.append_value(row.error_kind);
        self.attrs_json.append_value(&row.attrs_json);
        self.rows_in_buf += 1;
        if self.rows_in_buf >= BATCH_ROWS {
            self.flush_batch()?;
        }
        Ok(())
    }

    /// Drain in-memory builders into the current parquet file and rotate if
    /// the file is past `cap_bytes`.
    ///
    /// # Errors
    /// Returns [`RingError`] on parquet/arrow failure.
    pub fn flush(&mut self) -> Result<(), RingError> {
        if self.rows_in_buf > 0 {
            self.flush_batch()?;
        }
        if let Some(w) = self.current.as_mut() {
            w.flush()?;
        }
        Ok(())
    }

    fn flush_batch(&mut self) -> Result<(), RingError> {
        let schema = span_schema();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(self.ts_ns.finish()),
                Arc::new(self.span_id.finish()),
                Arc::new(self.parent_id.finish()),
                Arc::new(self.kind.finish()),
                Arc::new(self.provider.finish()),
                Arc::new(self.tool.finish()),
                Arc::new(self.dur_us.finish()),
                Arc::new(self.error_kind.finish()),
                Arc::new(self.attrs_json.finish()),
            ],
        )?;

        let approx = approx_batch_bytes(&batch);
        if self.current.is_none() || self.bytes_in_file + approx > self.cap_bytes {
            self.rotate()?;
        }
        let writer = self.current.as_mut().expect("rotate sets writer");
        writer.write(&batch)?;
        self.bytes_in_file += approx;
        self.rows_in_buf = 0;
        Ok(())
    }

    fn rotate(&mut self) -> Result<(), RingError> {
        if let Some(w) = self.current.take() {
            w.close()?;
        }
        let today = chrono::Utc::now().format("%Y-%m-%d");
        let ts_ms = chrono::Utc::now().timestamp_millis();
        let path = self.dir.join(format!("trace-{today}-{ts_ms}.parquet"));
        let file = File::create(&path)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let writer = ArrowWriter::try_new(file, span_schema(), Some(props))?;
        self.current = Some(writer);
        self.current_path = path;
        self.bytes_in_file = 0;
        Ok(())
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        let _ = self.flush();
        if let Some(w) = self.current.take() {
            let _ = w.close();
        }
    }
}

fn approx_batch_bytes(batch: &RecordBatch) -> usize {
    // Snappy-compressed parquet typically lands ~25-40% of the raw arrow size
    // for our string-heavy schema. Use the raw size as a conservative cap
    // proxy — the actual file may be smaller, which is fine.
    batch
        .columns()
        .iter()
        .map(|c| c.get_array_memory_size())
        .sum::<usize>()
}
```

- [ ] **Step 7: Run the test, confirm pass**

Run: `cargo test -p origin-trace --test ring`
Expected: 2 tests pass. The rotation test may take ~5 s as it writes 130k rows.

- [ ] **Step 8: Verification gate (single-crate)**

```bash
cargo test -p origin-trace
cargo clippy -p origin-trace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 9: Commit**

```bash
git add crates/origin-trace Cargo.toml Cargo.lock
git commit -m "feat(origin-trace): parquet ring writer + Arrow span schema (P11.9)"
```

---

## Task P11.10 — `tracing::Subscriber` layer wired to the ring + daemon `#[instrument]`  **[sequential after P11.9]**

**Files:**

- Create: `crates/origin-trace/src/layer.rs`
- Create: `crates/origin-trace/benches/write.rs`
- Create: `crates/origin-trace/tests/layer.rs`
- Modify: `crates/origin-trace/src/lib.rs` — re-export `Layer` + `init`
- Modify: `crates/origin-daemon/src/main.rs` — install the layer
- Modify: `crates/origin-daemon/src/agent.rs` — `#[tracing::instrument]` hot paths
- Modify: `crates/origin-daemon/Cargo.toml` — add `origin-trace`, `tracing`, `tracing-subscriber`

- [ ] **Step 1: Write the failing test** at `crates/origin-trace/tests/layer.rs`

```rust
use origin_trace::{init, Ring};
use tempfile::tempdir;
use tracing::{info_span, instrument};

#[instrument(level = "info", fields(tool = "Read"))]
fn fake_tool(_arg: u32) {
    let _g = info_span!("inner", provider = "anthropic").entered();
}

#[test]
fn span_close_writes_a_row_to_the_ring() {
    let dir = tempdir().expect("tempdir");
    let _guard = init(dir.path()).expect("init layer");
    fake_tool(1);
    // Allow the SPSC drain to fire.
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(_guard); // forces flush via Drop
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert!(!files.is_empty(), "expected at least one parquet file after span close");
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-trace --test layer`
Expected: compile error — `init`/`Layer` not defined.

- [ ] **Step 3: Implement `crates/origin-trace/src/layer.rs`**

```rust
//! `tracing` Layer that feeds the parquet ring via a SPSC channel.
//!
//! The layer captures `on_close` events. Each close becomes one [`SpanRow`].
//! A background OS thread owns the [`Ring`] and drains the channel; the
//! foreground tracing path only does an `mpsc::Sender::send` (lock-free under
//! the common case).

use std::path::Path;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::thread::JoinHandle;
use std::time::Instant;

use tracing::{span, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::schema::SpanRow;
use crate::{Ring, RingError};

pub struct Layer {
    tx: SyncSender<SpanRow>,
}

/// Drop guard returned by [`init`]. Dropping flushes the channel and joins
/// the background thread.
#[must_use]
pub struct LayerGuard {
    join: Option<JoinHandle<()>>,
}

impl Drop for LayerGuard {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Initialise tracing with a parquet-backed layer writing to `dir`.
///
/// # Errors
/// Returns [`RingError`] if the ring cannot be opened.
pub fn init<P: AsRef<Path>>(dir: P) -> Result<LayerGuard, RingError> {
    use tracing_subscriber::layer::SubscriberExt;
    let ring = Ring::open(dir, 64 * 1024 * 1024)?;
    let (tx, rx) = sync_channel::<SpanRow>(4096);
    let join = std::thread::Builder::new()
        .name("origin-trace-drain".into())
        .spawn(move || {
            let mut ring = ring;
            for row in rx {
                let _ = ring.append(row);
            }
            let _ = ring.flush();
        })
        .map_err(RingError::Io)?;

    let layer = Layer { tx };
    let subscriber = tracing_subscriber::registry()
        .with(layer);
    // `set_global_default` may error if a subscriber is already installed
    // (e.g. in tests). For init we tolerate it: tests use the test-local
    // subscriber, but the layer's writes still flow via the explicit Ring.
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(LayerGuard { join: Some(join) })
}

impl<S> tracing_subscriber::Layer<S> for Layer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let mut span = ctx.span(id).expect("span exists");
        // Stash the start instant + a serialized attrs blob in the
        // span's extensions so on_close can compute duration without
        // re-walking the field set.
        let mut visitor = FieldCollector::default();
        attrs.record(&mut visitor);
        span.extensions_mut().insert(SpanStash {
            start: Instant::now(),
            kind: leak_str(visitor.kind.unwrap_or_else(|| "span".into())),
            provider: leak_str(visitor.provider.unwrap_or_default()),
            tool: leak_str(visitor.tool.unwrap_or_default()),
            error_kind: leak_str(visitor.error_kind.unwrap_or_default()),
            attrs_json: visitor.attrs_json(),
            parent: ctx.current_span().id().map(|i| i.into_u64()).unwrap_or(0),
        });
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(stash) = span.extensions().get::<SpanStash>().cloned() else { return };
        let dur_us = u64::try_from(stash.start.elapsed().as_micros()).unwrap_or(u64::MAX);
        let row = SpanRow {
            ts_ns: 0, // optional; the daemon's wall clock is captured per-record on the writer side if needed
            span_id: id.into_u64(),
            parent_id: stash.parent,
            kind: stash.kind,
            provider: stash.provider,
            tool: stash.tool,
            dur_us,
            error_kind: stash.error_kind,
            attrs_json: stash.attrs_json,
        };
        // Drop the row if the drain thread is wedged — we'd rather lose a
        // trace row than block the agent loop.
        let _ = self.tx.try_send(row);
    }
}

#[derive(Clone)]
struct SpanStash {
    start: Instant,
    kind: &'static str,
    provider: &'static str,
    tool: &'static str,
    error_kind: &'static str,
    attrs_json: String,
    parent: u64,
}

#[derive(Default)]
struct FieldCollector {
    kind: Option<String>,
    provider: Option<String>,
    tool: Option<String>,
    error_kind: Option<String>,
    other: std::collections::BTreeMap<&'static str, String>,
}

impl tracing::field::Visit for FieldCollector {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "kind"       => self.kind = Some(value.into()),
            "provider"   => self.provider = Some(value.into()),
            "tool"       => self.tool = Some(value.into()),
            "error_kind" => self.error_kind = Some(value.into()),
            other        => { self.other.insert(other, value.into()); }
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.other.insert(field.name(), format!("{value:?}"));
    }
}

impl FieldCollector {
    fn attrs_json(&self) -> String {
        // Pre-allocate a small JSON blob; the layer sees no `serde_json`
        // pretty-print cost on the hot path.
        let mut s = String::with_capacity(64 + self.other.len() * 16);
        s.push('{');
        for (i, (k, v)) in self.other.iter().enumerate() {
            if i > 0 { s.push(','); }
            s.push('"'); s.push_str(k);   s.push('"'); s.push(':');
            s.push('"'); s.push_str(&v.replace('"', "\\\""));  s.push('"');
        }
        s.push('}');
        s
    }
}

// `tracing` stash strings need a `'static` lifetime. We intern at span open.
// Strings are bounded by the number of distinct (kind, provider, tool, error)
// quadruples in the process — for our daemon, dozens.
fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
```

- [ ] **Step 4: Update `crates/origin-trace/src/lib.rs`**

```rust
pub mod layer;
pub mod ring;
pub mod schema;

pub use layer::{init, Layer, LayerGuard};
pub use ring::{Ring, RingError};
pub use schema::{span_schema, SpanRow};
```

- [ ] **Step 5: Bench** at `crates/origin-trace/benches/write.rs`

```rust
//! `cargo bench -p origin-trace --bench write -- --quick`
//!
//! Threshold: > 100k spans / sec on a single thread, single core.

use std::time::Instant;
use origin_trace::{Ring, SpanRow};
use tempfile::tempdir;

fn main() {
    let dir = tempdir().expect("tempdir");
    let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open");
    let n = 200_000;
    let start = Instant::now();
    for i in 0..n {
        ring.append(SpanRow {
            ts_ns: i,
            span_id: i,
            parent_id: 0,
            kind: "tool",
            provider: "anthropic",
            tool: "Bash",
            dur_us: 1,
            error_kind: "",
            attrs_json: r#"{"k":"v"}"#.into(),
        })
        .expect("append");
    }
    ring.flush().expect("flush");
    let elapsed = start.elapsed();
    let rate = (n as f64) / elapsed.as_secs_f64();
    eprintln!("write rate: {rate:.0} rows/s in {elapsed:?}");
    assert!(rate > 100_000.0, "write rate {rate} below 100k rows/s threshold");
}
```

- [ ] **Step 6: Wire the layer into `crates/origin-daemon/src/main.rs`**

Replace the bare `tracing_subscriber::fmt()` init at line ~35 of `main.rs` with a layered init that also installs `origin_trace::Layer`:

```rust
let trace_dir = dirs::data_local_dir()
    .unwrap_or_else(|| std::path::PathBuf::from("."))
    .join("origin").join("trace");
let _trace_guard = origin_trace::init(&trace_dir)
    .map_err(|e| std::io::Error::other(e.to_string()))?;
```

Add to `Cargo.toml`:

```toml
origin-trace = { path = "../origin-trace" }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
dirs = "5"
```

- [ ] **Step 7: Instrument hot paths in `crates/origin-daemon/src/agent.rs`**

Annotate `run_turn`, `dispatch_tool`, `call_provider`, `sidecar_job` (the exact function names may differ — instrument whichever function names are present):

```rust
#[tracing::instrument(level = "info", skip(self, args), fields(kind = "tool", tool = %meta.name))]
async fn dispatch_tool(&self, meta: &ToolMeta, args: Value) -> Result<Value, …> { … }

#[tracing::instrument(level = "info", skip(self), fields(kind = "turn"))]
async fn run_turn(&self, …) -> … { … }

#[tracing::instrument(level = "info", skip(self), fields(kind = "provider", provider = %provider_name))]
async fn call_provider(&self, provider_name: &str, …) -> … { … }
```

- [ ] **Step 8: Run the test, confirm pass**

Run: `cargo test -p origin-trace --test layer`
Expected: 1 test passes.

- [ ] **Step 9: Run the bench**

Run: `cargo bench -p origin-trace --bench write -- --quick`
Expected: rate > 100k rows/s.

- [ ] **Step 10: Verification gate (bench-touching)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo bench -p origin-trace --bench write -- --quick
```

- [ ] **Step 11: Commit**

```bash
git add crates/origin-trace crates/origin-daemon Cargo.toml Cargo.lock
git commit -m "feat(origin-trace): tracing Layer + daemon instrumentation (P11.10)"
```

---

## Task P11.11 — `origin trace query` CLI subcommand  **[sequential after P11.10]**

**Files:**

- Create: `crates/origin-trace/src/query.rs`
- Create: `crates/origin-trace/tests/query.rs`
- Create: `crates/origin-cli/src/trace_cmd.rs`
- Modify: `crates/origin-cli/src/main.rs` — clap dispatch
- Modify: `crates/origin-cli/Cargo.toml` — add `origin-trace`, `clap`

- [ ] **Step 1: Write the failing test** at `crates/origin-trace/tests/query.rs`

```rust
use origin_trace::{query::QueryArgs, Ring, SpanRow};
use tempfile::tempdir;

#[test]
fn query_filters_by_kind_and_error_kind() {
    let dir = tempdir().expect("tempdir");
    {
        let mut ring = Ring::open(dir.path(), 64 * 1024 * 1024).expect("open");
        ring.append(SpanRow {
            ts_ns: 1, span_id: 1, parent_id: 0,
            kind: "tool", provider: "anthropic", tool: "Bash",
            dur_us: 9, error_kind: "Sandbox",
            attrs_json: "{}".into(),
        }).expect("append");
        ring.append(SpanRow {
            ts_ns: 2, span_id: 2, parent_id: 0,
            kind: "tool", provider: "anthropic", tool: "Read",
            dur_us: 7, error_kind: "",
            attrs_json: "{}".into(),
        }).expect("append");
        ring.append(SpanRow {
            ts_ns: 3, span_id: 3, parent_id: 0,
            kind: "provider", provider: "anthropic", tool: "",
            dur_us: 18, error_kind: "Sandbox",
            attrs_json: "{}".into(),
        }).expect("append");
        ring.flush().expect("flush");
    }

    let rows = origin_trace::query::run(&QueryArgs {
        dir: dir.path().to_path_buf(),
        kind: Some("tool".into()),
        error_kind: Some("Sandbox".into()),
        limit: 100,
    }).expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].span_id, 1);
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-trace --test query`
Expected: compile error — `query` module not present.

- [ ] **Step 3: Implement `crates/origin-trace/src/query.rs`**

```rust
//! Parquet reader with pushdown predicates on `kind` and `error_kind`.

use std::fs::File;
use std::path::PathBuf;

use arrow::array::{Array, StringArray, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct QueryArgs {
    pub dir: PathBuf,
    pub kind: Option<String>,
    pub error_kind: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct QueryRow {
    pub ts_ns:      u64,
    pub span_id:    u64,
    pub parent_id:  u64,
    pub kind:       String,
    pub provider:   String,
    pub tool:       String,
    pub dur_us:     u64,
    pub error_kind: String,
    pub attrs_json: String,
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("arrow: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

/// Stream every `.parquet` file under `args.dir`, filter rows that match
/// `(kind, error_kind)`, return up to `limit`.
///
/// # Errors
/// Returns [`QueryError`] on I/O or parquet decode failure.
pub fn run(args: &QueryArgs) -> Result<Vec<QueryRow>, QueryError> {
    let mut out = Vec::with_capacity(args.limit.min(1024));
    for entry in std::fs::read_dir(&args.dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("parquet") { continue; }
        let file = File::open(entry.path())?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;
        for batch in reader {
            let batch = batch?;
            let kind_col       = batch.column(3).as_any().downcast_ref::<StringArray>().expect("utf8");
            let error_col      = batch.column(7).as_any().downcast_ref::<StringArray>().expect("utf8");
            let ts_col         = batch.column(0).as_any().downcast_ref::<UInt64Array>().expect("u64");
            let span_col       = batch.column(1).as_any().downcast_ref::<UInt64Array>().expect("u64");
            let parent_col     = batch.column(2).as_any().downcast_ref::<UInt64Array>().expect("u64");
            let provider_col   = batch.column(4).as_any().downcast_ref::<StringArray>().expect("utf8");
            let tool_col       = batch.column(5).as_any().downcast_ref::<StringArray>().expect("utf8");
            let dur_col        = batch.column(6).as_any().downcast_ref::<UInt64Array>().expect("u64");
            let attrs_col      = batch.column(8).as_any().downcast_ref::<StringArray>().expect("utf8");

            for i in 0..batch.num_rows() {
                if let Some(want) = &args.kind {
                    if kind_col.value(i) != want { continue; }
                }
                if let Some(want) = &args.error_kind {
                    if error_col.value(i) != want { continue; }
                }
                out.push(QueryRow {
                    ts_ns: ts_col.value(i),
                    span_id: span_col.value(i),
                    parent_id: parent_col.value(i),
                    kind: kind_col.value(i).into(),
                    provider: provider_col.value(i).into(),
                    tool: tool_col.value(i).into(),
                    dur_us: dur_col.value(i),
                    error_kind: error_col.value(i).into(),
                    attrs_json: attrs_col.value(i).into(),
                });
                if out.len() >= args.limit { return Ok(out); }
            }
        }
    }
    Ok(out)
}
```

Re-export from `lib.rs`:

```rust
pub mod query;
pub use query::{run, QueryArgs, QueryError, QueryRow};
```

- [ ] **Step 4: Implement `crates/origin-cli/src/trace_cmd.rs`**

```rust
//! `origin trace query` subcommand.

use std::path::PathBuf;

use clap::Args;
use origin_trace::query::{run, QueryArgs};

#[derive(Debug, Args)]
pub struct TraceQuery {
    /// Trace ring directory. Defaults to `$XDG_DATA_HOME/origin/trace`
    /// (or the local-data dir on Windows / macOS).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Filter by `kind` column (e.g. `tool`, `provider`, `turn`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Filter by `error_kind` column (e.g. `Sandbox`, `Provider429`).
    #[arg(long)]
    pub error_kind: Option<String>,
    /// Maximum rows to print.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

/// # Errors
/// Returns [`origin_trace::query::QueryError`] on parquet/io failure.
pub fn invoke(args: TraceQuery) -> Result<(), Box<dyn std::error::Error>> {
    let dir = args.dir.unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("origin")
            .join("trace")
    });
    let q = QueryArgs {
        dir,
        kind: args.kind,
        error_kind: args.error_kind,
        limit: args.limit,
    };
    let rows = run(&q)?;
    for row in rows {
        println!(
            "{ts_ns:>20} {kind:<10} {provider:<12} {tool:<16} dur={dur_us}µs err={error_kind} attrs={attrs_json}",
            ts_ns = row.ts_ns,
            kind = row.kind,
            provider = row.provider,
            tool = row.tool,
            dur_us = row.dur_us,
            error_kind = if row.error_kind.is_empty() { "-".to_string() } else { row.error_kind },
            attrs_json = row.attrs_json,
        );
    }
    Ok(())
}
```

- [ ] **Step 5: Wire it into `crates/origin-cli/src/main.rs`**

Inspect the current `main.rs` shape. If it has no clap structure yet (the survey confirmed this), introduce one:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "origin", version, about = "origin agentic coding harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Query the trace ring.
    Trace {
        #[command(subcommand)]
        sub: TraceSub,
    },
}

#[derive(Subcommand)]
enum TraceSub {
    Query(origin_cli::trace_cmd::TraceQuery),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.cmd {
        None => origin_cli::run_tui(),
        Some(Cmd::Trace { sub: TraceSub::Query(q) }) => origin_cli::trace_cmd::invoke(q),
    }
}
```

Add `clap = { version = "4", features = ["derive"] }`, `origin-trace = { path = "../origin-trace" }`, `dirs = "5"` to `crates/origin-cli/Cargo.toml`.

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-trace --test query`
Expected: 1 test passes.

- [ ] **Step 7: Smoke-test the CLI**

```bash
cargo run -p origin-cli -- trace query --dir /tmp/origin-test-trace --limit 5
```

Expected: exits 0 with no output (no rows in an empty dir) or the matching rows.

- [ ] **Step 8: Verification gate (cross-crate)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 9: Commit**

```bash
git add crates/origin-trace crates/origin-cli Cargo.toml Cargo.lock
git commit -m "feat(origin-cli): trace query subcommand + parquet pushdown reader (P11.11)"
```

---

# Cluster D — Metrics + TUI panel + `/metrics`

## Task P11.12 — `origin-metrics` crate + Prom endpoint + TUI `?metrics` panel  **[parallel-safe with A/B/C/E]**

**Files:**

- Create: `crates/origin-metrics/Cargo.toml`
- Create: `crates/origin-metrics/src/lib.rs`
- Create: `crates/origin-metrics/src/keys.rs`
- Create: `crates/origin-metrics/src/exporter.rs`
- Create: `crates/origin-metrics/tests/encode.rs`
- Create: `crates/origin-metrics/benches/encode.rs`
- Modify: `crates/origin-daemon/src/main.rs` — `--metrics-bind` flag
- Modify: `crates/origin-daemon/Cargo.toml` — add `origin-metrics`, `hyper`
- Create: `crates/origin-tui/src/widgets/metrics.rs`
- Modify: `crates/origin-tui/src/panel.rs` — route `?` to metrics widget
- Modify: `crates/origin-tui/Cargo.toml` — add `origin-metrics`
- Create: `crates/origin-tui/tests/metrics_panel.rs`

- [ ] **Step 1: Manifest** at `crates/origin-metrics/Cargo.toml`

```toml
[package]
name = "origin-metrics"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[features]
default = []
otel = ["dep:opentelemetry", "dep:opentelemetry-otlp"]

[dependencies]
prometheus = { workspace = true }
serde = { version = "1", features = ["derive"] }
thiserror = "1"
opentelemetry = { workspace = true, optional = true }
opentelemetry-otlp = { workspace = true, optional = true }

[dev-dependencies]
```

- [ ] **Step 2: Write the failing test** at `crates/origin-metrics/tests/encode.rs`

```rust
use origin_metrics::{Metrics, Snapshot};

#[test]
fn counter_increments_and_encodes_as_prom_text() {
    let m = Metrics::new();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    m.tool_call_total("anthropic", "Edit", "err").inc();

    let text = m.encode_text().expect("encode");
    assert!(text.contains("origin_tool_call_total{provider=\"anthropic\",tool=\"Bash\",result=\"ok\"} 2"),
        "got: {text}");
    assert!(text.contains("origin_tool_call_total{provider=\"anthropic\",tool=\"Edit\",result=\"err\"} 1"));
}

#[test]
fn token_accounting_observes_per_provider() {
    let m = Metrics::new();
    m.tokens_in_total("anthropic", "claude-opus-4-7").inc_by(120);
    m.tokens_out_total("anthropic", "claude-opus-4-7").inc_by(85);
    let text = m.encode_text().expect("encode");
    assert!(text.contains("origin_tokens_in_total{provider=\"anthropic\",model=\"claude-opus-4-7\"} 120"));
    assert!(text.contains("origin_tokens_out_total{provider=\"anthropic\",model=\"claude-opus-4-7\"} 85"));
}

#[test]
fn snapshot_returns_every_registered_metric() {
    let m = Metrics::new();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    let snap: Snapshot = m.snapshot();
    let bash_ok = snap.iter()
        .find(|s| s.name == "origin_tool_call_total"
              && s.labels.iter().any(|(k,v)| k=="tool" && v=="Bash"))
        .expect("Bash metric in snapshot");
    assert_eq!(bash_ok.value, 1.0);
}

#[test]
fn cardinality_is_bounded() {
    // Allowlist cap = 4 label dimensions; reject extras with a no-op.
    let m = Metrics::new();
    for i in 0..50 {
        let tool = Box::leak(format!("UnknownTool{i}").into_boxed_str()) as &'static str;
        m.tool_call_total("anthropic", tool, "ok").inc();
    }
    // The label allowlist drops unknown tool names into a single "_other_"
    // bucket so cardinality is capped (see `keys.rs::canonical_tool`).
    let text = m.encode_text().expect("encode");
    let lines = text.lines().filter(|l| l.contains("origin_tool_call_total{")).count();
    assert!(lines <= 25, "expected ≤25 distinct label tuples, got {lines}");
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-metrics --test encode`
Expected: compile error — `Metrics`/`Snapshot` not defined.

- [ ] **Step 4: Implement `crates/origin-metrics/src/keys.rs`**

```rust
//! Bounded-cardinality label keyspace.
//!
//! We enforce a static allowlist of (provider, tool) tuples that count
//! against the metric label set. Unknown values fall into `_other_` so a
//! pathological MCP server can't inflate cardinality.

pub const ALLOWED_PROVIDERS: &[&str] = &[
    "anthropic", "openai", "gemini", "openrouter", "bedrock", "ollama", "github",
];

pub const ALLOWED_TOOLS: &[&str] = &[
    "Bash", "Edit", "Read", "Glob", "Grep", "Write", "Recall",
    "WebFetch", "graph_query", "graph_path", "graph_summarize", "graph_explain", "graph_rebuild",
    "mem_search", "mem_save", "mem_forget", "Ask", "Task",
];

pub const ALLOWED_RESULTS: &[&str] = &["ok", "err", "denied"];

#[must_use]
pub fn canonical_provider(s: &str) -> &'static str {
    canonicalize(s, ALLOWED_PROVIDERS)
}
#[must_use]
pub fn canonical_tool(s: &str) -> &'static str {
    canonicalize(s, ALLOWED_TOOLS)
}
#[must_use]
pub fn canonical_result(s: &str) -> &'static str {
    canonicalize(s, ALLOWED_RESULTS)
}

fn canonicalize(s: &str, allow: &[&'static str]) -> &'static str {
    for a in allow { if *a == s { return *a; } }
    "_other_"
}
```

- [ ] **Step 5: Implement `crates/origin-metrics/src/lib.rs`**

```rust
//! Bounded-cardinality counters + Prometheus text encoder.

pub mod exporter;
pub mod keys;

use std::sync::Arc;

use prometheus::{Encoder, IntCounterVec, Opts, Registry, TextEncoder};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("encode: {0}")]
    Encode(String),
    #[error("register: {0}")]
    Register(String),
}

#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,
    tool_call:  IntCounterVec,
    tokens_in:  IntCounterVec,
    tokens_out: IntCounterVec,
    cache_hit:  IntCounterVec,
    sandbox_violation: IntCounterVec,
}

impl Metrics {
    /// Build a fresh registry with all `origin_*` series declared.
    ///
    /// # Panics
    /// Panics if the static metric metadata is malformed (caught at boot).
    #[must_use]
    pub fn new() -> Self {
        let registry = Arc::new(Registry::new());
        let tool_call = IntCounterVec::new(
            Opts::new("origin_tool_call_total", "total tool invocations"),
            &["provider", "tool", "result"],
        ).expect("metric opts");
        let tokens_in = IntCounterVec::new(
            Opts::new("origin_tokens_in_total", "input tokens billed"),
            &["provider", "model"],
        ).expect("metric opts");
        let tokens_out = IntCounterVec::new(
            Opts::new("origin_tokens_out_total", "output tokens billed"),
            &["provider", "model"],
        ).expect("metric opts");
        let cache_hit = IntCounterVec::new(
            Opts::new("origin_cache_hit_total", "prompt-cache reads served from cache"),
            &["provider"],
        ).expect("metric opts");
        let sandbox_violation = IntCounterVec::new(
            Opts::new("origin_sandbox_violation_total", "kernel-enforced sandbox denials"),
            &["profile", "kind"],
        ).expect("metric opts");
        for c in [&tool_call, &tokens_in, &tokens_out, &cache_hit, &sandbox_violation] {
            registry.register(Box::new(c.clone())).expect("register");
        }
        Self { registry, tool_call, tokens_in, tokens_out, cache_hit, sandbox_violation }
    }

    #[must_use]
    pub fn tool_call_total(&self, provider: &str, tool: &str, result: &str) -> prometheus::IntCounter {
        self.tool_call.with_label_values(&[
            keys::canonical_provider(provider),
            keys::canonical_tool(tool),
            keys::canonical_result(result),
        ])
    }

    #[must_use]
    pub fn tokens_in_total(&self, provider: &str, model: &str) -> prometheus::IntCounter {
        self.tokens_in.with_label_values(&[keys::canonical_provider(provider), model])
    }

    #[must_use]
    pub fn tokens_out_total(&self, provider: &str, model: &str) -> prometheus::IntCounter {
        self.tokens_out.with_label_values(&[keys::canonical_provider(provider), model])
    }

    #[must_use]
    pub fn cache_hit_total(&self, provider: &str) -> prometheus::IntCounter {
        self.cache_hit.with_label_values(&[keys::canonical_provider(provider)])
    }

    #[must_use]
    pub fn sandbox_violation_total(&self, profile: &str, kind: &str) -> prometheus::IntCounter {
        self.sandbox_violation.with_label_values(&[profile, kind])
    }

    /// Prometheus text exposition.
    ///
    /// # Errors
    /// Returns [`MetricsError::Encode`] on UTF-8 conversion failure.
    pub fn encode_text(&self) -> Result<String, MetricsError> {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buf = Vec::with_capacity(8192);
        encoder
            .encode(&metric_families, &mut buf)
            .map_err(|e| MetricsError::Encode(e.to_string()))?;
        String::from_utf8(buf).map_err(|e| MetricsError::Encode(e.to_string()))
    }

    /// Plain rows for in-process consumers (TUI `?metrics` panel).
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let mut rows = Vec::new();
        for fam in self.registry.gather() {
            for m in fam.get_metric() {
                let labels = m.get_label().iter()
                    .map(|p| (p.get_name().to_string(), p.get_value().to_string()))
                    .collect::<Vec<_>>();
                rows.push(SnapshotRow {
                    name:   fam.get_name().to_string(),
                    labels,
                    value:  m.get_counter().get_value(),
                });
            }
        }
        Snapshot { rows }
    }
}

impl Default for Metrics {
    fn default() -> Self { Self::new() }
}

#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot { pub rows: Vec<SnapshotRow> }

impl Snapshot {
    #[must_use]
    pub fn iter(&self) -> impl Iterator<Item = &SnapshotRow> { self.rows.iter() }
}
```

- [ ] **Step 6: Implement `crates/origin-metrics/src/exporter.rs`** (OTel feature-gated)

```rust
//! Optional OpenTelemetry OTLP exporter. Gated behind the `otel` cargo feature.

#[cfg(feature = "otel")]
pub mod otel {
    use opentelemetry::global;
    use opentelemetry_otlp::WithExportConfig;

    /// Install a global OTel meter provider pointing at `endpoint`.
    ///
    /// # Errors
    /// Returns a string error if exporter setup fails.
    pub fn install(endpoint: &str) -> Result<(), String> {
        let _ = global::meter_provider();
        let _ = endpoint;
        // The actual exporter wiring depends on opentelemetry 0.24's surface;
        // the meter-provider boot is sketched here. The full body lands once
        // the crate compiles cleanly against the workspace `tracing` major
        // and the OTel-bridge crate is added to the workspace deps.
        Ok(())
    }
}
```

The OTel body is intentionally minimal at P11.12; richer integration is post-GA.

- [ ] **Step 7: Implement the `/metrics` HTTP server in `crates/origin-daemon/src/main.rs`**

Add a flag and a spawned tokio task:

```rust
#[derive(Parser)]
struct DaemonArgs {
    /// Bind a /metrics Prometheus endpoint (e.g. `127.0.0.1:9876`).
    #[arg(long)]
    metrics_bind: Option<String>,
    /* …existing args… */
}

if let Some(bind) = args.metrics_bind.clone() {
    let metrics = metrics_handle.clone();
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(&bind).await
            .expect("bind metrics endpoint");
        loop {
            let (stream, _) = match listener.accept().await { Ok(p) => p, Err(_) => continue };
            let metrics = metrics.clone();
            tokio::spawn(async move {
                let io = hyper_util::rt::TokioIo::new(stream);
                let svc = hyper::service::service_fn(move |_req: hyper::Request<_>| {
                    let body = metrics.encode_text().unwrap_or_default();
                    async move {
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(
                            http_body_util::Full::new(hyper::body::Bytes::from(body))
                        ))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
}
```

Add to `crates/origin-daemon/Cargo.toml`:

```toml
origin-metrics  = { path = "../origin-metrics" }
hyper           = { workspace = true }
hyper-util      = { version = "0.1", features = ["tokio"] }
http-body-util  = "0.1"
```

> **Note on `hyper` 1 vs 0.x:** The hyper 1 API requires `hyper-util` for tokio adapters and `http-body-util` for body builders. These are stable and pull no extra transitive risk.

- [ ] **Step 8: Implement the TUI metrics widget** at `crates/origin-tui/src/widgets/metrics.rs`

```rust
//! `?metrics` panel widget.

use origin_metrics::Metrics;

pub struct MetricsWidget<'a> {
    metrics: &'a Metrics,
}

impl<'a> MetricsWidget<'a> {
    #[must_use]
    pub const fn new(metrics: &'a Metrics) -> Self { Self { metrics } }

    /// Render the snapshot as a series of lines (one per metric series).
    /// The caller is responsible for clipping to the panel rect.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let snap = self.metrics.snapshot();
        let mut out: Vec<String> = Vec::with_capacity(snap.rows.len());
        for row in snap.iter() {
            let labels = row.labels.iter()
                .map(|(k,v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            out.push(format!("{name:<32} {labels:<48} {value:>10.0}",
                name = row.name, labels = labels, value = row.value));
        }
        out
    }
}
```

Route the `?` key in `crates/origin-tui/src/panel.rs` to this widget. The existing panel already routes events through `PanelEvent`; add a `PanelEvent::ShowMetrics` variant and a new sub-state `PanelState::Metrics`. Match `KeyEvent { code: KeyCode::Char('?'), .. }` in the main input loop.

Add `origin-metrics = { path = "../origin-metrics" }` to `crates/origin-tui/Cargo.toml`.

- [ ] **Step 9: Write the TUI panel test** at `crates/origin-tui/tests/metrics_panel.rs`

```rust
use origin_metrics::Metrics;
use origin_tui::widgets::metrics::MetricsWidget;

#[test]
fn snapshot_contains_every_registered_metric() {
    let m = Metrics::new();
    m.tool_call_total("anthropic", "Bash", "ok").inc();
    m.tokens_in_total("anthropic", "claude-opus-4-7").inc_by(10);
    let widget = MetricsWidget::new(&m);
    let lines = widget.lines();
    assert!(lines.iter().any(|l| l.contains("origin_tool_call_total") && l.contains("Bash")));
    assert!(lines.iter().any(|l| l.contains("origin_tokens_in_total") && l.contains("claude-opus-4-7")));
}
```

- [ ] **Step 10: Bench** at `crates/origin-metrics/benches/encode.rs`

```rust
//! `cargo bench -p origin-metrics --bench encode -- --quick`
//!
//! Threshold: encode 1000-series snapshot in ≤ 200 µs.

use std::time::Instant;
use origin_metrics::Metrics;

fn main() {
    let m = Metrics::new();
    for i in 0..1000 {
        let result = if i % 2 == 0 { "ok" } else { "err" };
        m.tool_call_total("anthropic", "Bash", result).inc();
    }
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = m.encode_text().expect("encode");
    }
    let elapsed = start.elapsed();
    let per_call = elapsed / 1000;
    eprintln!("encode {per_call:?} avg");
    assert!(per_call.as_micros() <= 200, "encode took {per_call:?} > 200µs threshold");
}
```

- [ ] **Step 11: Run the tests, confirm pass**

```bash
cargo test -p origin-metrics
cargo test -p origin-tui --test metrics_panel
```

Expected: all pass.

- [ ] **Step 12: Verification gate (bench-touching)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo bench -p origin-metrics --bench encode -- --quick
```

- [ ] **Step 13: Commit**

```bash
git add crates/origin-metrics crates/origin-daemon crates/origin-tui Cargo.toml Cargo.lock
git commit -m "feat(origin-metrics): bounded-cardinality registry + /metrics + TUI panel (P11.12)"
```

---

# Cluster E — KeyVault audit log + `Secret<T>` CI lint

## Task P11.13 — KeyVault audit log ring  **[parallel-safe with A/B/C/D]**

**Files:**

- Create: `crates/origin-keyvault/src/audit.rs`
- Modify: `crates/origin-keyvault/src/lib.rs` — emit audit events from every public method
- Create: `crates/origin-keyvault/tests/audit.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-keyvault/tests/audit.rs`

```rust
use origin_keyvault::audit::{AuditAction, AuditRing};
use tempfile::tempdir;

#[tokio::test]
async fn ring_appends_and_replays() {
    let dir = tempdir().expect("tempdir");
    let ring = AuditRing::open(dir.path()).await.expect("open");
    ring.record(AuditAction::Set, "anthropic", "default").await.expect("rec");
    ring.record(AuditAction::Get, "anthropic", "default").await.expect("rec");
    ring.record(AuditAction::Delete, "anthropic", "default").await.expect("rec");

    let events = ring.replay().await.expect("replay");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].action, AuditAction::Set);
    assert_eq!(events[0].provider, "anthropic");
    assert_eq!(events[0].account, "default");
}

#[tokio::test]
async fn ring_never_records_secret_bytes() {
    let dir = tempdir().expect("tempdir");
    let ring = AuditRing::open(dir.path()).await.expect("open");
    ring.record(AuditAction::Set, "anthropic", "default").await.expect("rec");
    let events = ring.replay().await.expect("replay");
    // Field schema: action + provider + account + timestamp; no `secret` field.
    let json = serde_json::to_string(&events[0]).expect("ser");
    assert!(!json.contains("sk-"), "secret token must never appear in audit: {json}");
    assert!(!json.contains("Bearer"), "auth header must never appear in audit: {json}");
}

#[tokio::test]
async fn ring_rotates_after_30_days_worth_of_entries() {
    // Use an aggressively-small page size so the test runs in <1s; real config
    // is 8 MiB per page * 30 days.
    let dir = tempdir().expect("tempdir");
    let ring = AuditRing::open_with_page_size(dir.path(), 1024).await.expect("open");
    for i in 0..500 {
        ring.record(AuditAction::Get, "anthropic", &format!("acct-{i}")).await.expect("rec");
    }
    let pages: Vec<_> = std::fs::read_dir(dir.path()).expect("readdir").collect();
    assert!(pages.len() >= 2, "expected ≥2 pages after rotation, got {}", pages.len());
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-keyvault --test audit`
Expected: compile error — `audit` module / `AuditRing` not present.

- [ ] **Step 3: Implement `crates/origin-keyvault/src/audit.rs`**

```rust
//! KeyVault audit log: 30-day rotating ring, 8 MiB pages, JSON-Lines on disk.
//!
//! Records **what** key was touched (provider + account + action + timestamp),
//! never the secret bytes. The ring is independent of the parquet trace
//! pipeline (N10.16) so a parquet failure cannot drop audit records.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction { Set, Get, Delete, List }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub ts_ms:    i64,
    pub action:   AuditAction,
    pub provider: String,
    pub account:  String,
}

pub struct AuditRing {
    dir:        PathBuf,
    page_bytes: usize,
    current:    Mutex<RingState>,
}

struct RingState {
    file:        File,
    current_path: PathBuf,
    bytes:        usize,
}

impl AuditRing {
    pub async fn open<P: AsRef<Path>>(dir: P) -> Result<Self, AuditError> {
        Self::open_with_page_size(dir, 8 * 1024 * 1024).await
    }

    pub async fn open_with_page_size<P: AsRef<Path>>(dir: P, page_bytes: usize) -> Result<Self, AuditError> {
        let dir = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;
        let (file, path, bytes) = Self::open_current_page(&dir).await?;
        Ok(Self {
            dir,
            page_bytes,
            current: Mutex::new(RingState { file, current_path: path, bytes }),
        })
    }

    async fn open_current_page(dir: &Path) -> Result<(File, PathBuf, usize), AuditError> {
        let today = Utc::now().format("%Y-%m-%d");
        let path = dir.join(format!("audit-{today}.jsonl"));
        let bytes = match tokio::fs::metadata(&path).await {
            Ok(m) => usize::try_from(m.len()).unwrap_or(0),
            Err(_) => 0,
        };
        let file = OpenOptions::new().create(true).append(true).open(&path).await?;
        Ok((file, path, bytes))
    }

    /// Record an event. Never blocks on disk-rotation lock contention beyond
    /// the per-process mutex (one ring per daemon).
    ///
    /// # Errors
    /// Returns [`AuditError`] on I/O or serialization failure.
    pub async fn record(&self, action: AuditAction, provider: &str, account: &str) -> Result<(), AuditError> {
        let ev = AuditEvent {
            ts_ms: Utc::now().timestamp_millis(),
            action,
            provider: provider.into(),
            account: account.into(),
        };
        let mut line = serde_json::to_string(&ev)?;
        line.push('\n');
        let buf = line.as_bytes();

        let mut g = self.current.lock().await;
        if g.bytes + buf.len() > self.page_bytes {
            g.file.flush().await?;
            let (file, path, _) = Self::open_next_page(&self.dir).await?;
            g.file = file;
            g.current_path = path;
            g.bytes = 0;
        }
        g.file.write_all(buf).await?;
        g.bytes += buf.len();
        // Best-effort GC: remove any audit page older than 30 days.
        let _ = Self::gc_old_pages(&self.dir).await;
        Ok(())
    }

    async fn open_next_page(dir: &Path) -> Result<(File, PathBuf, usize), AuditError> {
        let stamp = Utc::now().format("%Y-%m-%d-%H%M%S%f");
        let path = dir.join(format!("audit-{stamp}.jsonl"));
        let file = OpenOptions::new().create_new(true).write(true).open(&path).await?;
        Ok((file, path, 0))
    }

    async fn gc_old_pages(dir: &Path) -> Result<(), AuditError> {
        let cutoff = Utc::now() - chrono::Duration::days(30);
        let mut rd = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let meta = match entry.metadata().await { Ok(m) => m, Err(_) => continue };
            let mtime: chrono::DateTime<Utc> = meta
                .modified()
                .ok()
                .map(chrono::DateTime::<Utc>::from)
                .unwrap_or_else(Utc::now);
            if mtime < cutoff {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
        Ok(())
    }

    /// Read every page in chronological order and return all events. Used by
    /// integration tests and the future `origin keyring audit` CLI.
    ///
    /// # Errors
    /// Returns [`AuditError`] on I/O / parse failure.
    pub async fn replay(&self) -> Result<Vec<AuditEvent>, AuditError> {
        let mut entries: Vec<PathBuf> = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.dir).await?;
        while let Some(e) = rd.next_entry().await? {
            if e.path().extension().and_then(|s| s.to_str()) == Some("jsonl") {
                entries.push(e.path());
            }
        }
        entries.sort();
        let mut out = Vec::new();
        for p in entries {
            let f = tokio::fs::File::open(&p).await?;
            let mut lines = BufReader::new(f).lines();
            while let Some(line) = lines.next_line().await? {
                if line.is_empty() { continue; }
                out.push(serde_json::from_str(&line)?);
            }
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Wire audit emission into `crates/origin-keyvault/src/lib.rs`**

Modify the `KeyVault` struct + impl to hold an `Option<Arc<audit::AuditRing>>` and emit on every public method:

```rust
pub mod audit;

use crate::audit::{AuditAction, AuditRing};

#[derive(Clone)]
pub struct KeyVault {
    inner: Arc<dyn Backend>,
    audit: Option<Arc<AuditRing>>,
}

impl KeyVault {
    /// Open with an attached audit ring under `audit_dir`.
    ///
    /// # Errors
    /// Forwards [`audit::AuditError`] as [`Error::Backend`].
    pub async fn detect_with_audit<P: AsRef<std::path::Path>>(audit_dir: P) -> Result<Self, Error> {
        let mut vault = Self::detect()?;
        let ring = AuditRing::open(audit_dir).await
            .map_err(|e| Error::Backend(e.to_string()))?;
        vault.audit = Some(Arc::new(ring));
        Ok(vault)
    }

    async fn audit(&self, action: AuditAction, provider: &str, account: &str) {
        if let Some(ring) = &self.audit {
            let _ = ring.record(action, provider, account).await;
        }
    }
}
```

Then in each existing method (`set`/`get`/`delete`/`list`), call `self.audit(AuditAction::…, provider, account).await` *after* the backend call returns.

Update `Self::detect()` and `Self::in_memory()` to default `audit: None` so existing call-sites compile.

- [ ] **Step 5: Run the test, confirm pass**

Run: `cargo test -p origin-keyvault --test audit`
Expected: 3 tests pass.

- [ ] **Step 6: Verification gate (single-crate)**

```bash
cargo test -p origin-keyvault
cargo clippy -p origin-keyvault --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit**

```bash
git add crates/origin-keyvault Cargo.toml Cargo.lock
git commit -m "feat(origin-keyvault): 30-day rotating audit ring (P11.13)"
```

---

## Task P11.14 — `xtask lint-secrets` redaction lint  **[parallel-safe with A/B/C/D; sequential after P11.13 (no shared files but conceptual)]**

**Files:**

- Create: `xtask/Cargo.toml`
- Create: `xtask/src/main.rs`
- Create: `xtask/src/lint_secrets.rs`
- Create: `xtask/tests/fixtures/clean.rs`
- Create: `xtask/tests/fixtures/dirty.rs`
- Create: `xtask/tests/lint_secrets.rs`

- [ ] **Step 1: Manifest** at `xtask/Cargo.toml`

```toml
[package]
name = "xtask"
version = "0.0.1"
edition = "2021"
publish = false

[lints]
workspace = true

[dependencies]
clap = { version = "4", features = ["derive"] }
syn = { version = "2", features = ["full", "visit"] }
walkdir = "2"
regex = "1"
thiserror = "1"
```

- [ ] **Step 2: Write the failing test** at `xtask/tests/lint_secrets.rs`

```rust
use std::process::Command;

fn xtask_lint(path: &str) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "xtask", "--", "lint-secrets", "--path", path])
        .output()
        .expect("spawn xtask")
}

#[test]
fn clean_fixture_passes() {
    let out = xtask_lint("xtask/tests/fixtures/clean.rs");
    assert!(out.status.success(),
        "clean fixture should pass; stderr={}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn dirty_fixture_fails() {
    let out = xtask_lint("xtask/tests/fixtures/dirty.rs");
    assert!(!out.status.success(),
        "dirty fixture should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("api_key") || stderr.contains("ApiKey"),
        "expected api_key violation in stderr: {stderr}");
}
```

- [ ] **Step 3: Implement the fixtures**

Clean — `xtask/tests/fixtures/clean.rs`:

```rust
// Every secret-looking field is wrapped in `Secret<…>` or marked `#[redact]`.

pub struct Session {
    pub user: String,
    pub api_key: origin_keyvault::Secret<String>,
    #[redact]
    pub token: String,
}
```

Dirty — `xtask/tests/fixtures/dirty.rs`:

```rust
// `api_key` is a raw `String` with `#[derive(Debug)]` on the struct → must
// fail the lint.

#[derive(Debug)]
pub struct Session {
    pub user: String,
    pub api_key: String, // VIOLATION
}
```

- [ ] **Step 4: Implement `xtask/src/main.rs`**

```rust
use clap::{Parser, Subcommand};

mod lint_secrets;

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan Rust source for unwrapped secret-named fields under `#[derive(Debug)]`.
    LintSecrets(lint_secrets::Args),
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::LintSecrets(a) => lint_secrets::run(a),
    };
    std::process::exit(code);
}
```

- [ ] **Step 5: Implement `xtask/src/lint_secrets.rs`**

```rust
//! Walks the workspace AST, flags any `#[derive(Debug)]` struct whose field
//! name matches `(?i)(key|token|password|auth|secret|credential)` unless the
//! field type contains `Secret<…>` or the field has a `#[redact]` attribute.

use std::path::PathBuf;

use clap::Args;
use regex::Regex;
use syn::{visit::Visit, File, Item, ItemStruct, Meta, Type, TypePath};
use walkdir::WalkDir;

#[derive(Debug, Args)]
pub struct CliArgs {
    /// Path to scan. Defaults to the workspace root.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub use CliArgs as Args;

#[must_use]
pub fn run(args: Args) -> i32 {
    let pat = Regex::new(r"(?i)(key|token|password|auth|secret|credential)")
        .expect("compile regex");
    let mut violations: Vec<String> = Vec::new();

    let paths_to_scan: Vec<PathBuf> = if args.path.is_dir() {
        WalkDir::new(&args.path)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
            .filter(|e| !e.path().components().any(|c| c.as_os_str() == "target"))
            .map(|e| e.into_path())
            .collect()
    } else {
        vec![args.path.clone()]
    };

    for p in &paths_to_scan {
        let src = match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let ast: File = match syn::parse_file(&src) {
            Ok(a) => a,
            Err(_) => continue,
        };
        let mut v = LintVisitor { regex: &pat, path: p.clone(), violations: &mut violations };
        v.visit_file(&ast);
    }

    if violations.is_empty() {
        0
    } else {
        for v in &violations {
            eprintln!("secret-redaction violation: {v}");
        }
        1
    }
}

struct LintVisitor<'a> {
    regex: &'a Regex,
    path: PathBuf,
    violations: &'a mut Vec<String>,
}

impl<'a, 'ast> Visit<'ast> for LintVisitor<'a> {
    fn visit_item_struct(&mut self, s: &'ast ItemStruct) {
        let derives_debug = s.attrs.iter().any(|a| matches!(&a.meta, Meta::List(ml)
            if ml.path.is_ident("derive")
               && ml.tokens.to_string().split(',').any(|t| t.trim() == "Debug")));
        if !derives_debug { return; }
        for field in s.fields.iter() {
            let Some(name) = field.ident.as_ref() else { continue; };
            let name_s = name.to_string();
            if !self.regex.is_match(&name_s) { continue; }
            if has_redact_attr(&field.attrs) { continue; }
            if is_secret_type(&field.ty) { continue; }
            self.violations.push(format!(
                "{p}: struct `{ty}` field `{f}` looks secret-typed but is `{kind}`; \
                 wrap in `Secret<…>` or add `#[redact]`",
                p = self.path.display(),
                ty = s.ident,
                f = name_s,
                kind = quote_type(&field.ty),
            ));
        }
    }
}

fn has_redact_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("redact"))
}

fn is_secret_type(ty: &Type) -> bool {
    if let Type::Path(TypePath { path, .. }) = ty {
        return path.segments.iter().any(|seg| seg.ident == "Secret");
    }
    false
}

fn quote_type(ty: &Type) -> String {
    use quote::ToTokens;
    let mut s = proc_macro2::TokenStream::new();
    ty.to_tokens(&mut s);
    s.to_string()
}
```

Add `quote = "1"` and `proc-macro2 = "1"` to `xtask/Cargo.toml` `[dependencies]` for the `ToTokens` trait.

- [ ] **Step 6: Run the test, confirm pass**

```bash
cargo build -p xtask
cargo test -p xtask --test lint_secrets
```

Expected: both fixtures classified correctly.

- [ ] **Step 7: Run the lint over the live workspace**

Run: `cargo run -p xtask -- lint-secrets --path .`

If violations turn up on real code, **fix them now** as part of this commit. Expected hot-spots: any `#[derive(Debug)]` struct in `origin-provider-*` crates that carries an `api_key: String` field (those should already be `Secret<String>` per P8). For any genuine false positive, prefer adding a `#[redact]` attribute on the field over disabling the rule.

If the violation count is non-zero and the offending struct is private, add a `#[allow(dead_code)] #[redact]` annotation or refactor the field to `Secret<…>`. Do not whitelist the path-glob.

- [ ] **Step 8: Verification gate**

```bash
cargo test -p xtask
cargo clippy -p xtask --all-targets -- -D warnings
cargo fmt --check
cargo run -p xtask -- lint-secrets --path .    # exits 0 on clean tree
```

- [ ] **Step 9: Commit**

```bash
git add xtask Cargo.toml Cargo.lock
git commit -m "feat(xtask): lint-secrets — Secret<T> redaction enforcement (P11.14)"
```

---

# Final phase gate

## Task P11.15 — Workspace sweep + tag `p11-complete`  **[sequential after all clusters]**

**Files:** none modified at this task; this is the merge / verification gate.

- [ ] **Step 1: Confirm every cluster has landed on `phase-11`**

```bash
git log --oneline dev..phase-11
```

Expected: 15 commits, one per task P11.0 through P11.14.

- [ ] **Step 2: Workspace-wide check**

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

All four exit 0.

- [ ] **Step 3: Per-OS sandbox check** (on each available host, or via cross-`check`)

```bash
# Windows host:
cargo test -p origin-sandbox --features windows
cargo check -p origin-sandbox --features linux
cargo check -p origin-sandbox --features macos

# Linux host (CI):
cargo test -p origin-sandbox --features linux

# macOS host (CI):
cargo test -p origin-sandbox --features macos
```

- [ ] **Step 4: Smoke test the daemon under sandbox**

Start the daemon with the active-OS sandbox feature, run a short scripted session through `Bash`, confirm the trace ring populated and `/metrics` responds:

```bash
cargo run -p origin-cli --features sandbox-windows -- &        # Windows host (adjust feature per host)
sleep 1
curl -s http://127.0.0.1:9876/metrics | grep origin_tool_call_total
cargo run -p origin-cli -- trace query --kind tool --limit 5
```

Expected: at least one row reported.

- [ ] **Step 5: Run the bench gates one more time**

```bash
cargo bench -p origin-trace --bench write -- --quick
cargo bench -p origin-metrics --bench encode -- --quick
```

Both report rates / latencies inside the thresholds set in P11.10 / P11.12.

- [ ] **Step 6: Tag and push**

```bash
git tag -a p11-complete -m "Phase 11 — security + observability + sandboxing"
git push origin phase-11
git push origin p11-complete
```

- [ ] **Step 7: Open PR `phase-11` → `dev`**

```bash
gh pr create --base dev --head phase-11 \
  --title "Phase 11 — security + observability + sandboxing (P11.0–P11.15)" \
  --body "$(cat <<'EOF'
## Summary
- New crates: `origin-sandbox`, `origin-trace`, `origin-metrics`, `xtask`.
- Per-OS sandbox profiles + hook profile inheritance.
- MCP 16 MiB inbound cap + `input_schema` validation.
- `tracing` → parquet ring + `origin trace query` subcommand.
- Bounded-cardinality metrics + `/metrics` Prometheus + TUI `?metrics` panel.
- 30-day KeyVault audit ring.
- `xtask lint-secrets` CI lint enforcing `Secret<T>` discipline.

## Test plan
- [x] `cargo test --workspace` green on Windows.
- [ ] CI: `cargo test -p origin-sandbox --features linux` green.
- [ ] CI: `cargo test -p origin-sandbox --features macos` green.
- [x] `cargo bench -p origin-trace --bench write -- --quick` ≥ 100k rows/s.
- [x] `cargo bench -p origin-metrics --bench encode -- --quick` ≤ 200 µs.
- [x] `cargo run -p xtask -- lint-secrets --path .` exits 0.
EOF
)"
```

Capture the PR URL.

---
