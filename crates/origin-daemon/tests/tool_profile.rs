// SPDX-License-Identifier: Apache-2.0
//! C7: the per-model edit-format table picks the advertised edit tools, and
//! `ORIGIN_TOOL_PROFILE=lean` narrows the builtins to the core hot set. Both
//! are decided once per session, so the tools prefix never changes mid-session.

// The ENV_LOCK guard intentionally spans the awaited `run_loop` calls: they
// read the env vars these tests set.
#![allow(clippy::panic, clippy::unwrap_used, clippy::await_holding_lock)]

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, ToolSchema, Usage};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Every test here sets or reads `ORIGIN_EDITFMT` / `ORIGIN_TOOL_PROFILE`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_env(editfmt: Option<&str>, profile: Option<&str>) {
    for (key, value) in [("ORIGIN_EDITFMT", editfmt), ("ORIGIN_TOOL_PROFILE", profile)] {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

/// Records every request and answers with plain text (one request per prompt).
struct RecordingProvider {
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text("done")),
            usage: Usage::default(),
        })
    }
}

fn opts() -> LoopOptions {
    LoopOptions {
        max_turns: 2,
        ..LoopOptions::default().without_streaming()
    }
}

/// The tools a fresh session on `model` advertises under the given env.
async fn advertised(model: &str, editfmt: Option<&str>, profile: Option<&str>) -> Vec<ToolSchema> {
    set_env(editfmt, profile);
    let provider = RecordingProvider {
        requests: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("recording", model);
    run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts())
        .await
        .unwrap();
    let tools = provider.requests.lock().unwrap()[0].tools.clone();
    set_env(None, None);
    tools
}

/// The list every session advertised before C7: each registered tool in
/// registry order, hot ones with their full schema, the rest deferred.
fn todays_list() -> Vec<(String, String, String)> {
    origin_tools::registry_iter()
        .map(|m| {
            if m.hot {
                (
                    m.name.to_string(),
                    m.description.to_string(),
                    m.input_schema.to_string(),
                )
            } else {
                (
                    m.name.to_string(),
                    format!(
                        "{} (deferred; call ToolSearch with select:{}, to fetch full schema)",
                        m.description, m.name
                    ),
                    r#"{"type":"object","properties":{}}"#.to_string(),
                )
            }
        })
        .collect()
}

fn triples(tools: &[ToolSchema]) -> Vec<(String, String, String)> {
    tools
        .iter()
        .map(|t| (t.name.clone(), t.description.clone(), t.input_schema_json.clone()))
        .collect()
}

fn names(tools: &[ToolSchema]) -> Vec<&str> {
    tools.iter().map(|t| t.name.as_str()).collect()
}

/// `todays_list` names, in registry order, minus `drop`.
fn todays_names_without(drop: &[&str]) -> Vec<String> {
    todays_list()
        .into_iter()
        .map(|(n, _, _)| n)
        .filter(|n| !drop.contains(&n.as_str()))
        .collect()
}

#[tokio::test]
async fn rich_profile_with_an_unflagged_or_frontier_model_advertises_todays_list() {
    let _g = env_lock();
    let today = todays_list();
    assert!(today.len() > 10, "registry must be populated");
    for model in [
        "claude-test",
        "claude-opus-4",
        "gpt-4o",
        "deepseek-coder",
        "some-random-model",
    ] {
        for profile in [None, Some("rich"), Some("garbage")] {
            let tools = advertised(model, None, profile).await;
            assert_eq!(triples(&tools), today, "model {model}, profile {profile:?}");
        }
    }
    // `ORIGIN_EDITFMT=0` bypasses the table even for a flagged model.
    let tools = advertised("gpt-3.5-turbo", Some("0"), None).await;
    assert_eq!(triples(&tools), today);
}

#[tokio::test]
async fn a_whole_file_model_keeps_write_but_not_the_search_replace_tools() {
    let _g = env_lock();
    let expected = todays_names_without(&["Edit", "MultiEdit", "ApplyPatch"]);
    // From the table (dated id: prefix match) and forced by `ORIGIN_EDITFMT`.
    for (model, editfmt) in [
        ("gpt-3.5-turbo", None),
        ("gpt-3.5-turbo-0125", Some("1")),
        ("claude-opus-4", Some("whole-file")),
    ] {
        let tools = advertised(model, editfmt, None).await;
        assert_eq!(
            names(&tools),
            expected,
            "model {model}, ORIGIN_EDITFMT {editfmt:?}"
        );
        assert!(names(&tools).contains(&"Write"));
    }
}

#[tokio::test]
async fn lean_profile_keeps_only_the_core_hot_set() {
    let _g = env_lock();
    // Core tools plus every tool the loop's own injected text tells the model
    // to call (`Recall` in offload hints, `TodoWrite` in the todo checklist).
    let core = [
        "Read",
        "Edit",
        "MultiEdit",
        "ApplyPatch",
        "Write",
        "Bash",
        "Grep",
        "Glob",
        "Recall",
        "TodoWrite",
    ];
    let mut lean = names(&advertised("claude-opus-4", None, Some("lean")).await)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    lean.sort();
    let mut want = core.map(str::to_string).to_vec();
    want.sort();
    assert_eq!(lean, want);

    // A whole-file model under lean: the edit format still narrows first.
    let tools = advertised("gpt-3.5-turbo", None, Some("lean")).await;
    let mut lean = names(&tools);
    lean.sort_unstable();
    assert_eq!(
        lean,
        ["Bash", "Glob", "Grep", "Read", "Recall", "TodoWrite", "Write"]
    );
}

#[tokio::test]
async fn tool_list_is_fixed_for_the_session_across_env_and_model_changes() {
    let _g = env_lock();
    set_env(None, None);
    let provider = RecordingProvider {
        requests: Mutex::new(Vec::new()),
    };
    let mut session = Session::new("recording", "gpt-3.5-turbo");
    run_loop(&mut session, "one", &provider, &AlwaysAllow, &opts())
        .await
        .unwrap();
    // Mid-session: both env vars change and the model is switched.
    set_env(Some("0"), Some("lean"));
    session.model = "claude-opus-4".to_string();
    run_loop(&mut session, "two", &provider, &AlwaysAllow, &opts())
        .await
        .unwrap();
    set_env(None, None);

    let reqs = provider.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(
        !names(&reqs[0].tools).contains(&"Edit"),
        "the first turn used the whole-file table entry"
    );
    let wire = |r: &ChatRequest| origin_provider_anthropic::encode_request_for_test(r)["tools"].to_string();
    assert_eq!(wire(&reqs[0]), wire(&reqs[1]), "tools bytes changed mid-session");
}
