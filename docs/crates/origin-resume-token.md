# origin-resume-token

> MAC-authenticated session resume token shared by the daemon and supervisor

## Purpose

`origin-resume-token` is a tiny leaf crate carrying the cross-process
`ResumeToken` shape. The daemon (writer) checkpoints a token at each
assistant-turn boundary; the supervisor (replayer) reads any tokens on restart
and replays them to the next daemon over IPC. Keeping the type in a leaf breaks a
would-be daemon ↔ supervisor dependency cycle. Crucially, every token on disk is
**MAC-authenticated**, so an attacker who can write the resume directory cannot
swap a CAS handle and steer the resumed daemon into arbitrary content.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `ResumeToken` | struct | Snapshot of an open session sufficient to resume it. |
| `ResumeToken::save` | fn | Write `<dir>/<session_id>.json` as a MAC-wrapped envelope. |
| `ResumeToken::load_one` | fn | Load + verify a single token by session id (`Ok(None)` if absent). |
| `ResumeToken::load_all` | fn | Load + verify every `*.json` token under a dir. |

## Key types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeToken {
    pub session_id: String,
    pub last_turn: u32,
    pub cas_handle_root: [u8; 32],          // CAS root for the message log
    pub pending_tool_calls: Vec<String>,    // in-flight tool calls at checkpoint
    pub plan_seq: u64,
    #[serde(default)] pub goal: Option<origin_goal::GoalSnapshot>,
    #[serde(default)] pub detached_at_unix: Option<u64>,
    #[serde(default)] pub memory_estimate_bytes: Option<u64>,
}
```

The on-disk envelope (internal `OnDisk`) is:

```text
{ "payload": "<inner ResumeToken JSON, compact, as a STRING>",
  "mac_hex": "<hex of blake3::keyed_hash(key, payload.as_bytes())>" }
```

## How it works

Tokens live at `<state_dir>/resume/<session_id>.json`. On `save`, the inner
`ResumeToken` is serialized to compact JSON and embedded **as a string field**;
the MAC input is then literally `payload.as_bytes()` — no canonicalization
round-trip, no formatter sensitivity. The MAC is `blake3::keyed_hash` under a
32-byte key stored at `<dir>/.mac-key`, generated on first save via `getrandom`
and `chmod 0600` on unix. On windows the stdlib cannot tighten the ACL, so the
enclosing state dir must already be user-private (documented gap).

On load, the wrapper is parsed, the key is read **strictly** (a missing key is an
error — never auto-generated, so a deleted key can't let a tampered token slide
through), the MAC is recomputed and compared in **constant time** (`subtle::ct_eq`),
and only then is the inner payload deserialized. There is no back-compat for the
pre-MAC bare-JSON format — an unwrapped file errors out.

```text
daemon: ResumeToken::save(dir)  ──► <session_id>.json  +  .mac-key
supervisor (restart): ResumeToken::load_all(dir) ──verify MAC──► replay over IPC
```

`session_id` is attacker-influenced (it arrives over the wire) and is
interpolated into a filename, so `validate_session_id` permits only a single
`Normal` path component — blocking `../../evil` traversal. The newer optional
fields (`goal`, `detached_at_unix`, `memory_estimate_bytes`) use
`#[serde(default)]` so tokens written before they existed still deserialize.

## Dependencies & features

- `blake3` (keyed MAC), `getrandom` (key generation), `subtle` (constant-time
  compare), `hex`.
- `serde` / `serde_json` (envelope), `thiserror`.
- `origin-goal` — `GoalSnapshot` carried in the token.
- Dev: `tempfile`.
- `#![forbid(unsafe_code)]`. No cargo features.

## Used by

Per `Grep "origin-resume-token" crates/*/Cargo.toml`: `origin-daemon` (writer) and
`origin-supervisor` (replayer).

## Testing

In-file `#[cfg(test)]` module in `src/lib.rs`: round-trip, missing-dir,
MAC-mismatch rejection, missing-key rejection, key persistence across saves,
legacy-format rejection, and `#[serde(default)]` backward-compat. Additionally
`crates/origin-resume-token/tests/goal_snapshot_round_trip.rs`.

## See also

- [../security/security-model.md](../security/security-model.md) — token authentication and the threat it defends.
- [../subsystems/agent-and-sessions.md](../subsystems/agent-and-sessions.md) — checkpoint/resume lifecycle.
- [origin-cas.md](origin-cas.md) — the `cas_handle_root` the token references.
- Back to [../crates/README.md](../crates/README.md).

_Last reviewed against workspace version 0.9.8._
