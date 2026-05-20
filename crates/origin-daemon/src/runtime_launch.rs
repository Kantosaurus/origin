//! Two-runtime launcher — control core + worker pool.
//!
//! The control core runs on its own named OS thread (`origin-ctrl`) and
//! hosts a `current_thread` Tokio runtime. The worker pool runs a
//! `multi_thread` runtime with `physical_cores - 1` workers, thread name
//! `origin-work`.
//!
//! The control core runs the IPC accept loop, renderer ticks, and event
//! dispatch. The worker pool runs everything else — provider HTTP/2,
//! agent turns, tool execution, relays, background tasks. The split
//! keeps the latency-critical control path isolated from CPU-heavy work.
//!
//! P12.8 lands the split itself. P12.9 migrates every `tokio::spawn`
//! call site in the daemon to `origin_runtime::spawn_in(class, …)` so the
//! per-class semaphores enforce the budget contract.

use std::future::Future;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use tokio::runtime::{Builder, Handle, Runtime};

/// Shared shutdown flag plus the lazily-populated runtime handles.
///
/// Production wires SIGTERM/SIGINT to `trigger()` via `ctrlc::set_handler`
/// (see `main.rs`). Cloning is cheap — every field is `Arc`-shared.
#[derive(Clone)]
pub struct ShutdownSignal {
    inner: Arc<(Mutex<bool>, Condvar)>,
    control: ControlHandle,
    worker: WorkerHandle,
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownSignal {
    /// Construct a fresh signal. The `ControlHandle` and `WorkerHandle`
    /// start out empty — `start()` populates them when the runtimes spin up.
    #[must_use]
    pub fn new() -> Self {
        let inner = Arc::new((Mutex::new(false), Condvar::new()));
        Self {
            inner,
            control: ControlHandle::pending(),
            worker: WorkerHandle::pending(),
        }
    }

    /// Signal shutdown. The control runtime's parking task wakes up and
    /// the launcher returns. Safe to call from any thread or signal handler.
    ///
    /// # Panics
    /// Does not panic in practice — a poisoned shutdown mutex is treated as
    /// a successful trigger (the lock is only held to flip the boolean).
    pub fn trigger(&self) {
        let (lock, cvar) = &*self.inner;
        if let Ok(mut g) = lock.lock() {
            *g = true;
            cvar.notify_all();
        }
    }

    /// Block until `trigger()` is called. Used by the control runtime's
    /// parking task.
    ///
    /// # Panics
    /// Panics if the shutdown mutex is poisoned (a panic happened while
    /// another thread held the lock); in that case the daemon is already
    /// in a broken state and aborting is the safest option.
    // The guard `g` must live across the `cvar.wait(g)` re-acquire calls —
    // clippy's drop-tightening lint can't see that `cvar.wait` consumes and
    // returns the same guard, so we silence it here.
    #[allow(clippy::significant_drop_tightening)]
    pub fn wait(&self) {
        let (lock, cvar) = &*self.inner;
        let mut g = lock.lock().expect("shutdown lock poisoned");
        while !*g {
            g = cvar.wait(g).expect("shutdown wait poisoned");
        }
    }

    #[must_use]
    pub const fn control_handle(&self) -> &ControlHandle {
        &self.control
    }

    #[must_use]
    pub const fn worker_handle(&self) -> &WorkerHandle {
        &self.worker
    }
}

/// Handle to the control-core runtime. Use `spawn_on_control` to schedule
/// IPC accept loops / renderer ticks / event dispatch on the named
/// `origin-ctrl` OS thread.
#[derive(Clone)]
pub struct ControlHandle {
    handle: Arc<Mutex<Option<Handle>>>,
}

impl ControlHandle {
    fn pending() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
        }
    }

    /// # Panics
    /// Panics if the inner mutex is poisoned.
    fn set(&self, h: Handle) {
        *self.handle.lock().expect("ctrl handle lock") = Some(h);
    }

    /// Spawn a future on the control runtime. No-op if the runtime is not
    /// yet up — the caller will retry after `start()` initialises the handle.
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned.
    pub fn spawn_on_control<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if let Some(h) = self.handle.lock().expect("ctrl lock").as_ref() {
            h.spawn(fut);
        }
    }

    /// Expose the inner `Handle` for callers that need to drive an arbitrary
    /// future (e.g. `block_on` from a worker context).
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned.
    #[must_use]
    pub fn raw(&self) -> Option<Handle> {
        self.handle.lock().expect("ctrl lock").clone()
    }
}

/// Handle to the worker-pool runtime. Use `spawn_on_worker` to schedule
/// blocking startup closures, or `raw()` to dispatch async work via
/// `Handle::spawn` / `Handle::block_on`.
#[derive(Clone)]
pub struct WorkerHandle {
    handle: Arc<Mutex<Option<Handle>>>,
}

impl WorkerHandle {
    fn pending() -> Self {
        Self {
            handle: Arc::new(Mutex::new(None)),
        }
    }

    /// # Panics
    /// Panics if the inner mutex is poisoned.
    fn set(&self, h: Handle) {
        *self.handle.lock().expect("worker handle lock") = Some(h);
    }

    /// Run a blocking closure on the worker runtime via `spawn_blocking`.
    /// The closure runs on a tokio worker thread; the closure body can
    /// itself enter a runtime context via `Handle::current()`.
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned.
    pub fn spawn_on_worker<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if let Some(h) = self.handle.lock().expect("worker lock").as_ref() {
            h.spawn_blocking(f);
        }
    }

    /// Expose the inner `Handle`. Returns `None` if `start()` has not yet
    /// populated it.
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned.
    #[must_use]
    pub fn raw(&self) -> Option<Handle> {
        self.handle.lock().expect("worker lock").clone()
    }
}

/// Start both runtimes and block until `signal.trigger()` is called.
///
/// Order matters: we build the worker pool first so the control core
/// can dispatch to it the moment it comes up. Then we spawn a dedicated
/// OS thread named `origin-ctrl` and host the `current_thread` runtime
/// there — that's how the test observes a stable, named control thread.
///
/// On shutdown we drop the worker runtime explicitly; Tokio's `Drop`
/// impl waits for all worker tasks to settle.
///
/// # Panics
/// Panics if either runtime fails to build, or the control OS thread
/// cannot be spawned (both are fatal startup failures).
pub fn start(signal: ShutdownSignal) {
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4);
    let worker_threads = cores.saturating_sub(1).max(1);

    // Worker pool first — control core may dispatch to it on startup.
    let worker_rt: Runtime = Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("origin-work")
        .enable_all()
        .build()
        .expect("worker runtime");
    signal.worker.set(worker_rt.handle().clone());

    // Control core on its own named OS thread.
    let signal_ctrl = signal;
    let ctrl_join = thread::Builder::new()
        .name("origin-ctrl".to_string())
        .spawn(move || {
            let ctrl_rt: Runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("ctrl runtime");
            signal_ctrl.control.set(ctrl_rt.handle().clone());
            ctrl_rt.block_on(async move {
                // Park until shutdown is requested. We use spawn_blocking so
                // the parking Condvar wait doesn't starve the single-thread
                // runtime — the wait happens on a blocking thread, and the
                // current-thread runtime can still service other futures
                // spawned via `spawn_on_control`.
                let s = signal_ctrl.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || s.wait()).await {
                    tracing::error!(error = %e, "control park task join error");
                }
            });
        })
        .expect("ctrl thread spawn");

    // Main thread waits on the control thread.
    let _ = ctrl_join.join();
    // Drop the worker runtime — Tokio's Drop waits for tasks to settle.
    drop(worker_rt);
}
