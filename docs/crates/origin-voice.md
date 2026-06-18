# origin-voice

> Speech-to-text dictation config and transcript interleave policy.

## Purpose

`origin-voice` owns the dictation configuration and the queue/interleave policy
that turns a stream of partial and final speech-to-text transcripts into
submittable prompt chunks. The STT engine itself is shelled out by the caller via
a configuration the crate builds into an argv — the crate stays synchronous,
side-effect free, and offline-testable. It is `#![forbid(unsafe_code)]`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `DictationConfig` | struct | `{ command, args, language, device }`. |
| `DictationConfig::new(command, args)` | fn | Config with no language/device. |
| `DictationMode` | enum | `Queue` (submit on final) / `Interleave` (submit each partial). |
| `Transcript` | struct | `{ text, is_final }` STT fragment. |
| `Transcript::new(text, is_final)` | fn | Construct a fragment. |
| `DictationSession` | struct | Stateful accumulator applying a mode's policy. |
| `DictationSession::new(mode)` / `push` / `take_ready` / `pending` | fn | Feed transcripts, pull ready chunks. |
| `build_command(&cfg)` | fn → `(String, Vec<String>)` | Resolve the full STT argv. |
| `validate(&cfg)` | fn → `Result<(), VoiceError>` | Reject an empty command. |
| `VoiceError` | enum | `EmptyCommand`. |

## Key types

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictationConfig {
    pub command: String,
    pub args: Vec<String>,
    pub language: Option<String>,
    pub device: Option<String>,
}

pub enum DictationMode { Queue, Interleave }
```

## How it works

A `DictationSession` applies its `DictationMode` to each pushed `Transcript`:

```
Queue mode:                          Interleave mode:
  push(partial) → buffer text          push(partial, non-empty) → ready = text
  push(final)   → ready = buffer       (each fragment emitted eagerly)
                  buffer cleared        pending() always ""
```

In `Queue` mode, non-empty fragment text is appended (space-separated) to an
internal buffer and only promoted to `ready` once a `final` transcript arrives;
an all-whitespace utterance produces nothing. In `Interleave` mode, each non-
empty (trimmed) partial is marked ready immediately and `pending()` stays empty.
`take_ready` consumes the chunk, returning `None` until more transcripts are
pushed.

`build_command` clones the base argv and appends `--language <value>` and
`--device <value>` only when those optional fields are set, returning the
executable name plus the resolved argument vector. `validate` rejects an empty or
whitespace-only command with `VoiceError::EmptyCommand` before the caller spawns
a process.

## Dependencies & features

- `serde` (with `derive`) — config / transcript / mode serialization.
- `thiserror` — `VoiceError`.
- `#![forbid(unsafe_code)]`; no cargo features; no audio/process crates (the STT
  process is the caller's responsibility, configured via `build_command`).

## Used by

`Grep "origin-voice" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-voice/Cargo.toml` (self)

## Testing

Inline tests cover: `Queue` buffering until final then yielding once (and
clearing); `Interleave` yielding each non-empty partial and skipping empty ones;
`build_command` injecting language + device flags and omitting them when absent;
`validate` rejecting empty/whitespace commands and accepting a real one;
`take_ready` returning `None` when nothing is ready; and `Queue` dropping an
all-whitespace utterance.

## See also

- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
