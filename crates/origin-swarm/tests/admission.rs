// SPDX-License-Identifier: Apache-2.0
//! Memory-governed admission: deterministic proofs with a scripted probe, no
//! real allocation. Covers the >=1 forward-progress floor, memory tracking,
//! resume-on-completion and resume-on-recovery, no-double-admit under
//! concurrency, the hard cap, degrade-safe behavior when the probe is blind,
//! RAII release on cancel/panic, and that the Coordinator actually gates on it.
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use origin_cas::{Store as CasStore, StoreConfig};
use origin_plan::{ActorId, Plan, PlanStore};
use origin_swarm::{
    AdmissionGate, AdmissionTicket, Budget, CompletionReport, Coordinator, GateCfg, NullProbe, PlanHandle,
    ReportStatus, ScriptedProbe, Usage, WorkerContext, WorkerFn, WorkerSpec,
};
use tempfile::TempDir;
use tokio::sync::Mutex as TokioMutex;

const GIB: u64 = 1 << 30;
const NEVER: Duration = Duration::from_secs(3600);

const fn cfg(reserve: u64, headroom: u64, static_ceiling: u32, hard_max: Option<u32>, poll: Duration) -> GateCfg {
    GateCfg {
        reserve_bytes: reserve,
        headroom_bytes: headroom,
        hard_max,
        lane_ceiling: 4096,
        static_ceiling,
        governor: true,
        poll,
    }
}

fn gate(probe: ScriptedProbe, cfg: GateCfg) -> Arc<AdmissionGate> {
    Arc::new(AdmissionGate::with_probe(Arc::new(probe), cfg))
}

/// Admit, holding every ticket, until the gate parks (the next admit can't be
/// granted within `budget`). Returns the held tickets — their count is the
/// concurrency the gate allowed.
async fn collect_until_park(g: &Arc<AdmissionGate>, budget: Duration) -> Vec<AdmissionTicket> {
    let mut held = Vec::new();
    while let Ok(t) = tokio::time::timeout(budget, g.admit()).await {
        held.push(t);
    }
    held
}

// RED 2 — the >=1 forward-progress floor admits even at zero free memory.
#[tokio::test]
async fn always_admits_one_under_pressure() {
    let g = gate(ScriptedProbe::constant(0, 16 * GIB), cfg(GIB, GIB, 100, None, NEVER));
    let ticket = tokio::time::timeout(Duration::from_millis(200), g.admit()).await;
    assert!(ticket.is_ok(), "first admit must never block, even with 0 free memory");
    assert_eq!(g.in_flight(), 1);
}

// A pathological `lane_ceiling`/`static_ceiling` of 0 must NOT wedge the >=1
// forward-progress floor: the first worker still admits, subsequent ones
// serialize. (Regression for the floor-ordering audit finding.)
#[tokio::test]
async fn zero_ceilings_serialize_never_deadlock() {
    let g = Arc::new(AdmissionGate::with_probe(
        Arc::new(ScriptedProbe::constant(64 * GIB, 64 * GIB)),
        GateCfg {
            reserve_bytes: GIB,
            headroom_bytes: 0,
            hard_max: None,
            lane_ceiling: 0,
            static_ceiling: 0,
            governor: true,
            poll: NEVER,
        },
    ));
    let held = collect_until_park(&g, Duration::from_millis(100)).await;
    assert_eq!(held.len(), 1, "the floor must admit the first worker even when every ceiling is 0");
}

// RED 3 — admitted concurrency tracks live free memory: 8 GiB free, 1 GiB
// reserve, 1 GiB headroom ⇒ 7 fit (keep 1 GiB back), the 8th parks.
#[tokio::test]
async fn admit_tracks_scripted_memory() {
    let g = gate(ScriptedProbe::constant(8 * GIB, 16 * GIB), cfg(GIB, GIB, 100, None, NEVER));
    let held = collect_until_park(&g, Duration::from_millis(100)).await;
    assert_eq!(held.len(), 7, "8 GiB free − 1 GiB headroom, 1 GiB each ⇒ 7 concurrent");
}

// RED 4 — when exactly one fits, the second parks, then RESUMES when the first
// completes (ticket drop → notify), with the poll arm effectively disabled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_admit_blocks_then_resumes() {
    // 2 GiB free, 1.5 GiB reserve, 1 GiB headroom ⇒ only the floor's 1 fits.
    let g = gate(ScriptedProbe::constant(2 * GIB, 8 * GIB), cfg(3 * GIB / 2, GIB, 100, None, NEVER));
    let first = g.admit().await;
    assert_eq!(g.in_flight(), 1);

    let g2 = Arc::clone(&g);
    let second = tokio::spawn(async move { g2.admit().await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!second.is_finished(), "second admit must park while the first holds memory");

    drop(first); // releases the reserve + notifies
    let resumed = tokio::time::timeout(Duration::from_secs(2), second).await;
    assert!(resumed.is_ok(), "second admit must resume once the first completes");
}

// RED 5 — a parked admit resumes when an EXTERNAL process frees memory, via the
// poll re-probe, even though no swarm worker completed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_on_recovery_without_completion() {
    let probe = Arc::new(ScriptedProbe::constant(2 * GIB, 32 * GIB));
    let g = Arc::new(AdmissionGate::with_probe(
        Arc::clone(&probe) as Arc<_>,
        cfg(3 * GIB / 2, GIB, 100, None, Duration::from_millis(20)),
    ));
    let _first = g.admit().await; // held, NOT dropped
    assert_eq!(g.in_flight(), 1);

    let g2 = Arc::clone(&g);
    let second = tokio::spawn(async move { g2.admit().await });
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!second.is_finished(), "second admit parks while memory is tight");

    probe.set_available(16 * GIB); // external recovery, no completion
    let resumed = tokio::time::timeout(Duration::from_secs(2), second).await;
    assert!(resumed.is_ok(), "parked admit must resume after a re-probe sees recovery");
}

// RED 6 — under 50 concurrent admits, the committed-reserve accounting + atomic
// check-increment admit EXACTLY the number that fit; no double-admit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_double_admit_under_concurrency() {
    // 4 GiB free, 1 GiB reserve, 1 GiB headroom ⇒ floor 1 + 2 more = 3.
    let g = gate(ScriptedProbe::constant(4 * GIB, 16 * GIB), cfg(GIB, GIB, 100, None, NEVER));
    let held: Arc<Mutex<Vec<AdmissionTicket>>> = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for _ in 0..50u32 {
        let g = Arc::clone(&g);
        let held = Arc::clone(&held);
        tasks.push(tokio::spawn(async move {
            if let Ok(t) = tokio::time::timeout(Duration::from_millis(200), g.admit()).await {
                held.lock().unwrap().push(t); // hold it (don't release)
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
    assert_eq!(held.lock().unwrap().len(), 3, "exactly 3 fit; the other 47 must park, never double-admit");
}

// RED 7 — a hard cap overrides abundant memory.
#[tokio::test]
async fn hard_max_overrides_memory() {
    let g = gate(ScriptedProbe::constant(64 * GIB, 64 * GIB), cfg(GIB, GIB, 100, Some(3), NEVER));
    let held = collect_until_park(&g, Duration::from_millis(100)).await;
    assert_eq!(held.len(), 3, "ORIGIN_SWARM_MAX=3 caps concurrency regardless of free RAM");
}

// RED 8 — a blind probe degrades to a bounded default, never unbounded, no panic.
#[tokio::test]
async fn probe_unavailable_degrades_to_bounded() {
    // NullProbe ⇒ Unavailable; static_ceiling stands in for the fallback bound.
    let g = Arc::new(AdmissionGate::with_probe(
        Arc::new(NullProbe),
        cfg(GIB, GIB, 3, None, NEVER),
    ));
    let held = collect_until_park(&g, Duration::from_millis(100)).await;
    assert_eq!(held.len(), 3, "blind probe must bound concurrency to the static ceiling");
}

// RED 9 — a misconfigured hard cap of 1 serializes but never wedges the floor.
#[tokio::test]
async fn hard_max_one_serializes() {
    let g = gate(ScriptedProbe::constant(64 * GIB, 64 * GIB), cfg(GIB, GIB, 100, Some(1), NEVER));
    let held = collect_until_park(&g, Duration::from_millis(100)).await;
    assert_eq!(held.len(), 1, "hard_max=1 ⇒ serial, but the first always proceeds");
}

// RED 10a — the RAII ticket releases its reserve when the holding task is cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_release_on_cancel() {
    let g = gate(ScriptedProbe::constant(64 * GIB, 64 * GIB), cfg(GIB, GIB, 100, None, NEVER));
    let ticket = g.admit().await;
    assert_eq!(g.in_flight(), 1);
    let task = tokio::spawn(async move {
        let _t = ticket; // move ticket in
        std::future::pending::<()>().await; // park forever until aborted
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let _ = task.await; // cancelled → ticket dropped on unwind
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(g.in_flight(), 0, "cancelled holder must release its admission slot");
}

// RED 10b — the RAII ticket releases its reserve when the holding task panics.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_release_on_panic() {
    let g = gate(ScriptedProbe::constant(64 * GIB, 64 * GIB), cfg(GIB, GIB, 100, None, NEVER));
    let ticket = g.admit().await;
    assert_eq!(g.in_flight(), 1);
    let task = tokio::spawn(async move {
        let _t = ticket;
        panic!("boom");
    });
    let _ = task.await; // JoinError(panic) → Drop ran during unwind
    assert_eq!(g.in_flight(), 0, "panicking holder must release its admission slot");
}

// RED 11 — two sequential admits on one task never self-deadlock (decide is a
// pure sync fn; the lock is dropped before the park await).
#[tokio::test]
async fn two_sequential_admits_no_self_deadlock() {
    let g = gate(ScriptedProbe::constant(64 * GIB, 64 * GIB), cfg(GIB, GIB, 100, None, NEVER));
    let ok = tokio::time::timeout(Duration::from_secs(1), async {
        let _a = g.admit().await;
        let _b = g.admit().await;
    })
    .await;
    assert!(ok.is_ok(), "sequential admits must not deadlock the gate");
}

// ---- Coordinator integration (RED 12): the gate actually limits the swarm ----

fn open_cas(root: std::path::PathBuf) -> CasStore {
    CasStore::open(StoreConfig {
        root,
        hot_capacity: 64,
        warm_pack_target_bytes: 1 << 20,
        cold_zstd_level: 3,
    })
    .unwrap()
}

fn plan_handle(tmp: &TempDir) -> PlanHandle {
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).unwrap());
    let cas = Arc::new(open_cas(tmp.path().join("cas")));
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).unwrap());
    PlanHandle::new(Arc::new(TokioMutex::new(Plan::default())), plan_store)
}

fn spec(goal: &str) -> WorkerSpec {
    WorkerSpec {
        goal: goal.into(),
        allowed_tools: vec![],
        budget: Budget {
            max_wall_ms: 5_000,
            max_input_tokens: 100,
            max_output_tokens: 100,
            max_tool_calls: 10,
        },
        workspace: None,
        parent_actor: ActorId::new(0),
        model: None,
    }
}

/// A worker that records the peak number of simultaneously-running workers.
fn peak_worker(current: Arc<AtomicUsize>, max_seen: Arc<AtomicUsize>) -> WorkerFn {
    Arc::new(move |ctx: WorkerContext| {
        let (cur, mx) = (Arc::clone(&current), Arc::clone(&max_seen));
        Box::pin(async move {
            let now = cur.fetch_add(1, Ordering::SeqCst) + 1;
            mx.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            cur.fetch_sub(1, Ordering::SeqCst);
            Ok(CompletionReport {
                goal: ctx.spec.goal.clone(),
                status: ReportStatus::Completed,
                plan_updates: Vec::new(),
                files_touched: Vec::new(),
                decisions: Vec::new(),
                follow_ups: Vec::new(),
                transcript_handle: [0; 32],
                usage: Usage::default(),
            })
        })
    })
}

// A tight gate (hard_max=1) forces the Coordinator's workers to SERIALIZE,
// proving the spawn path actually awaits admission before launching.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coordinator_serializes_under_tight_gate() {
    let tmp = TempDir::new().unwrap();
    let tight = gate(ScriptedProbe::constant(GIB, 2 * GIB), cfg(GIB, GIB, 100, Some(1), NEVER));
    let coord = Coordinator::new(plan_handle(&tmp), "tight").with_memory_gate(tight);

    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let worker = peak_worker(Arc::clone(&current), Arc::clone(&max_seen));

    let h1 = coord.spawn_with(spec("a"), Arc::clone(&worker)).await.unwrap();
    let h2 = coord.spawn_with(spec("b"), Arc::clone(&worker)).await.unwrap();
    let h3 = coord.spawn_with(spec("c"), Arc::clone(&worker)).await.unwrap();
    coord.await_completion(&h1).await.unwrap();
    coord.await_completion(&h2).await.unwrap();
    coord.await_completion(&h3).await.unwrap();

    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        1,
        "hard_max=1 gate must serialize the swarm — only one worker runs at a time"
    );
}

// A generous gate lets the same three workers overlap — proving the gate, not a
// fixed cap, is what bounds concurrency.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn coordinator_parallelizes_under_generous_gate() {
    let tmp = TempDir::new().unwrap();
    let generous = gate(ScriptedProbe::constant(64 * GIB, 64 * GIB), cfg(GIB, 0, 100, None, NEVER));
    let coord = Coordinator::new(plan_handle(&tmp), "generous").with_memory_gate(generous);

    let current = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));
    let worker = peak_worker(Arc::clone(&current), Arc::clone(&max_seen));

    let h1 = coord.spawn_with(spec("a"), Arc::clone(&worker)).await.unwrap();
    let h2 = coord.spawn_with(spec("b"), Arc::clone(&worker)).await.unwrap();
    let h3 = coord.spawn_with(spec("c"), Arc::clone(&worker)).await.unwrap();
    coord.await_completion(&h1).await.unwrap();
    coord.await_completion(&h2).await.unwrap();
    coord.await_completion(&h3).await.unwrap();

    assert_eq!(
        max_seen.load(Ordering::SeqCst),
        3,
        "a generous memory gate must let all three workers run concurrently"
    );
}
