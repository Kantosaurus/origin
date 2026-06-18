# origin-telemetry

> Opt-in, self-hostable product telemetry pipeline with secret redaction and sampling.

## Purpose

`origin-telemetry` computes the redacted, sampled JSONL lines a host *should*
ship for product analytics, while performing no network or filesystem I/O itself
— delivery is left to a caller-supplied sink. It honors the `DO_NOT_TRACK`
convention and explicit opt-in, applies deterministic hash-based sampling so
retries never change inclusion, and redacts values that look like secrets before
serialization. The crate is pure and `#![forbid(unsafe_code)]`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `REDACTED` | const | The `***` placeholder substituted for secrets. |
| `Event` | struct | `{ name, props: Vec<(String,String)>, ts_unix_ms }`. |
| `Event::new(name, ts)` | fn | Event with no properties. |
| `redact(&mut [props])` | fn → `usize` | Redact secret-shaped values in place; returns count. |
| `Config` | struct | `{ enabled, sample_rate, endpoint }`. |
| `Config::from_env(do_not_track, opt_in, sample)` | fn | DNT always wins; sample clamped to `0.0..=1.0`. |
| `Config::with_endpoint(url)` | fn | Set the delivery endpoint. |
| `should_emit(&cfg, event_hash)` | fn → `bool` | Deterministic sampling decision. |
| `event_hash(&Event)` | fn → `u64` | Stable FNV-1a hash over name + timestamp. |
| `to_jsonl(&Event)` | fn → `Result<String, TelemetryError>` | One redacted compact JSON line. |
| `SessionStopReason` | enum | `snake_case` pain buckets. |
| `PainMetrics` | struct | Optional per-session agent-time / pain metrics. |
| `PainMetrics::into_event(name, ts)` | fn → `Result<Event, …>` | Fold metrics into a redactable event. |
| `Pipeline` | struct | Buffers events; `record` / `drain` / `pending`. |
| `TelemetryError` | enum | `Serde(String)`. |

## Key types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    pub name: String,
    pub props: Vec<(String, String)>,
    pub ts_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub enabled: bool,
    pub sample_rate: f64,
    pub endpoint: Option<String>,
}
```

## How it works

`Config::from_env` computes `enabled = opt_in && !do_not_track` — `DO_NOT_TRACK`
always forces telemetry off — and clamps (NaN-safe) the sample rate. Redaction
flags values via prefix checks (`sk-`, `ghp_`, `xoxb-`, `Bearer …`, `AIza…`),
inline `key=secret` assignments, and long hex / mixed base64 blobs, replacing
each with `REDACTED` while never touching keys.

```
Pipeline::record(Event) ──► buffer (always)
Pipeline::drain():
   if !cfg.enabled → clear buffer, return []
   else for each event:
       should_emit(cfg, event_hash(event))?  ──► to_jsonl (redact + compact)
```

Sampling is deterministic: `event_hash` is FNV-1a over the event name + LE
timestamp bytes, and `should_emit` keeps an event when
`hash / u64::MAX < sample_rate` (with `<= 0.0` never emitting and `>= 1.0`
always emitting for an enabled config). `PainMetrics` carries an optional
model/tool time split, time-to-first-useful-action, turn count, autonomy streak,
and a `SessionStopReason`; `into_event` serializes it under a single
`pain_metrics` property so it rides the existing JSONL sink unchanged. All
optional fields use `skip_serializing_if` so an empty record serializes to `{}`.

## Dependencies & features

- `serde` / `serde_json` — event + metrics serialization.
- `thiserror` — `TelemetryError`.
- No cargo features; no transport dependencies (delivery is the caller's job).

## Used by

`Grep "origin-telemetry" glob "crates/*/Cargo.toml"`:

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-telemetry/Cargo.toml` (self)

## Testing

A rich inline test module covers: DNT forcing disabled, opt-in enabling, sample
clamping/NaN safety, redaction of `sk-`/`Bearer`/assignment/long-blob values,
deterministic + extreme sampling, JSONL validity + redaction, `Pipeline` drain
semantics (empty when disabled, emit + redact when enabled), `SessionStopReason`
stable tag round-trips, `PainMetrics` round-trip + partial-split totals, and the
invariant that the plain turn-event serialization is byte-identical
(`{"name":"turn","props":[["provider","anthropic"]],"ts_unix_ms":42}`).

## See also

- [Observability subsystem](../subsystems/observability.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
