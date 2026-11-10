// SPDX-License-Identifier: Apache-2.0
//! B3/B5: the system prompt and default effort a run gets, per profile
//! (interactive, headless `origin run`, swarm worker), measured on the request
//! a recording provider captures.

#![allow(clippy::panic, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use origin_cas::{Store as CasStore, StoreConfig};
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_plan::{Plan, PlanStore};
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_swarm::{Budget, Coordinator, PlanHandle, WorkerSpec};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// Records every request and answers with plain text, under a chosen name.
struct Recording {
    name: &'static str,
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for Recording {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

fn recording(name: &'static str) -> Arc<Recording> {
    Arc::new(Recording {
        name,
        requests: Mutex::new(Vec::new()),
    })
}

/// A small Rust workspace for the repo map to rank.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("a.rs"), "pub fn alpha() { crate::b::beta(); }\n").unwrap();
    std::fs::write(src.join("b.rs"), "pub fn beta() {}\n").unwrap();
    dir
}

async fn main_loop_system(opts: &LoopOptions) -> String {
    let dir = workspace();
    let provider = recording("recording");
    let mut session = Session::new("recording", "claude-test");
    session.roots = vec![dir.path().to_path_buf()];
    run_loop(&mut session, "hi", provider.as_ref(), &AlwaysAllow, opts)
        .await
        .unwrap();
    let system = provider.requests.lock().unwrap()[0].system.clone();
    system
}

/// Run one real swarm worker against `provider`; returns its captured requests.
async fn worker_requests(provider: Arc<Recording>) -> Vec<ChatRequest> {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).unwrap());
    let cas = Arc::new(
        CasStore::open(StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .unwrap(),
    );
    let plan_store = Arc::new(PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).unwrap());
    let plan = PlanHandle::new(Arc::new(AsyncMutex::new(Plan::default())), plan_store);
    let active: Arc<RwLock<Arc<dyn Provider>>> =
        Arc::new(RwLock::new(Arc::clone(&provider) as Arc<dyn Provider>));
    let mut coord = Coordinator::new(plan, format!("origin-prompt-profiles-{}", std::process::id()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        origin_daemon::config::Governance::default(),
    ));
    let spec = WorkerSpec {
        goal: "investigate the foo module".into(),
        allowed_tools: vec!["Read".into(), "Grep".into()],
        budget: Budget {
            max_wall_ms: 30_000,
            max_input_tokens: 10_000,
            max_output_tokens: 10_000,
            max_tool_calls: 4,
        },
        workspace: None,
        parent_actor: origin_plan::ActorId::new(0),
        model: None,
        mcp_servers: Vec::new(),
    };
    let handle = coord.spawn(spec).await.unwrap();
    coord.await_completion(&handle).await.unwrap();
    let reqs = provider.requests.lock().unwrap().clone();
    reqs
}

/// Run one prompt on a fresh session against `provider`; returns its first request.
async fn first_request(provider: &Arc<Recording>, model: &str, opts: &LoopOptions) -> ChatRequest {
    let mut session = Session::new(provider.name, model);
    run_loop(&mut session, "hi", provider.as_ref(), &AlwaysAllow, opts)
        .await
        .unwrap();
    let req = provider.requests.lock().unwrap()[0].clone();
    req
}

fn opts() -> LoopOptions {
    LoopOptions {
        max_turns: 2,
        ..LoopOptions::default().without_streaming()
    }
}

/// B3: with no explicit effort the main loop sends `high` to Anthropic (was `max`).
#[tokio::test]
async fn main_loop_defaults_to_high_effort_on_anthropic() {
    let req = first_request(&recording("anthropic"), "claude-test", &opts()).await;
    let body = origin_provider_anthropic::encode_request_for_test(&req);
    assert_eq!(
        body["output_config"]["effort"], "high",
        "{}",
        body["output_config"]
    );
}

/// B3: an explicit top effort is sent exactly as before (`max`).
#[tokio::test]
async fn explicit_effort_beats_the_default() {
    let opts = LoopOptions {
        effort: Some(origin_provider::ReasoningEffort::Ultracode),
        ..opts()
    };
    let req = first_request(&recording("anthropic"), "claude-test", &opts).await;
    let body = origin_provider_anthropic::encode_request_for_test(&req);
    assert_eq!(body["output_config"]["effort"], "max");
}

/// B3: a provider with no effort default today still sends no effort at all.
#[tokio::test]
async fn non_anthropic_request_is_unchanged() {
    let req = first_request(&recording("openai"), "gpt-5", &opts()).await;
    assert_eq!(req.effort, None);
    let body =
        serde_json::to_value(origin_provider_openai_compat::wire::encode_request(&req, false)).unwrap();
    assert!(body.get("reasoning_effort").is_none(), "{body}");
}

/// B3: a swarm worker on Anthropic sends `medium`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_defaults_to_medium_effort_on_anthropic() {
    let reqs = worker_requests(recording("anthropic")).await;
    let body = origin_provider_anthropic::encode_request_for_test(&reqs[0]);
    assert_eq!(
        body["output_config"]["effort"], "medium",
        "{}",
        body["output_config"]
    );
}

/// Prints the system-prompt sizes the report quotes (`--nocapture`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_system_prompt_bytes() {
    let interactive = main_loop_system(&opts()).await;
    let worker = worker_requests(recording("anthropic")).await;
    eprintln!(
        "MEASURE interactive_system_bytes={} worker_system_bytes={} worker_effort={:?}",
        interactive.len(),
        worker[0].system.len(),
        worker[0].effort
    );
}
