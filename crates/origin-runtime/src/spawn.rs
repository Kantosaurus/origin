//! `spawn_in(class, fut)` — the only sanctioned way to spawn an async task in
//! `origin-daemon`. Every call acquires a per-class permit before polling the
//! inner future.

use crate::bulk_gate::BulkGate;
use crate::class::TaskClass;
use crate::registry::{note_critical_acquire, note_critical_release, registry};
use std::future::Future;
use tokio::task::JoinHandle;

/// Spawn `fut` onto the current Tokio runtime under the given class.
///
/// # Panics
/// Panics if called outside a Tokio runtime context.
#[allow(clippy::module_name_repetitions)]
pub fn spawn_in<F>(class: TaskClass, fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let reg = registry();
    let sema = std::sync::Arc::clone(&reg.sema[class as usize]);
    tokio::spawn(async move {
        let _permit = sema.acquire_owned().await.expect("semaphore closed");
        if matches!(class, TaskClass::Critical) {
            note_critical_acquire();
        }
        if matches!(class, TaskClass::Bulk) {
            // Park while any Critical task holds a permit — centralised in BulkGate.
            BulkGate::current().wait_until_idle().await;
        }
        let out = fut.await;
        if matches!(class, TaskClass::Critical) {
            note_critical_release();
        }
        out
    })
}
