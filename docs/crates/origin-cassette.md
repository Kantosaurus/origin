# origin-cassette

> Deterministic, secret-safe HTTP cassette recording and sequential replay matching

## Purpose

`origin-cassette` records HTTP request/response shapes to a JSON "cassette" and
replays them deterministically for tests and offline runs — the VCR pattern,
narrowed to exactly what affects matching. Its differentiator is **secret
safety**: it refuses to persist a cassette that still carries a live credential,
scrubbing auth headers and bearer/`sk-`/URL-embedded tokens in place and gating
saves in CI. The secret scan is regex-free (no `regex`/`once_cell` dep,
MSRV-safe) and runs on pure byte/char heuristics.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Cassette` | struct | Named, ordered interactions with an in-memory replay cursor. |
| `Interaction` | struct | One `{ request: ReqShape, response: RespShape }` pair. |
| `ReqShape` / `RespShape` | struct | Stored request / response shapes (method, url, headers, body / status, …). |
| `Cassette::record` / `take_next` / `match_next` / `rewind` / `cursor` / `set_cursor` | fn | Append + sequential, shape-based consumption. |
| `Cassette::to_json` / `from_json` | fn | Lossless JSON (de)serialization. |
| `scrub_secrets` | fn | Redact secrets across every interaction in place; returns count. |
| `assert_redacted` | fn | CI gate: error on the first unredacted secret. |
| `contains_secret` | fn | Regex-free heuristic secret detector. |
| `position_path` / `replay_next` | fn | Durable, cross-call sequential replay via a `<cassette>.pos` sidecar. |
| `CassetteError` | enum | `Serde` / `UnredactedSecret` / `Io` / `ReplayMiss`. |
| `REDACTED` | const | The `"***"` sentinel. |

## Key types

```rust
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Cassette {
    pub name: String,
    pub interactions: Vec<Interaction>,
    #[serde(skip)]
    cursor: AtomicUsize, // transient; never serialized; ignored by Eq/Clone
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReqShape { pub method: String, pub url: String,
    pub headers: Vec<(String, String)>, pub body: String }
```

## How it works

Replay is **sequential by `(method, url)` shape**, not byte-exact: headers and
bodies vary across runs (timestamps, nonces) but the call sequence is stable.
`take_next` scans forward from the cursor for the next matching interaction and
advances past it, so recording `A, B` to the same URL and calling twice yields
`A` then `B` — not `A` twice (the multi-turn-agent regression this design fixes).
The cursor is in-memory only; durable cross-process position is provided by
`replay_next`, which reloads the cassette, restores the cursor from a `<path>.pos`
sidecar, consumes one match, and persists the advanced position.

Secret handling has two halves. `scrub_secrets` redacts `authorization` /
`x-api-key` / `proxy-authorization` header values, URL userinfo and embedded
query keys, and bearer/`sk-`/`api_key=`/long-opaque tokens in bodies — returning
how many values changed. `assert_redacted` is the CI save gate that errors with a
located message on the first leak. Critically, URL matching canonicalizes both
sides through the same redaction, so a *scrubbed* recorded URL (e.g. Gemini's
`?key=…`) still matches the *live* secret-bearing probe on replay.

```text
record ──► Cassette { interactions } ──scrub_secrets──► assert_redacted ──► JSON
replay ──► replay_next(path, method, url) ◄── <path>.pos cursor (durable)
```

`contains_secret` is deliberately conservative: a marker followed only by `***`
is "already scrubbed", and the opaque-token heuristic requires ≥32 chars of the
base64/hex alphabet with at least one digit — so it catches API keys without
flagging ordinary prose.

## Dependencies & features

- `serde` / `serde_json` — the only runtime deps (no regex engine, by design).
- `thiserror`.
- Dev: `tempfile`.
- `#![forbid(unsafe_code)]`. No cargo features.

## Used by

Per `Grep "origin-cassette" crates/*/Cargo.toml`: `origin-provider-anthropic` and
`origin-provider-openai-compat` (provider replay taps call `match_next`).

## Testing

An extensive in-file `#[cfg(test)]` module in `src/lib.rs` covers header/body/URL
scrubbing, the sequential-advance regression, `replay_next` cursor persistence
across fresh loads, CI-gate failures, JSON round-trips, idempotent scrubbing, and
the `contains_secret` heuristics.

## See also

- [../security/security-model.md](../security/security-model.md) — the secret-safety gate.
- [origin-replay.md](origin-replay.md) — the broader deterministic-replay subsystem.
- [../subsystems/providers.md](../subsystems/providers.md) — providers that record/replay against cassettes.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
