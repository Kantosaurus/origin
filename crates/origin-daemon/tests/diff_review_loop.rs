// SPDX-License-Identifier: Apache-2.0
//! C6 (`ORIGIN_DIFF_REVIEW=1`): a tripwire hit annotates the edit result and
//! never blocks; at the terminal turn one tool-less request that carries only
//! the run's diff reviews it, and its findings get one extra round.

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopError, LoopOptions, LoopSummary};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use std::sync::Mutex;

/// This binary's tests set and clear `ORIGIN_DIFF_REVIEW`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const PROMPT: &str = "user-prompt-c6-marker";
const SECRET: &str = "sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn name(&self) -> &'static str {
        "scripted"
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().expect("lock").push(req);
        let mut q = self.responses.lock().expect("lock");
        if q.is_empty() {
            return Err(ProviderError::Api("no more scripted responses".into()));
        }
        Ok(q.remove(0))
    }
}

fn text(t: &str) -> Vec<Block> {
    vec![Block::text(t)]
}

fn write(path: &str, content: &str) -> Vec<Block> {
    vec![Block::ToolUse {
        id: "tu_w".into(),
        name: "Write".into(),
        input_json: serde_json::to_vec(&serde_json::json!({ "file_path": path, "content": content }))
            .expect("json"),
        cache_marker: None,
    }]
}

/// Run one loop over `turns` with the flag set to `flag`.
fn run(flag: bool, turns: Vec<Vec<Block>>) -> (Result<LoopSummary, LoopError>, Vec<ChatRequest>) {
    run_capped(flag, None, turns)
}

/// [`run`] with an optional `max_turns`.
fn run_capped(
    flag: bool,
    max_turns: Option<u32>,
    turns: Vec<Vec<Block>>,
) -> (Result<LoopSummary, LoopError>, Vec<ChatRequest>) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if flag {
        std::env::set_var("ORIGIN_DIFF_REVIEW", "1");
    } else {
        std::env::remove_var("ORIGIN_DIFF_REVIEW");
    }
    let provider = ScriptedProvider {
        responses: Mutex::new(
            turns
                .into_iter()
                .map(|blocks| ChatResponse {
                    assistant: blocks
                        .into_iter()
                        .fold(Message::new(Role::Assistant), Message::with_block),
                    usage: Usage::default(),
                })
                .collect(),
        ),
        requests: Mutex::new(Vec::new()),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut session = Session::new("diff-review", "scripted-model");
    let mut opts = LoopOptions::default().without_streaming();
    if let Some(m) = max_turns {
        opts.max_turns = m;
    }
    let out = rt.block_on(run_loop(&mut session, PROMPT, &provider, &AlwaysAllow, &opts));
    std::env::remove_var("ORIGIN_DIFF_REVIEW");
    let requests = std::mem::take(&mut *provider.requests.lock().expect("lock"));
    (out, requests)
}

/// The text of the tool result for `tu_w` and its `is_error`.
fn write_result(req: &ChatRequest) -> (String, bool) {
    req.messages
        .iter()
        .flat_map(|m| &m.blocks)
        .find_map(|b| match b {
            Block::ToolResult {
                tool_use_id,
                inline,
                is_error,
                ..
            } if tool_use_id == "tu_w" => Some((
                String::from_utf8_lossy(inline.as_deref().unwrap_or_default()).into_owned(),
                *is_error,
            )),
            _ => None,
        })
        .expect("Write result")
}

fn all_text(m: &Message) -> String {
    m.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn tripwire_annotates_without_blocking_and_no_findings_ends_the_run() {
    let d = tempfile::tempdir().expect("tempdir");
    let path = d.path().join("cfg.py").to_string_lossy().into_owned();
    let body = format!("API_KEY = \"{SECRET}\"\n");
    let (out, requests) = run(true, vec![write(&path, &body), text("done"), text("NO FINDINGS")]);
    assert_eq!(out.expect("run ok").assistant_text, "done");
    assert_eq!(
        std::fs::read_to_string(&path).expect("written"),
        body,
        "never blocks"
    );

    let (result, is_error) = write_result(&requests[1]);
    assert!(!is_error, "{result}");
    assert!(result.contains(":1: possible hard-coded secret"), "{result}");
    assert!(
        !result.contains(SECRET),
        "the note must not echo the secret: {result}"
    );

    // The review: fresh context, no tools, only the diff; NO FINDINGS ⇒ no extra round.
    assert_eq!(requests.len(), 3, "work turn, done turn, review");
    let review = &requests[2];
    assert!(review.tools.is_empty(), "review carries no tools");
    assert_eq!(review.messages.len(), 1, "no conversation history");
    let input = all_text(&review.messages[0]);
    assert!(
        input.contains(&format!("+++ b/{path}")) && input.contains("+API_KEY"),
        "{input}"
    );
    assert!(!input.contains(PROMPT), "the user prompt is not sent: {input}");
    assert!(review.system.contains("NO FINDINGS"), "{}", review.system);
    assert_eq!(review.model, "scripted-model");
}

#[test]
fn findings_get_one_extra_round_and_only_one_review() {
    let d = tempfile::tempdir().expect("tempdir");
    let path = d.path().join("a.txt").to_string_lossy().into_owned();
    let (out, requests) = run(
        true,
        vec![
            write(&path, "x\n"),
            text("done"),
            text("1. a.txt lacks a header"),
            text("headers are optional here"),
        ],
    );
    assert_eq!(out.expect("run ok").assistant_text, "headers are optional here");
    assert_eq!(requests.len(), 4, "one review, one extra round, no second review");
    let last = requests[3].messages.last().expect("message");
    assert!(matches!(last.role, Role::User));
    let t = all_text(last);
    assert!(t.contains("[diff-review]") && t.contains("lacks a header"), "{t}");
}

#[test]
fn failed_review_is_not_fatal_and_the_flag_is_opt_in() {
    let d = tempfile::tempdir().expect("tempdir");
    let path = d.path().join("b.py").to_string_lossy().into_owned();
    let body = format!("K = \"{SECRET}\"\n");
    // Review call fails (script exhausted) ⇒ the run still ends normally.
    let (out, requests) = run(true, vec![write(&path, &body), text("done")]);
    assert_eq!(out.expect("run ok").assistant_text, "done");
    assert_eq!(requests.len(), 3);

    // Flag off: no tripwire note and no review request.
    std::fs::remove_file(&path).expect("rm");
    let (out, requests) = run(false, vec![write(&path, &body), text("done")]);
    assert_eq!(out.expect("run ok").assistant_text, "done");
    assert_eq!(requests.len(), 2);
    assert!(!write_result(&requests[1]).0.contains("[tripwire]"));
}

/// I1: no review on the last allowed turn — its findings could only turn a
/// finished run into `MaxTurns`.
#[test]
fn no_review_on_the_last_allowed_turn() {
    let d = tempfile::tempdir().expect("tempdir");
    let path = d.path().join("c.txt").to_string_lossy().into_owned();
    let (out, requests) = run_capped(
        true,
        Some(2),
        vec![write(&path, "x\n"), text("done"), text("1. a finding")],
    );
    assert_eq!(out.expect("finished run stays Ok").assistant_text, "done");
    assert_eq!(requests.len(), 2, "no review request on the last turn");
}

/// M8: an `ApplyPatch` touching two files puts both in the review diff.
#[test]
fn apply_patch_touching_two_files_is_reviewed_whole() {
    let d = tempfile::tempdir().expect("tempdir");
    let a = d.path().join("one.txt").to_string_lossy().into_owned();
    let b = d.path().join("two.txt").to_string_lossy().into_owned();
    let patch =
        format!("*** Begin Patch\n*** Add File: {a}\n+alpha\n*** Add File: {b}\n+beta\n*** End Patch\n");
    let call = vec![Block::ToolUse {
        id: "tu_p".into(),
        name: "ApplyPatch".into(),
        input_json: serde_json::to_vec(&serde_json::json!({ "patch": patch })).expect("json"),
        cache_marker: None,
    }];
    let (out, requests) = run(true, vec![call, text("done"), text("NO FINDINGS")]);
    assert_eq!(out.expect("run ok").assistant_text, "done");
    assert_eq!(std::fs::read_to_string(&b).expect("patched").trim(), "beta");
    let input = all_text(&requests.last().expect("review").messages[0]);
    for (p, line) in [(&a, "+alpha"), (&b, "+beta")] {
        assert!(
            input.contains(&format!("+++ b/{p}")) && input.contains(line),
            "{input}"
        );
    }
}
