# Observability Runbook

Operational how-to for the three **independent** observability concerns in
`origin`: developer **tracing**, operational **metrics**, and opt-in product
**telemetry**, plus the `origin doctor` diagnostics and the TUI `?metrics`
panel. For the *design* (schemas, cardinality math, redaction internals) see
[`../subsystems/observability.md`](../subsystems/observability.md); this page is
the **do-this-to-get-that** companion.

> Quick reference: traces are **on** (local, postmortem); metrics counters are
> always live but `/metrics` is **off** until you bind it; OTLP export and product
> telemetry are **off by default** and only ever leave the machine when you
> explicitly enable them.

---

## Signals at a glance

| Signal | Default | Leaves machine? | Enable | Read with |
|---|---|---|---|---|
| Trace spans (parquet ring) | **On**, local | No (unless OTLP) | always on | DuckDB / Arrow / `query::run` |
| Daemon text log | **On**, local | No | always on | `tail -f daemon.log` |
| Operational counters | **On** (in-process) | No | always live | `?metrics` panel, `/metrics` |
| Prometheus `/metrics` | **Off** | Only when bound | `--metrics-bind` / `ORIGIN_METRICS_BIND` | Prometheus scrape |
| OTLP metrics + spans | **Off** | Only when enabled | `otel` feature **+** `ORIGIN_OTLP_ENDPOINT` | your OTLP collector |
| Product telemetry (JSONL) | **Off** (opt-in) | Only if opted in | opt-in **+** non-zero sample | your sink |
| Diagnostics report | On demand | No | `origin doctor` | stdout / JSON |

---

## Traces: parquet ring + OTLP

### Reading the local ring (postmortem)

The daemon writes **one row per span close** to a per-day parquet ring under
`<data>/origin/trace/`, rotating files at **64 MiB**. Files are named
`trace-<YYYY-MM-DD>-<unix_ms>-<seq>.parquet` and sort chronologically by name.
The ring is **not human-readable** — query it with any parquet tool:

```sh
# DuckDB: last 100 provider errors across the ring
duckdb -c "SELECT ts_ns, provider, tool, error_kind, dur_us
           FROM read_parquet('<data>/origin/trace/trace-*.parquet')
           WHERE error_kind <> ''
           ORDER BY ts_ns DESC LIMIT 100;"
```

The schema columns are: `ts_ns, span_id, parent_id, kind, provider, tool,
dur_us, error_kind, attrs_json`. The in-process reader (`query::run`) supports
**pushdown predicates** on the two low-cardinality columns `kind` and
`error_kind`, plus a `limit`. A missing trace dir = "no traces yet" (empty, not
an error). Under backpressure a span row is **dropped** rather than block the
agent loop — traces are best-effort.

### Tailing the live text log

For "what is the daemon doing right now," tail the human-readable log:

```sh
tail -f "$HOME/.local/share/origin/logs/daemon.log"   # Linux
# macOS:   ~/Library/Application Support/origin/logs/daemon.log
# Windows: %LOCALAPPDATA%\origin\logs\daemon.log
```

Raise verbosity by starting the daemon with `ORIGIN_LOG=debug` (then `RUST_LOG`,
default `info`). The log truncates on each daemon start.

### Enabling OTLP span + metric export

OTLP requires **both** the `otel` build feature **and** the runtime endpoint:

```sh
# 1) build with the feature
cargo build --release --features otel -p origin-daemon
# 2) run pointing at a collector (gRPC)
ORIGIN_OTLP_ENDPOINT=http://localhost:4317 origin-daemon
```

This installs the global meter provider and a best-effort span pipeline against
the same endpoint; metrics flush every **30 s** (10 s per-export timeout). Both
signals share a `service.name` `Resource` so they correlate. The wire format uses
**OpenTelemetry GenAI semantic conventions** (`gen_ai.usage.input_tokens`,
`gen_ai.system`, `gen_ai.request.model`, `gen_ai.tool.name`, …). An unreachable
collector still installs cleanly — failed exports are retried/dropped off the hot
path. Off by default; nothing leaves the machine unless you do **both** steps.

---

## Metrics: Prometheus scrape

The counters are always live in-process; the `/metrics` HTTP endpoint is **off
until you bind it**:

```sh
# CLI flag …
origin-daemon --metrics-bind 127.0.0.1:9090
# … or env var
ORIGIN_METRICS_BIND=127.0.0.1:9090 origin-daemon
```

Point Prometheus at it:

```yaml
scrape_configs:
  - job_name: origin
    static_configs:
      - targets: ["127.0.0.1:9090"]
```

```sh
curl -s http://127.0.0.1:9090/metrics | grep '^origin_'
```

A bind failure is **logged and does not abort the daemon**. The exposition is
**cardinality-bounded by construction** (a static allowlist of 7 providers ×
18 tools × 3 results, each plus an `_other_` bucket), so scrape size is bounded
regardless of workload — a pathological MCP server cannot inflate it.

### Counter families

| Family | Labels | Meaning |
|---|---|---|
| `origin_tool_call_total` | `provider`, `tool`, `result` | Tool invocations (≤ 456 series). |
| `origin_tokens_in_total` | `provider`, `model` | Input tokens billed. |
| `origin_tokens_out_total` | `provider`, `model` | Output tokens billed. |
| `origin_cache_hit_total` | `provider` | Prompt-cache reads served from cache. |
| `origin_sandbox_violation_total` | `profile`, `kind` | Kernel-enforced sandbox denials. |

Useful PromQL:

```promql
# tool error rate (last 5m)
sum(rate(origin_tool_call_total{result="err"}[5m]))
  / sum(rate(origin_tool_call_total[5m]))

# prompt-cache hit rate
sum(rate(origin_cache_hit_total[5m])) / sum(rate(origin_tokens_in_total[5m]))

# sandbox denials by kind
sum by (kind) (rate(origin_sandbox_violation_total[5m]))
```

### The `?metrics` TUI panel

Pressing `?` in the interactive TUI toggles a live metrics view (reads the same
registry the daemon increments), so the panel, `/metrics`, and `origin usage`
all report the same real numbers. No setup required.

---

## Product telemetry: on / off

Product telemetry is **off unless you opt in**, and `DO_NOT_TRACK` **always
wins**. The decision is `enabled = opt_in && !do_not_track`.

```sh
# Guarantee silence (overrides any opt-in):
export DO_NOT_TRACK=1
```

**To enable** (self-hosted): opt in explicitly, set a non-zero sample rate, and
point the host sink at your own collector endpoint. Everything emitted is
**redacted** (secret-looking values → `***`) and **deterministically sampled**
before it reaches your sink; the crate never opens a socket itself (delivery is
the host's job). Nothing is sent unless you opt in.

---

## `origin doctor` diagnostics

```sh
origin doctor          # text report
origin doctor --json   # machine-readable
```

Six probes run in a stable order; the report's verdict is the worst of them:

| Probe | Ok | Warn | Fail |
|---|---|---|---|
| `toolchain` | ≥ MSRV 1.83 | unknown version | older than 1.83 |
| `config` | found | none (runs on defaults) | — |
| `daemon` | reachable | not running (starts on demand) | — |
| `providers` | ≥ 1 configured | — | none configured |
| `home` | writable | — | not writable (sessions can't persist) |
| `network` | verified | not checked (offline) | probe failed |

The report also prints the **phone-home disclosure** — every outbound behaviour
the tool can perform: `npm auto-update check (disable with ORIGINX_NO_UPDATE=1)`,
`model/provider API requests`, and `optional telemetry (opt-in)`. This list is
always shown, even when checks fail.

---

## What to check when X

| Symptom | First look | Then |
|---|---|---|
| Agent slow / wedged | `tail -f daemon.log` (set `ORIGIN_LOG=debug`) | Query the parquet ring for high `dur_us` spans by `tool`/`provider`. |
| Provider errors spiking | `origin_tool_call_total{result="err"}` | Ring filter `error_kind` (e.g. `rate_limit`); check provider config. |
| Cache cost too high | `origin_cache_hit_total` rate, `origin usage` | Cold-cache nudge in TUI; see cost accounting in observability.md. |
| Sandbox denials | `origin_sandbox_violation_total` by `kind` | Cross-ref [`troubleshooting.md`](./troubleshooting.md) sandbox rows. |
| `/metrics` returns nothing | confirm `--metrics-bind`/`ORIGIN_METRICS_BIND` set | bind failure is logged in `daemon.log`. |
| No spans in collector | confirm `otel` feature **and** `ORIGIN_OTLP_ENDPOINT` | collector reachable on `:4317`; exports flush every 30 s. |
| "Is anything leaking?" | `origin doctor` phone-home section | set `ORIGINX_NO_UPDATE=1` / `DO_NOT_TRACK=1` to silence outbound. |
| Daemon won't start | `origin doctor` `home`/`providers`/`toolchain` | see daemon restart-storm guidance in [`daemon-and-supervisor.md`](./daemon-and-supervisor.md). |
| High RSS | supervisor sheds background sessions | tune `ORIGIN_SUPERVISOR_MEM_BUDGET_MB`; see troubleshooting. |

---

## Cheat sheet

```sh
# Tracing
ORIGIN_LOG=debug origin-daemon                       # verbose text log
duckdb -c "SELECT * FROM read_parquet('…/trace/*.parquet') LIMIT 50;"

# Metrics
ORIGIN_METRICS_BIND=127.0.0.1:9090 origin-daemon
curl -s 127.0.0.1:9090/metrics | grep origin_

# OTLP (build + run)
cargo build --release --features otel -p origin-daemon
ORIGIN_OTLP_ENDPOINT=http://localhost:4317 origin-daemon

# Telemetry off (belt and braces)
export DO_NOT_TRACK=1 ORIGINX_NO_UPDATE=1

# Health
origin doctor --json
```

---

_Last reviewed against workspace version 0.9.8._
