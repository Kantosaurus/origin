// SPDX-License-Identifier: Apache-2.0
//! B8: a sub-agent's `Task` result is a real return contract — `files_touched`
//! lists the files its successful edits changed, the report text is capped with
//! a notice, and a worker stops at a finite turn cap (default 50) with its
//! partial work reported as a normal result.
#![allow(clippy::panic)]

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use origin_cas::{Store as CasStore, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_plan::{Plan, PlanStore};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_swarm::{Coordinator, PlanHandle};
use origin_tools::builtins::task::{task_tool, TaskInput, TaskOutput};
use tempfile::TempDir;
use tokio::sync::{Mutex as AsyncMutex, RwLock};

type Script = Box<dyn Fn(u32) -> ChatResponse + Send + Sync>;

/// Answers the `n`th chat call (0-based) with `script(n)`, counting the calls.
struct ScriptedProvider {
    calls: AtomicU32,
    script: Script,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok((self.script)(self.calls.fetch_add(1, Ordering::SeqCst)))
    }
}

fn reply(blocks: Vec<Block>) -> ChatResponse {
    let mut m = Message::new(Role::Assistant);
    for b in blocks {
        m = m.with_block(b);
    }
    ChatResponse {
        assistant: m,
        usage: Usage::default(),
    }
}

fn call(name: &str, n: u32, input: &serde_json::Value) -> Block {
    Block::ToolUse {
        id: format!("tu_{n}"),
        name: name.into(),
        input_json: serde_json::to_vec(input).expect("json"),
        cache_marker: None,
    }
}

fn write(n: u32, path: &Path, content: &str) -> Block {
    call(
        "Write",
        n,
        &serde_json::json!({ "file_path": path.to_string_lossy(), "content": content }),
    )
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Run one `Task` through the daemon's real worker against `script`; returns
/// the parent-visible output and how many model calls the worker made.
async fn run_task(input: serde_json::Value, script: Script) -> (TaskOutput, u32) {
    let tmp = TempDir::new().expect("tmp");
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).expect("store"));
    let cas = Arc::new(
        CasStore::open(StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("cas"),
    );
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).expect("plan store"));
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);
    let provider = Arc::new(ScriptedProvider {
        calls: AtomicU32::new(0),
        script,
    });
    let active: Arc<RwLock<Arc<dyn Provider>>> =
        Arc::new(RwLock::new(Arc::clone(&provider) as Arc<dyn Provider>));
    let mut coord = Coordinator::new(plan, format!("origin-worker-b8-{}", std::process::id()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        origin_daemon::config::Governance::default(),
    ));
    let input: TaskInput = serde_json::from_value(input).expect("task input");
    let out = task_tool(&coord, input, &[]).await.expect("task result");
    (out, provider.calls.load(Ordering::SeqCst))
}

/// `n` spelled in letters: the stuck-loop detector ignores digits, so calls
/// that differ only by a number would read as the same action repeated.
fn letters(n: u32) -> String {
    let letter = |i: u32| char::from_u32(u32::from(b'a') + i).unwrap_or('z');
    format!("{}{}", letter(n / 26), letter(n % 26))
}

/// A worker that writes a new file every turn and never finishes on its own
/// (it gives up after 200 calls so a missing cap fails instead of hanging).
fn runaway(dir: &Path) -> Script {
    let dir = dir.to_path_buf();
    Box::new(move |n| {
        if n >= 200 {
            return reply(vec![Block::text("gave up")]);
        }
        let name = letters(n);
        reply(vec![
            Block::text(format!("step {name}")),
            write(n, &dir.join(format!("{name}.txt")), &name),
        ])
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn edited_files_are_listed_once_in_first_touch_order() {
    let dir = TempDir::new().expect("dir");
    let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
    let (a2, b2) = (a.clone(), b.clone());
    let (out, _) = run_task(
        serde_json::json!({ "goal": "edit files", "allowed_tools": ["Write"] }),
        Box::new(move |n| match n {
            0 => reply(vec![write(n, &a2, "1")]),
            1 => reply(vec![write(n, &b2, "1")]),
            2 => reply(vec![write(n, &a2, "2")]),
            _ => reply(vec![Block::text("done")]),
        }),
    )
    .await;
    assert_eq!(out.status, "completed", "{}", out.summary);
    assert_eq!(out.files_touched, vec![s(&a), s(&b)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn denied_and_failed_edits_are_not_touches() {
    let dir = TempDir::new().expect("dir");
    let (denied, missing) = (dir.path().join("denied.txt"), dir.path().join("missing.txt"));
    let (d2, m2) = (denied.clone(), missing.clone());
    let (out, _) = run_task(
        // `Write` is not granted, so it is denied; the `Edit` targets a file
        // that does not exist, so it fails.
        serde_json::json!({ "goal": "edit files", "allowed_tools": ["Edit"] }),
        Box::new(move |n| match n {
            0 => reply(vec![write(n, &d2, "x")]),
            1 => reply(vec![call(
                "Edit",
                n,
                &serde_json::json!({ "file_path": m2.to_string_lossy(), "old_string": "a", "new_string": "b" }),
            )]),
            _ => reply(vec![Block::text("done")]),
        }),
    )
    .await;
    assert!(!denied.exists() && !missing.exists());
    assert_eq!(out.status, "completed", "{}", out.summary);
    assert!(out.files_touched.is_empty(), "{:?}", out.files_touched);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_long_final_message_is_capped_with_one_notice() {
    let dir = TempDir::new().expect("dir");
    let a = dir.path().join("a.txt");
    let a2 = a.clone();
    let (out, _) = run_task(
        serde_json::json!({ "goal": "edit a file", "allowed_tools": ["Write"] }),
        Box::new(move |n| match n {
            0 => reply(vec![write(n, &a2, "1")]),
            _ => reply(vec![Block::text("x".repeat(20_000))]),
        }),
    )
    .await;
    assert_eq!(out.files_touched, vec![s(&a)]);
    assert_eq!(
        out.summary.matches("report truncated").count(),
        1,
        "{}",
        out.summary
    );
    assert!(out.summary.contains("12000 more characters"), "{}", out.summary);
    assert_eq!(out.summary.matches('x').count(), 8_000);
    // Compact for the parent: the whole JSON stays near the 8,000-char cap.
    let json = serde_json::to_string(&out).expect("json");
    assert!(json.len() < 8_400, "{} bytes", json.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runaway_worker_stops_at_the_default_cap_with_its_partial_work() {
    let dir = TempDir::new().expect("dir");
    let (out, calls) = run_task(
        serde_json::json!({ "goal": "loop forever", "allowed_tools": ["Write"] }),
        runaway(dir.path()),
    )
    .await;
    assert_eq!(calls, 50, "default turn cap");
    // A normal result, not an error: status, partial files and last text.
    assert_eq!(out.status, "budget_exhausted", "{}", out.summary);
    assert_eq!(out.files_touched.len(), 50);
    assert_eq!(out.files_touched[0], s(&dir.path().join("aa.txt")));
    assert!(out.summary.contains("turn cap (50 turns)"), "{}", out.summary);
    assert!(
        out.summary.contains(&format!("step {}", letters(49))),
        "{}",
        out.summary
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_explicit_turn_cap_overrides_the_default_both_ways() {
    for cap in [10_u32, 60] {
        let dir = TempDir::new().expect("dir");
        let (out, calls) = run_task(
            serde_json::json!({
                "goal": "loop forever",
                "allowed_tools": ["Write"],
                "budget": { "max_tool_calls": cap },
            }),
            runaway(dir.path()),
        )
        .await;
        assert_eq!(calls, cap);
        assert_eq!(out.status, "budget_exhausted", "{}", out.summary);
        assert_eq!(out.files_touched.len(), cap as usize);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn patch_deletes_and_moves_are_touches() {
    let dir = TempDir::new().expect("dir");
    let (gone, old, new) = (
        dir.path().join("gone.txt"),
        dir.path().join("old.txt"),
        dir.path().join("new.txt"),
    );
    std::fs::write(&gone, "bye\n").expect("seed");
    std::fs::write(&old, "a\n").expect("seed");
    let patch = format!(
        "*** Begin Patch\n*** Delete File: {}\n*** Update File: {}\n*** Move to: {}\n@@\n-a\n+b\n*** End Patch\n",
        s(&gone),
        s(&old),
        s(&new)
    );
    let (out, _) = run_task(
        serde_json::json!({ "goal": "reshuffle", "allowed_tools": ["ApplyPatch"] }),
        Box::new(move |n| match n {
            0 => reply(vec![call(
                "ApplyPatch",
                n,
                &serde_json::json!({ "patch": patch }),
            )]),
            _ => reply(vec![Block::text("done")]),
        }),
    )
    .await;
    assert!(!gone.exists() && !old.exists() && new.exists(), "{}", out.summary);
    assert_eq!(out.files_touched, vec![s(&gone), s(&new)], "{}", out.summary);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denial_stop_reports_aborted_with_the_work_done_so_far() {
    let dir = TempDir::new().expect("dir");
    let edited = dir.path().join("edited.txt");
    std::fs::write(&edited, "old\n").expect("seed");
    let (e2, d) = (edited.clone(), dir.path().to_path_buf());
    let (out, _) = run_task(
        // `Edit` is granted; every later `Write` is denied until the A8 cap.
        serde_json::json!({ "goal": "edit then write", "allowed_tools": ["Edit"] }),
        Box::new(move |n| match n {
            0 => reply(vec![call(
                "Edit",
                n,
                &serde_json::json!({ "file_path": e2.to_string_lossy(), "old_string": "old", "new_string": "new" }),
            )]),
            _ => reply(vec![
                Block::text("trying to write"),
                write(n, &d.join(format!("{}.txt", letters(n))), "x"),
            ]),
        }),
    )
    .await;
    assert_eq!(std::fs::read_to_string(&edited).expect("read"), "new\n");
    assert_eq!(out.status, "aborted", "{}", out.summary);
    assert_eq!(out.files_touched, vec![s(&edited)]);
    assert!(
        out.summary.contains("Stopped after repeated permission denials"),
        "{}",
        out.summary
    );
}
