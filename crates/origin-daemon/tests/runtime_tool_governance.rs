// SPDX-License-Identifier: Apache-2.0
//! A4: a sub-agent's inline-MCP ("runtime") tool passes the same post-decision
//! checks and output handling as a registry tool: the governance overlay, the
//! read-only overlay (MCP tools count as mutating), the uniform denial result,
//! secret redaction and a bounded error body. The `PreTool` hook is covered in
//! `runtime_tool_pretool_hook.rs` (it needs its own process).
#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, scope_runtime_tools, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use origin_tools::{DynTool, SandboxProfile, SideEffects, Tier, ToolMeta, Urgency};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SECRET: &str = "sk-ant-a4-runtime-tool-0123456789abcdef";

/// Replays `script`; records every request.
struct Scripted {
    script: Mutex<Vec<ChatResponse>>,
    seen: Mutex<Vec<ChatRequest>>,
}

impl Scripted {
    const fn new(script: Vec<ChatResponse>) -> Self {
        Self {
            script: Mutex::new(script),
            seen: Mutex::new(Vec::new()),
        }
    }
    /// Every tool-result body the model was last shown, with its error flag.
    fn last_results(&self) -> Vec<(String, bool)> {
        let seen = self.seen.lock().unwrap();
        seen.last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|m| &m.blocks)
            .filter_map(|b| match b {
                Block::ToolResult {
                    inline: Some(body),
                    is_error,
                    ..
                } => Some((String::from_utf8_lossy(body).into_owned(), *is_error)),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl Provider for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.seen.lock().unwrap().push(req);
        let mut q = self.script.lock().unwrap();
        if q.is_empty() {
            return Err(ProviderError::Api("script exhausted".into()));
        }
        Ok(q.remove(0))
    }
}

fn text(t: &str) -> ChatResponse {
    ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::text(t)),
        usage: Usage::default(),
    }
}

fn tool_call(name: &str) -> ChatResponse {
    ChatResponse {
        assistant: Message::new(Role::Assistant).with_block(Block::ToolUse {
            id: "T1".into(),
            name: name.into(),
            input_json: b"{}".to_vec(),
            cache_marker: None,
        }),
        usage: Usage::default(),
    }
}

/// Answers `initialize` / `tools/list` / `tools/call` for one tool, `echo`,
/// whose result carries [`SECRET`]. Counts `tools/call` requests.
struct EchoMcp(Arc<Mutex<usize>>);

impl wiremock::Respond for EchoMcp {
    fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
        let body: Value = req.body_json().unwrap();
        let result = match body["method"].as_str().unwrap() {
            "initialize" => json!({ "protocolVersion": "2024-11-05", "capabilities": {} }),
            "tools/list" => json!({ "tools": [{
                "name": "echo", "description": "e", "inputSchema": { "type": "object" }
            }] }),
            "tools/call" => {
                *self.0.lock().unwrap() += 1;
                json!({ "content": [{ "type": "text", "text": format!("token={SECRET}") }] })
            }
            other => panic!("unexpected method {other}"),
        };
        wiremock::ResponseTemplate::new(200)
            .set_body_json(json!({ "jsonrpc": "2.0", "id": body["id"], "result": result }))
    }
}

/// Run the daemon's REAL swarm worker with one inline-MCP server `srv` (tool
/// `mcp__srv__echo`) under `governance`. Returns the provider and how many
/// `tools/call` requests the server received.
async fn run_mcp_worker(governance: origin_daemon::config::Governance) -> (Arc<Scripted>, usize) {
    use origin_swarm::{Budget, Coordinator, McpServerSpec, PlanHandle, WorkerSpec};
    let server = wiremock::MockServer::start().await;
    let calls = Arc::new(Mutex::new(0));
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(EchoMcp(Arc::clone(&calls)))
        .mount(&server)
        .await;
    let provider = Arc::new(Scripted::new(vec![tool_call("mcp__srv__echo"), text("done")]));
    let tmp = tempfile::TempDir::new().unwrap();
    let store = Arc::new(origin_store::Store::open(tmp.path().join("origin.db")).unwrap());
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .unwrap(),
    );
    let plan_store = Arc::new(origin_plan::PlanStore::open(Arc::clone(&store), Arc::clone(&cas)).unwrap());
    let plan = PlanHandle::new(
        Arc::new(tokio::sync::Mutex::new(origin_plan::Plan::default())),
        plan_store,
    );
    let active: Arc<tokio::sync::RwLock<Arc<dyn Provider>>> = Arc::new(tokio::sync::RwLock::new(Arc::clone(
        &provider,
    )
        as Arc<dyn Provider>));
    let mut coord = Coordinator::new(plan, format!("origin-a4-rt-{}", ulid::Ulid::new()));
    coord.set_default_worker(origin_daemon::swarm_worker::real_worker(
        active,
        Arc::clone(&cas),
        origin_keyvault::KeyVault::in_memory(),
        origin_planner::Plan::default(),
        governance,
    ));
    let handle = coord
        .spawn(WorkerSpec {
            goal: "echo".into(),
            allowed_tools: vec!["Read".into()],
            budget: Budget {
                max_wall_ms: 20_000,
                max_input_tokens: 1_000_000,
                max_output_tokens: 1_000_000,
                max_tool_calls: 8,
            },
            workspace: None,
            parent_actor: origin_plan::ActorId::new(0),
            model: None,
            mcp_servers: vec![McpServerSpec {
                name: "srv".into(),
                command: None,
                args: Vec::new(),
                url: Some(server.uri()),
            }],
            effort: None,
            ponytail: None,
        })
        .await
        .unwrap();
    coord.await_completion(&handle).await.unwrap();
    let n = *calls.lock().unwrap();
    (provider, n)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_governance_policy_denies_an_inline_mcp_tool_with_the_standard_denial() {
    let policy = origin_policy::PolicyEngine::new(vec![origin_policy::PolicyLayer {
        denied_tools: Some(vec!["mcp__srv__echo".into()]),
        ..Default::default()
    }]);
    let governance = origin_daemon::config::Governance {
        policy: Some(Arc::new(policy)),
        ..Default::default()
    };
    let (provider, calls) = run_mcp_worker(governance).await;
    assert_eq!(calls, 0, "a denied tool is never called");
    let results = provider.last_results();
    assert_eq!(results.len(), 1, "{results:?}");
    let (body, is_error) = &results[0];
    assert!(is_error, "{body}");
    assert!(
        body.starts_with("Error: permission denied for `mcp__srv__echo` (tier=")
            && body.contains("policy: tool `mcp__srv__echo`")
            && body.contains("The call was not executed."),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_secret_in_an_inline_mcp_result_is_redacted_and_stays_wrapped() {
    origin_daemon::redact::install(origin_daemon::redact::Redactor::new([(
        "ANTHROPIC_API_KEY".to_string(),
        SECRET.to_string(),
    )]));
    let (provider, calls) = run_mcp_worker(origin_daemon::config::Governance::default()).await;
    assert_eq!(calls, 1);
    let results = provider.last_results();
    assert_eq!(results.len(), 1, "{results:?}");
    let (body, is_error) = &results[0];
    assert!(!is_error, "{body}");
    assert!(body.starts_with("<external source=\"mcp:srv\">\n"), "{body}");
    assert!(body.ends_with("\n</external>"), "{body}");
    assert!(
        !body.contains(SECRET) && body.contains("[REDACTED:ANTHROPIC_API_KEY]"),
        "{body}"
    );
}

#[derive(Debug)]
struct FakeMcp(ToolMeta, Result<Value, String>);

#[async_trait]
impl DynTool for FakeMcp {
    fn meta(&self) -> &ToolMeta {
        &self.0
    }
    async fn invoke(&self, _args: Value) -> Result<Value, String> {
        self.1.clone()
    }
}

/// One call of a runtime tool `mcp__fake__op` that DECLARES itself pure and
/// auto-allowed, answering `result`, through the real `run_loop`.
async fn run_fake(result: Result<Value, String>, read_only: bool) -> Scripted {
    let mut tools: HashMap<String, Arc<dyn DynTool>> = HashMap::new();
    tools.insert(
        "mcp__fake__op".into(),
        Arc::new(FakeMcp(
            ToolMeta {
                name: "mcp__fake__op",
                description: "fake",
                tier: Tier::AutoAllowed,
                urgency: Urgency::Low,
                side_effects: SideEffects::Pure,
                input_schema: r#"{"type":"object"}"#,
                sandbox_profile: SandboxProfile::Inherit,
                hot: false,
            },
            result,
        )),
    );
    let provider = Scripted::new(vec![tool_call("mcp__fake__op"), text("done")]);
    let mut session = Session::new_with_id("sess-a4-rt".into(), "test-model".into());
    let mut opts = LoopOptions::default().without_streaming();
    opts.read_only = read_only;
    Box::pin(scope_runtime_tools(Arc::new(tools), async {
        Box::pin(run_loop(&mut session, "go", &provider, &AlwaysAllow, &opts))
            .await
            .unwrap();
    }))
    .await;
    provider
}

#[tokio::test]
async fn read_only_mode_denies_a_runtime_tool_whatever_it_declares() {
    let provider = run_fake(Ok(json!("ran")), true).await;
    let results = provider.last_results();
    let (body, is_error) = &results[0];
    assert!(is_error, "{body}");
    assert!(
        body.starts_with("Error: permission denied for `mcp__fake__op` (tier=") && body.contains("plan mode"),
        "{body}"
    );
}

#[tokio::test]
async fn an_oversized_mcp_error_body_is_bounded_inside_the_wrapper() {
    let provider = run_fake(Err(format!("boom {}", "e".repeat(40 * 1024))), false).await;
    let results = provider.last_results();
    let (body, is_error) = &results[0];
    assert!(is_error);
    assert!(
        body.starts_with("<external source=\"mcp:fake\">\nError: boom "),
        "{}",
        &body[..80]
    );
    assert!(
        body.ends_with("bytes shown]\n</external>"),
        "note inside the wrapper"
    );
    assert!(body.len() < 17 * 1024, "{} bytes", body.len());
}
