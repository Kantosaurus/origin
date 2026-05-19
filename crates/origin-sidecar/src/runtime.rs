//! Sidecar runtime: bounded mpsc queue + N worker tasks (N2.5).

use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use origin_cas::Store;
use origin_provider::Provider;

use crate::job::SidecarJob;

#[allow(
    clippy::module_name_repetitions,
    reason = "SidecarError is the canonical public name for this crate's error type"
)]
#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("queue full")]
    QueueFull,
    #[error("shutdown")]
    Shutdown,
}

#[allow(
    clippy::module_name_repetitions,
    reason = "SidecarConfig is the canonical public name for this crate's config type"
)]
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    pub workers: usize,
    pub queue_capacity: usize,
    pub model: String,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            workers: 2,
            queue_capacity: 256,
            model: "claude-haiku-4-5-20251001".to_string(),
        }
    }
}

pub struct Sidecar {
    tx: mpsc::Sender<SidecarJob>,
    /// Kept alive so the channel stays open even when `workers == 0`.
    _rx: Arc<Mutex<mpsc::Receiver<SidecarJob>>>,
    workers: Vec<JoinHandle<()>>,
}

impl Sidecar {
    /// Spawn `cfg.workers` worker tasks. Returns the handle.
    ///
    /// `cfg.workers == 0` is legal — useful for tests (queue stays open, no
    /// jobs are dispatched).
    ///
    /// Accepts owned `Arc` values because callers typically pass freshly-created
    /// arcs; worker tasks clone them internally.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers pass freshly-constructed Arc/config; by-value is the idiomatic API here"
    )]
    pub fn spawn(provider: Arc<dyn Provider>, cas: Arc<Store>, cfg: SidecarConfig) -> Self {
        let (tx, rx) = mpsc::channel::<SidecarJob>(cfg.queue_capacity.max(1));
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(cfg.workers);
        for _ in 0..cfg.workers {
            let rx = rx.clone();
            let provider = provider.clone();
            let cas = cas.clone();
            let model = cfg.model.clone();
            workers.push(tokio::spawn(async move {
                loop {
                    let job = {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    };
                    let Some(job) = job else { break };
                    dispatch_stub(&provider, &cas, &model, job).await;
                }
            }));
        }
        Self { tx, _rx: rx, workers }
    }

    /// Submit a job to the queue.
    ///
    /// # Errors
    /// Returns `QueueFull` if the bounded mpsc has no slot. `Shutdown` if the
    /// receiver half has been dropped.
    pub fn submit(&self, job: SidecarJob) -> Result<(), SidecarError> {
        self.tx.try_send(job).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => SidecarError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => SidecarError::Shutdown,
        })
    }

    /// Shut down all worker tasks gracefully.
    pub async fn shutdown(self) {
        drop(self.tx);
        for h in self.workers {
            let _ = h.await;
        }
    }
}

async fn dispatch_stub(provider: &Arc<dyn Provider>, _cas: &Arc<Store>, model: &str, job: SidecarJob) {
    // P5.2: Summarize arm calls `summarize::run`; P5.3 replaces the Extract arm.
    match job {
        SidecarJob::Summarize {
            session_id,
            turn_index,
            transcript,
            deliver_to,
        } => {
            crate::summarize::run(
                provider,
                model,
                &session_id,
                turn_index,
                &transcript,
                deliver_to.as_ref(),
            )
            .await;
        }
        SidecarJob::Extract { handle, deliver_to } => {
            deliver_to.deliver(handle, handle).await;
        }
    }
}
