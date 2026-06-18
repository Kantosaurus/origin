# origin-metrics

> Bounded-cardinality counters with a Prometheus text encoder

## Purpose

`origin-metrics` wraps the `prometheus` crate's `IntCounterVec` surface with a
static label allowlist enforced at the call site, so a pathological MCP server
cannot inflate metric cardinality — unknown provider/tool/result strings collapse
into a single `_other_` bucket. It exposes a fast pre-rendered text-exposition
path for the `/metrics` endpoint and an in-process snapshot for the TUI metrics
panel. An optional `otel` feature adds an OpenTelemetry OTLP metrics + trace
pipeline and `gen_ai.*` semantic-convention instruments.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Metrics::new()` / `default()` | fn | Build a registry with all `origin_*` families pre-declared. |
| `Metrics::tool_call_total(provider, tool, result)` | fn → `IntCounter` | `origin_tool_call_total` handle (allowlisted labels). |
| `Metrics::tokens_in_total` / `tokens_out_total(provider, model)` | fn → `IntCounter` | Token counters (`model` interned, not allowlisted). |
| `Metrics::cache_hit_total(provider)` | fn → `IntCounter` | Prompt-cache hit counter. |
| `Metrics::sandbox_violation_total(profile, kind)` | fn → `IntCounter` | Sandbox-denial counter. |
| `Metrics::encode_text()` | fn → `Result<String, MetricsError>` | Prometheus text exposition (fast path). |
| `Metrics::snapshot()` | fn → `Snapshot` | In-process rows for the TUI `?metrics` panel. |
| `Metrics::registry()` | fn → `Arc<Registry>` | Borrow the underlying registry for extra families. |
| `Snapshot` / `SnapshotRow` | struct | Sampled metric rows. |
| `MetricsError` | enum | `Encode` / `Register`. |
| `keys::canonical_provider` / `_tool` / `_result` | fn | Allowlist canonicalization to `_other_`. |
| `keys::genai` | mod | `gen_ai.*` semantic-convention name constants. |
| `keys::gen_ai_for_internal` / `gen_ai_attr_for_label` | fn | Internal → convention name mapping. |
| `instruments::record_gen_ai_usage` / `gen_ai_span` / … | fn | OTLP recording API (no-op without `otel`). |
| `exporter::otel::install` / `install_traces` | fn (`otel`) | Build + install the OTLP pipelines. |

## Key types

```rust
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,
    tool_call: IntCounterVec,
    tokens_in: IntCounterVec,
    tokens_out: IntCounterVec,
    cache_hit: IntCounterVec,
    sandbox_violation: IntCounterVec,
    headers: Arc<Vec<FamilyHeader>>,
    fast: Arc<Mutex<FastIndex>>,
}

pub struct SnapshotRow {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
}
```

## How it works

All five counter families are declared in `new()`, so the underlying registry
never sees a new family after construction. Each accessor canonicalizes its
labels (provider/tool/result through the `keys` allowlist; `model`/`profile`/
`kind` through a memoized `intern_label`) and registers the `(family, sorted
labels)` tuple into a parallel **fast index** that stores a pre-rendered
`origin_name{a="x",b="y"}` prefix plus a clone of the `IntCounter` handle.

```
accessor(provider, …) ─► canonicalize/intern ─► IntCounterVec::with_label_values
                                              └► register_fast() → FastIndex row
encode_text() ─► walk FamilyHeader headers ─► append matching FastIndex rows
                                            └► counter.get() (single atomic load)
```

`encode_text` walks the pre-rendered HELP/TYPE headers and emits one line per
fast-index row, reading each counter with a single atomic `get()` — avoiding
`Registry::gather()`'s protobuf clone walk. Label values are escaped for the
exposition format (`\\`, `\"`, `\n`) so untrusted model strings cannot inject
counterfeit metric lines. With the `otel` feature, `exporter::otel::install`
stands up an OTLP metrics pipeline (gRPC/tonic, 30s `PeriodicReader`), rebinds
the `gen_ai.*` instruments via `instruments::init_instruments`, and best-effort
installs a matching trace pipeline sharing one `service.name` resource.

## Dependencies & features

- `prometheus` — counter vectors + registry.
- `serde`, `thiserror` — snapshot rows + `MetricsError`.
- **`otel`** feature: `opentelemetry`, `opentelemetry-otlp` (`trace`),
  `opentelemetry_sdk` (`trace`) — real OTLP export and `gen_ai.*` instruments.
  Default build links only the zero-cost no-op stubs in `instruments`.
- Dev: `tokio` (multi-thread, for the OTLP install tests); an `encode` bench.

## Used by

`Grep "origin-metrics" glob "crates/*/Cargo.toml"`:

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-metrics/Cargo.toml` (self)
- `crates/origin-tui/Cargo.toml`

## Testing

Inline tests in `keys.rs` pin every `gen_ai.*` convention string and the
internal-family / label mappings. `instruments.rs` tests assert the attribute
sets and no-op behaviour (with and without `otel`). `exporter.rs` tests build
real OTLP metrics + trace pipelines against a valid endpoint with no live
collector (multi-thread Tokio, bounded by `timeout`). `tests/encode.rs` and the
`encode` bench cover the fast-path text exposition.

## See also

- [Observability subsystem](../subsystems/observability.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
