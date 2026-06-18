# origin-notify

> Out-of-band human notifications with quiet-hours, batching policy, and injectable channel dispatch.

## Purpose

`origin-notify` models a notification, a quiet-hours window, and a batching
policy, then decides whether and how to deliver each notification over a
`Channel`. It builds the OS-native command line for desktop/command channels and
serializes a JSON payload for webhooks, but performs no process or network side
effects itself — delivery is the caller's job — so the crate is fully unit-
testable offline. It is `#![forbid(unsafe_code)]`.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Channel` | enum | `Webhook { url }` / `Desktop` / `Command { program, args }`. |
| `Notification` | struct | `{ title, body, urgent }`. |
| `Notification::new(title, body, urgent)` | fn | Construct a notification. |
| `QuietHours` | struct | `{ start_min, end_min }` minutes-of-day window. |
| `QuietHours::new(start, end)` | fn | Window (may wrap midnight). |
| `QuietHours::is_quiet(minute_of_day)` | fn → `bool` | Whether the minute is inside the window. |
| `Batcher` | struct | FIFO buffer that signals flush at `max_batch`. |
| `Batcher::new(max)` / `push` / `flush` / `len` / `is_empty` | fn | Bounded batching API. |
| `should_send(&n, quiet, minute_of_day)` | fn → `bool` | Urgent bypasses; else suppressed in quiet hours. |
| `desktop_command(&n)` | fn → `(String, Vec<String>)` | OS-native notifier command. |
| `try_webhook_payload(&n)` / `webhook_payload(&n)` | fn | JSON payload (fallible / infallible). |
| `NotifyError` | enum | `Build(String)`. |

## Key types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    Webhook { url: String },
    Desktop,
    Command { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    pub start_min: u32,  // inclusive
    pub end_min: u32,    // exclusive
}
```

## How it works

Quiet hours are expressed as minutes-of-day and may wrap midnight: when
`start < end` a minute is quiet inside `[start, end)`; when `start > end`
(e.g. 23:00–07:00) it is quiet in `[start, midnight) ∪ [0, end)`; a degenerate
`start == end` window is never quiet.

```
should_send(n, quiet, t):
   n.urgent           → true            (always bypasses)
   quiet is None       → true
   else                → !quiet.is_quiet(t)
```

`Batcher::push` appends FIFO and returns `true` once the buffer reaches
`max_batch` (normalised to at least 1), signalling the caller to `flush`, which
drains the buffer with `mem::take`. `desktop_command` selects per platform:
`osascript -e "display notification …"` on macOS, a WinRT `powershell` toast on
Windows, and `notify-send` (with `-u critical` when urgent) elsewhere — with
AppleScript / PowerShell / JSON quoting helpers escaping each field.
`webhook_payload` is the infallible wrapper over `try_webhook_payload`, falling
back to a hand-built JSON object so the caller always gets valid JSON.

## Dependencies & features

- `serde` / `serde_json` — `Channel` / `Notification` serde + webhook payload.
- `thiserror` — `NotifyError`.
- `#![forbid(unsafe_code)]`; no cargo features. Platform behaviour is selected
  with `cfg!(target_os = …)` at runtime, not via features.

## Used by

`Grep "origin-notify" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-daemon/Cargo.toml`
- `crates/origin-notify/Cargo.toml` (self)

## Testing

Inline tests cover wrap-around, non-wrapping, and degenerate quiet windows;
urgent bypass and no-quiet-hours always-send paths; `Batcher` flushing at max,
flushing on demand below max, empty-flush, and the zero-max normalisation;
webhook payload validity (with embedded quotes/newlines) and round-trip; and
`desktop_command` returning a non-empty program + args that contain the title /
body text for the current platform. `Channel` serde round-trips.

## See also

- [Observability subsystem](../subsystems/observability.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
