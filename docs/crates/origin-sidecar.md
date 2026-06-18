# origin-sidecar

> Always-on small-model worker with a bounded queue and pooled workers.

## Purpose

`origin-sidecar` is an always-on background worker pool backed by a *small,
fast* model (Haiku-class by default). It offloads cheap auxiliary work —
transcript summarization and content extraction — from the main agent loop so
the foreground model is never blocked on housekeeping. Jobs are submitted to a
bounded mpsc queue and drained by a fixed pool of tokio worker tasks; results
are delivered back through injected deliverer traits.

## Public API surface

| Item | Kind | Description |
|------|------|-------------|
| `Sidecar` | struct | The worker pool: owns the queue sender and join handles. |
| `Sidecar::spawn` | fn | Spawn `cfg.workers` tasks over a bounded channel. |
| `SidecarConfig` | struct | `workers`, `queue_capacity`, `model`. |
| `SidecarError` | enum | `QueueFull` / `Shutdown`. |
| `SidecarJob` | enum | `Summarize { .. }` or `Extract { .. }`. |
| `SummaryDeliverer` | trait | Async sink for a produced summary. |
| `ExtractDeliverer` | trait | Async sink for an extraction outline CAS handle. |

Module map: `extract`, `job`, `runtime`, `summarize`.

## Key types

```rust
pub struct SidecarConfig { pub workers: usize, pub queue_capacity: usize, pub model: String }
// Default: 2 workers, 256-deep queue, "claude-haiku-4-5-20251001".

pub enum SidecarJob {
    Summarize { session_id: String, turn_index: u32,
                transcript: Vec<origin_core::types::Message>,
                deliver_to: Box<dyn SummaryDeliverer> },
    Extract   { handle: origin_cas::Hash, deliver_to: Box<dyn ExtractDeliverer> },
}

impl Sidecar {
    pub fn spawn(provider: Arc<dyn Provider>, cas: Arc<Store>, cfg: SidecarConfig) -> Self;
}

#[async_trait]
pub trait SummaryDeliverer: Send + Sync + Debug {
    async fn deliver(&self, session_id: &str, turn_index: u32, summary: &str);
}
```

## How it works

```
submit(job) ──► bounded mpsc::Sender ──► [ worker 0 ]
   │  (QueueFull if full,                  [ worker 1 ]  ... N workers
   │   Shutdown if tx dropped)             each: recv().await → dispatch → deliver
```

`spawn` creates a `mpsc::channel` of `queue_capacity` and starts `workers`
tasks, each looping on a shared `Mutex<Receiver>`; when a job arrives it is
dispatched against the provider/CAS and the result handed to the job's
`deliver_to`. `workers == 0` is legal (queue stays open, nothing dispatched) —
useful for tests. The sender is held in a `Mutex<Option<..>>` so the shutdown
phase can drop it without consuming `Sidecar`; after that, `submit` returns
`SidecarError::Shutdown`. Back-pressure surfaces as `QueueFull` rather than
blocking the caller.

## Dependencies & features

- Runtime deps: `origin-core`, `origin-provider`, `origin-cas`, `tokio`
  (sync/time/rt/macros), `async-trait`, `thiserror`, `serde`/`serde_json`.
- Dev-deps: `mockall`, `tokio` (`test-util`/`time`) for paused-time tests.
  No Cargo features.

## Used by

`Grep "origin-sidecar" glob "crates/*/Cargo.toml"` →

- `crates/origin-daemon/Cargo.toml`
- `crates/origin-sidecar/Cargo.toml` (self)

The daemon constructs the `Sidecar` at startup and feeds it `Summarize`/`Extract`
jobs as turns complete; its `sidecar_summary`/`sidecar_extract` tests exercise
the wiring end-to-end.

## Testing

`tests/` directory: `runtime.rs` (queue capacity, shutdown semantics, worker
fan-out), `summarize.rs`, and `extract.rs`. Dev-dependency `mockall` stubs the
provider; `tokio` test-util drives paused time.

## See also

- [Agent & sessions subsystem](../subsystems/agent-and-sessions.md)
- [Providers subsystem](../subsystems/providers.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
