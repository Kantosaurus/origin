// SPDX-License-Identifier: Apache-2.0
//! Memory-governed admission control for swarm sub-agent workers.
//!
//! origin's default is to parallelise where safe: the [`crate::Coordinator`]
//! spawns as many sub-agent workers concurrently as memory allows, backing off
//! *before* the OS would OOM rather than capping at a fixed small number. The
//! [`AdmissionGate`] is the binding limiter; the `TaskClass::Swarm` execution
//! semaphore is only a coarse runaway backstop.
//!
//! ## Shape
//! - A [`MemoryProbe`] reports best-effort available/total RAM. It is injected
//!   (dependency inversion) so unit tests can script exact byte readings with
//!   zero real allocation, and so a container-aware (cgroup) probe can drop in
//!   later without touching the gate.
//! - Admission is `min(static ceiling, live governor)`: each in-flight worker
//!   debits a full per-worker *reserve* the instant it is admitted (committed,
//!   not realised), so a burst of concurrent admits cannot all read the same
//!   pre-allocation slack and overshoot.
//! - A `>= 1` forward-progress floor always admits the first worker, so the
//!   gate can never deadlock even under zero free memory.
//! - Backpressure is *await*, never reject: a parked admit holds nothing (no
//!   execution permit, no task), and resumes when an in-flight worker completes
//!   (RAII ticket drop → `notify_waiters`) or when a re-probe shows recovery.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

const MIB: u64 = 1024 * 1024;

/// Effective cap when no memory information and no hard cap are available
/// (non-Linux, probe failure). Reproduces origin's prior small fixed default.
pub const DEFAULT_FALLBACK_MAX: u32 = 3;

/// A best-effort snapshot of OS memory, in bytes.
///
/// `Unavailable` selects the degrade-safe path: the dynamic governor is skipped
/// and admission falls back to the static ceiling / hard cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemReading {
    /// Memory figure available, in bytes.
    Available(u64),
    /// The probe could not determine the figure (no panic, no error path).
    Unavailable,
}

/// Source of OS memory readings. Injected into [`AdmissionGate`] so the policy
/// is testable without real allocation and portable across probe backends.
pub trait MemoryProbe: Send + Sync + 'static {
    /// Best-effort available bytes *right now*. Called on the admission hot
    /// path, so it must be cheap. Any failure maps to [`MemReading::Unavailable`].
    fn available_bytes(&self) -> MemReading;

    /// Total usable RAM, read once at gate construction to resolve the headroom
    /// percentage and the static ceiling.
    fn total_bytes(&self) -> MemReading;
}

/// Linux probe reading `/proc/meminfo`. Whole-machine figures (cgroup-unaware);
/// a container-correct probe is a drop-in replacement behind [`MemoryProbe`].
#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcMeminfoProbe;

#[cfg(target_os = "linux")]
impl ProcMeminfoProbe {
    fn read(key: &str) -> Option<u64> {
        let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
        meminfo_field(&contents, key)
    }
}

#[cfg(target_os = "linux")]
impl MemoryProbe for ProcMeminfoProbe {
    fn available_bytes(&self) -> MemReading {
        let Ok(contents) = std::fs::read_to_string("/proc/meminfo") else {
            return MemReading::Unavailable;
        };
        // `MemAvailable` is the kernel's own estimate (>= 3.14). Fall back to a
        // coarse free+cached+buffers sum on ancient kernels that lack it.
        let kb = meminfo_field(&contents, "MemAvailable:").or_else(|| {
            let free = meminfo_field(&contents, "MemFree:")?;
            let cached = meminfo_field(&contents, "Cached:").unwrap_or(0);
            let buffers = meminfo_field(&contents, "Buffers:").unwrap_or(0);
            Some(free.saturating_add(cached).saturating_add(buffers))
        });
        kb.map_or(MemReading::Unavailable, |k| {
            MemReading::Available(k.saturating_mul(1024))
        })
    }

    fn total_bytes(&self) -> MemReading {
        Self::read("MemTotal:").map_or(MemReading::Unavailable, |k| {
            MemReading::Available(k.saturating_mul(1024))
        })
    }
}

/// Parse a `/proc/meminfo` line of the form `Key:   12345 kB`, returning the
/// numeric value in kB. Matches on the exact `key` (colon included).
#[cfg(target_os = "linux")]
fn meminfo_field(contents: &str, key: &str) -> Option<u64> {
    contents
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse::<u64>().ok())
}

/// Blind probe: always [`MemReading::Unavailable`]. The default on non-Linux
/// platforms, where admission degrades to the static / hard-cap bound.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProbe;

impl MemoryProbe for NullProbe {
    fn available_bytes(&self) -> MemReading {
        MemReading::Unavailable
    }
    fn total_bytes(&self) -> MemReading {
        MemReading::Unavailable
    }
}

/// Test probe feeding scripted readings — no real allocation required.
///
/// Constructed with a constant or a queue of `available` values (the last is
/// repeated once the queue drains) and a fixed `total`. Not `#[cfg(test)]` so
/// cross-crate integration tests can use it.
#[derive(Debug)]
pub struct ScriptedProbe {
    available: Mutex<VecDeque<u64>>,
    total: u64,
}

impl ScriptedProbe {
    /// A probe that always reports `available` bytes free out of `total`.
    #[must_use]
    pub fn constant(available: u64, total: u64) -> Self {
        let mut q = VecDeque::new();
        q.push_back(available);
        Self {
            available: Mutex::new(q),
            total,
        }
    }

    /// A probe that yields each reading in turn, repeating the last forever.
    #[must_use]
    pub fn sequence(readings: impl IntoIterator<Item = u64>, total: u64) -> Self {
        Self {
            available: Mutex::new(readings.into_iter().collect()),
            total,
        }
    }

    /// Overwrite the current reading (used by the external-recovery test to
    /// raise free memory while an admit is parked).
    pub fn set_available(&self, bytes: u64) {
        let mut q = self.available.lock().unwrap_or_else(PoisonError::into_inner);
        q.clear();
        q.push_back(bytes);
    }
}

impl MemoryProbe for ScriptedProbe {
    fn available_bytes(&self) -> MemReading {
        let mut q = self.available.lock().unwrap_or_else(PoisonError::into_inner);
        let v = if q.len() > 1 { q.pop_front() } else { q.front().copied() };
        v.map_or(MemReading::Unavailable, MemReading::Available)
    }
    fn total_bytes(&self) -> MemReading {
        MemReading::Available(self.total)
    }
}

/// Immutable admission policy. Build from the environment via [`GateCfg::from_env`]
/// or directly (fields are public) for tests.
#[derive(Debug, Clone, Copy)]
pub struct GateCfg {
    /// Estimated per-worker memory reserve, in bytes. Denominator of the static
    /// ceiling and debited per admission. Conservative over-estimate by design.
    pub reserve_bytes: u64,
    /// Absolute free-memory floor the gate refuses to dip below.
    pub headroom_bytes: u64,
    /// Optional hard cap on concurrent workers. `None` ⇒ memory governs.
    pub hard_max: Option<u32>,
    /// Execution-lane / runaway backstop ceiling (mirrors `TaskClass::Swarm`).
    pub lane_ceiling: u32,
    /// Coarse "how many could ever fit" bound = `clamp(total/reserve, 1, lane)`.
    pub static_ceiling: u32,
    /// Master switch. When `false` the live memory test is skipped entirely.
    pub governor: bool,
    /// Re-probe interval for parked admits (catches external memory recovery).
    pub poll: Duration,
}

impl GateCfg {
    /// Resolve the policy from `ORIGIN_SWARM_*` env vars given a `total` reading.
    #[must_use]
    pub fn from_env(total: MemReading) -> Self {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);

        let reserve_bytes = env_u64("ORIGIN_SWARM_RESERVE_MB")
            .filter(|m| *m > 0)
            .unwrap_or(512)
            .saturating_mul(MIB)
            .max(MIB);

        let lane_ceiling = env_u64("ORIGIN_SWARM_LANE_MAX")
            .filter(|n| *n > 0)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or_else(|| u32::try_from((cores * 8).max(64)).unwrap_or(64));

        // Clamped `.max(1)` so a `=0` misconfig can never wedge the >=1 floor.
        let hard_max = env_u64("ORIGIN_SWARM_MAX")
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX).max(1));

        let governor = env_flag("ORIGIN_SWARM_MEM_GOVERNOR", true);

        // Clamped `> 0` like the other knobs: a `0` poll would make a parked
        // admit busy-spin (re-probe every scheduler tick) instead of waiting.
        let poll = Duration::from_millis(env_u64("ORIGIN_SWARM_POLL_MS").filter(|n| *n > 0).unwrap_or(250));

        let headroom_bytes = resolve_headroom(total);

        let static_ceiling = match total {
            MemReading::Available(t) => u32::try_from((t / reserve_bytes).max(1))
                .unwrap_or(u32::MAX)
                .min(lane_ceiling),
            MemReading::Unavailable => hard_max.unwrap_or(DEFAULT_FALLBACK_MAX),
        };

        Self {
            reserve_bytes,
            headroom_bytes,
            hard_max,
            lane_ceiling,
            static_ceiling,
            governor,
            poll,
        }
    }
}

/// Effective headroom = `max(512 MiB floor, HEADROOM_MB?, PCT% of total)`, or a
/// 1 GiB blind default when total RAM is unknown and no absolute floor is set.
fn resolve_headroom(total: MemReading) -> u64 {
    let floor = 512 * MIB;
    let from_mb = env_u64("ORIGIN_SWARM_HEADROOM_MB").map(|m| m.saturating_mul(MIB));
    let pct = env_u64("ORIGIN_SWARM_HEADROOM_PCT").unwrap_or(10).min(100);
    let mut h = floor;
    if let Some(m) = from_mb {
        h = h.max(m);
    }
    match total {
        MemReading::Available(t) => h = h.max(t / 100 * pct),
        MemReading::Unavailable => {
            if from_mb.is_none() {
                h = h.max(1024 * MIB);
            }
        }
    }
    h
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|s| s.parse::<u64>().ok())
}

fn env_flag(key: &str, default: bool) -> bool {
    std::env::var(key).map_or(default, |v| {
        !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no")
    })
}

/// Mutable admission bookkeeping, guarded by the gate's mutex.
#[derive(Debug, Default)]
struct GateInner {
    /// Workers admitted but not yet completed (ticket still alive).
    admitted: u32,
    /// Sum of reserves debited for the currently admitted workers.
    outstanding_reserve: u64,
}

/// Pure admission decision. On admit, mutates `g` (increment + debit) and
/// returns `true`. Side-effect-free otherwise. Exposed to in-crate tests.
///
/// Admit the next worker iff:
/// `admitted < lane_ceiling AND admitted < hard_max(>=1) AND
///  (admitted == 0 OR Unavailable OR
///   (admitted < static_ceiling AND available - outstanding - reserve >= headroom))`.
fn decide(g: &mut GateInner, cfg: &GateCfg, reading: MemReading) -> bool {
    // (a) Forward-progress floor FIRST: with nothing in flight, ALWAYS admit —
    // before EVERY ceiling and the memory test — so the gate can never deadlock
    // for ANY `GateCfg`. A misconfigured ceiling (`lane_ceiling`/`hard_max`/
    // `static_ceiling` == 0) can therefore never wedge the first worker; it only
    // serializes subsequent ones. (The env path additionally clamps the knobs,
    // but the floor must hold for direct/test construction too.)
    if g.admitted == 0 {
        g.admitted = 1;
        g.outstanding_reserve = cfg.reserve_bytes;
        return true;
    }
    // (b) Hard ceilings — cheap, need no memory figure.
    if g.admitted >= cfg.lane_ceiling {
        return false;
    }
    if let Some(m) = cfg.hard_max {
        if g.admitted >= m {
            return false;
        }
    }
    if g.admitted >= cfg.static_ceiling {
        return false;
    }
    // (c) Live memory test (only when admitted > 0 and a reading is available).
    match reading {
        MemReading::Unavailable => {
            g.admitted += 1;
            g.outstanding_reserve = g.outstanding_reserve.saturating_add(cfg.reserve_bytes);
            true
        }
        MemReading::Available(avail) => {
            let projected = avail
                .saturating_sub(g.outstanding_reserve)
                .saturating_sub(cfg.reserve_bytes);
            if projected >= cfg.headroom_bytes {
                g.admitted += 1;
                g.outstanding_reserve = g.outstanding_reserve.saturating_add(cfg.reserve_bytes);
                true
            } else {
                false
            }
        }
    }
}

/// Process-shared default gate. RAM is a process-global resource, so all rooms
/// share one authoritative budget (a per-room gate would let N rooms each admit
/// to the headroom and collectively OOM).
static SHARED: OnceLock<Arc<AdmissionGate>> = OnceLock::new();

/// Memory-governed admission gate. Held as `Arc<AdmissionGate>` by each
/// [`crate::Coordinator`]; the default is process-shared via [`AdmissionGate::shared`].
pub struct AdmissionGate {
    inner: Mutex<GateInner>,
    notify: Notify,
    probe: Arc<dyn MemoryProbe>,
    cfg: GateCfg,
}

impl std::fmt::Debug for AdmissionGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionGate").field("cfg", &self.cfg).finish_non_exhaustive()
    }
}

impl AdmissionGate {
    /// The process-shared gate, built once from the platform probe + env config.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::clone(SHARED.get_or_init(|| Arc::new(Self::from_env(default_probe()))))
    }

    /// Build a gate from the environment with an explicit probe (not shared).
    #[must_use]
    pub fn from_env(probe: Arc<dyn MemoryProbe>) -> Self {
        let cfg = GateCfg::from_env(probe.total_bytes());
        Self::with_probe(probe, cfg)
    }

    /// Build a gate with an explicit probe and policy (test/injection entry).
    #[must_use]
    pub fn with_probe(probe: Arc<dyn MemoryProbe>, cfg: GateCfg) -> Self {
        Self {
            inner: Mutex::new(GateInner::default()),
            notify: Notify::new(),
            probe,
            cfg,
        }
    }

    /// A gate that admits without limit — for tests that isolate execution
    /// concurrency from the memory policy (the governor is disabled and the
    /// ceilings are effectively infinite).
    #[must_use]
    pub fn unlimited_for_test() -> Arc<Self> {
        Arc::new(Self::with_probe(
            Arc::new(NullProbe),
            GateCfg {
                reserve_bytes: MIB,
                headroom_bytes: 0,
                hard_max: None,
                lane_ceiling: u32::MAX,
                static_ceiling: u32::MAX,
                governor: false,
                poll: Duration::from_secs(3600),
            },
        ))
    }

    /// The resolved policy (diagnostics / tests).
    #[must_use]
    pub const fn cfg(&self) -> &GateCfg {
        &self.cfg
    }

    /// Number of workers currently admitted (diagnostics / tests).
    #[must_use]
    pub fn in_flight(&self) -> u32 {
        self.lock().admitted
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Acquire admission for one worker, parking (holding nothing) until the
    /// admission inequality passes or the forward-progress floor fires.
    ///
    /// The returned [`AdmissionTicket`] must be kept alive for the worker's
    /// lifetime; dropping it releases the reserve and wakes parked admits.
    pub async fn admit(self: &Arc<Self>) -> AdmissionTicket {
        loop {
            // Arm the wakeup BEFORE reading state so a completion firing between
            // our check and our await is not lost (the poll arm is a backstop).
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let reading = if self.cfg.governor {
                self.probe.available_bytes()
            } else {
                MemReading::Unavailable
            };
            let admitted = {
                let mut g = self.lock();
                decide(&mut g, &self.cfg, reading)
            };
            if admitted {
                return AdmissionTicket {
                    gate: Arc::clone(self),
                    reserve: self.cfg.reserve_bytes,
                };
            }
            // Park: resume on a completion notification or the re-probe tick.
            tokio::select! {
                () = notified.as_mut() => {}
                () = tokio::time::sleep(self.cfg.poll) => {}
            }
        }
    }
}

/// Build the default [`MemoryProbe`] for this platform.
#[must_use]
pub fn default_probe() -> Arc<dyn MemoryProbe> {
    #[cfg(target_os = "linux")]
    {
        Arc::new(ProcMeminfoProbe)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Arc::new(NullProbe)
    }
}

/// RAII admission grant. Held inside the worker task; its `Drop` returns the
/// reserve and wakes parked admits on every exit path (return, panic, cancel).
#[derive(Debug)]
pub struct AdmissionTicket {
    gate: Arc<AdmissionGate>,
    reserve: u64,
}

impl Drop for AdmissionTicket {
    fn drop(&mut self) {
        {
            let mut g = self.gate.lock();
            g.admitted = g.admitted.saturating_sub(1);
            g.outstanding_reserve = g.outstanding_reserve.saturating_sub(self.reserve);
        }
        // Wake ALL parked admits to re-test; each re-runs `decide` under the
        // lock, so exactly those that now fit proceed and the rest re-park.
        self.gate.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(reserve: u64, headroom: u64, static_ceiling: u32) -> GateCfg {
        GateCfg {
            reserve_bytes: reserve,
            headroom_bytes: headroom,
            hard_max: None,
            lane_ceiling: 1024,
            static_ceiling,
            governor: true,
            poll: Duration::from_millis(0),
        }
    }

    // RED 1 — the pure admission inequality at its boundaries.
    #[test]
    fn decide_inequality_boundary() {
        let c = cfg(MIB, MIB, 100);
        // admitted == 0 always admits, regardless of memory.
        let mut g = GateInner::default();
        assert!(decide(&mut g, &c, MemReading::Available(0)));
        assert_eq!(g.admitted, 1);
        assert_eq!(g.outstanding_reserve, MIB);

        // Exactly at the floor: avail - outstanding - reserve == headroom ⇒ admit.
        let mut g = GateInner {
            admitted: 1,
            outstanding_reserve: MIB,
        };
        // need avail - MIB - MIB >= MIB  ⇒ avail == 3*MIB is the boundary.
        assert!(decide(&mut g, &c, MemReading::Available(3 * MIB)));
        // One byte under the boundary ⇒ refuse.
        let mut g = GateInner {
            admitted: 1,
            outstanding_reserve: MIB,
        };
        assert!(!decide(&mut g, &c, MemReading::Available(3 * MIB - 1)));

        // static_ceiling binds even with infinite memory.
        let c2 = cfg(MIB, MIB, 2);
        let mut g = GateInner {
            admitted: 2,
            outstanding_reserve: 2 * MIB,
        };
        assert!(!decide(&mut g, &c2, MemReading::Available(u64::MAX)));

        // Unavailable ⇒ admit up to static_ceiling without a memory test.
        let mut g = GateInner {
            admitted: 1,
            outstanding_reserve: MIB,
        };
        assert!(decide(&mut g, &c2, MemReading::Unavailable));
    }

    #[test]
    fn hard_max_clamped_to_one() {
        let mut c = cfg(MIB, MIB, 100);
        c.hard_max = Some(1); // from_env clamps a `=0` misconfig up to 1
        let mut g = GateInner::default();
        assert!(decide(&mut g, &c, MemReading::Available(u64::MAX))); // floor
        // admitted now 1; hard_max 1 ⇒ next refused.
        assert!(!decide(&mut g, &c, MemReading::Available(u64::MAX)));
    }
}
