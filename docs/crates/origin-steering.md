# origin-steering

> Mid-execution steering hints queued and merged into the next agent turn.

## Purpose

`origin-steering` is the pure queue-and-merge layer behind the agent's
"steer while running" capability. A user can type extra guidance while an
agent turn is already in flight; that text becomes a *hint* that is buffered
and folded into the prompt assembled for the **next** turn without
interrupting or restarting the running turn. The crate performs no I/O, holds
no async state, and never touches a provider — it is a `VecDeque` plus two
string-merge helpers, so the daemon owns the loop while this crate owns the
ordering and the wire format of the injected block.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `SteeringQueue` | struct | FIFO buffer of pending steering hints. |
| `SteeringQueue::push` | method | Queue a hint for the next turn. |
| `SteeringQueue::drain_block` | method | Drain all hints into one newline-joined block, clearing the queue. |
| `SteeringQueue::is_empty` / `len` | method | Queue introspection. |
| `merge_into_prompt` | fn | Append a steering block *after* base user text (cache-safe). |
| `wrap_block` | fn | Wrap a block in the markers with no base text. |
| `STEER_OPEN` / `STEER_CLOSE` | const | `<steering>` / `</steering>` delimiters. |
| `SteeringError` | enum | Reserved error type (`EmptyHint`); the queue itself is infallible. |

## Key types

```rust
pub struct SteeringQueue { /* hints: VecDeque<String> */ }

impl SteeringQueue {
    pub fn push(&mut self, hint: impl Into<String>);
    pub fn drain_block(&mut self) -> Option<String>; // None when empty
}

pub const STEER_OPEN: &str = "<steering>";
pub const STEER_CLOSE: &str = "</steering>";

#[must_use]
pub fn merge_into_prompt(base_user_text: &str, steering_block: Option<&str>) -> String;

#[must_use]
pub fn wrap_block(steering_block: &str) -> String;
```

## How it works

Hints accumulate in insertion order while a turn runs. When the daemon
assembles the next turn it calls `drain_block`, which joins every queued hint
with newlines into a single block and empties the queue. The block is placed
**after** the base user text by `merge_into_prompt`:

```
[ system + prior turns + base user text ]   <- stable, byte-identical prefix
                                             <- blank line
<steering>
focus on tests
avoid touching siblings
</steering>
```

Keeping steering as a *trailing suffix* is the central design choice: the
stable prefix stays byte-identical between turns, so Anthropic prefix caching
remains warm and the steering text never invalidates the cache. `wrap_block`
provides the same markered form when the daemon appends steering as its own
separate user-message block instead of concatenating.

## Dependencies & features

- Runtime deps: `thiserror` only. No async runtime, no serde, no provider — a
  true leaf crate.
- `#![forbid(unsafe_code)]`. No Cargo features.

## Used by

`Grep "origin-steering" glob "crates/*/Cargo.toml"` →

- `crates/origin-cli/Cargo.toml`
- `crates/origin-steering/Cargo.toml` (self)

The CLI is the consumer that turns typed-while-running text into queued hints;
the daemon's turn assembly merges the drained block via the helpers above.

## Testing

In-file `#[cfg(test)] mod tests` (no `tests/` dir). Coverage includes
push/drain ordering, `drain_block` returning `None` on an empty queue (and
again after draining), `merge_into_prompt` with `None` equalling the base
text, marker placement *after* the base text (steering is never a prefix),
`wrap_block` exact form, and a full merge→drain round-trip.

## See also

- [Swarm & orchestration subsystem](../subsystems/swarm-and-orchestration.md)
- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
