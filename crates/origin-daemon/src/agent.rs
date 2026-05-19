//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::session::Session;
use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};
use origin_permission::{check, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_tools::{registry_iter, ToolMeta};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone)]
pub struct LoopOptions {
    pub max_turns: u32,
    pub cas: Option<Arc<Store>>,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            max_turns: 25,
            cas: None,
        }
    }
}

impl LoopOptions {
    /// Attach a CAS so tool outputs are stored by handle instead of inline.
    #[must_use]
    pub fn with_cas(mut self, store: Arc<Store>) -> Self {
        self.cas = Some(store);
        self
    }
}

#[derive(Debug)]
pub struct LoopSummary {
    pub assistant_text: String,
    pub turns: u32,
}

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("provider: {0}")]
    Provider(#[from] origin_provider::ProviderError),
    #[error("hit max_turns ({0})")]
    MaxTurns(u32),
    #[error("tool not found: {0}")]
    UnknownTool(String),
    #[error("tool denied: {0}")]
    Denied(String),
    #[error("tool failure: {0}")]
    ToolFailure(String),
    #[error("malformed tool args: {0}")]
    BadArgs(String),
}

/// Run the agent loop until the assistant emits a turn without any `tool_use`
/// blocks, or until `max_turns` is reached.
///
/// # Errors
/// Returns `LoopError` for provider failures, permission denial, unknown tools,
/// tool execution failures, malformed tool inputs, or hitting `max_turns`.
pub async fn run_loop(
    session: &mut Session,
    user_text: &str,
    provider: &dyn Provider,
    prompter: &dyn Prompter,
    opts: &LoopOptions,
) -> Result<LoopSummary, LoopError> {
    session.push(Message::new(Role::User).with_block(Block::text(user_text)));

    let tools_schema = registry_iter()
        .map(|m| origin_provider::ToolSchema {
            name: m.name.to_string(),
            description: m.description.to_string(),
            input_schema_json: m.input_schema.to_string(),
        })
        .collect::<Vec<_>>();

    for turn in 1..=opts.max_turns {
        let req = ChatRequest {
            system: String::new(),
            messages: session.snapshot(),
            model: session.model.clone(),
            tools: tools_schema.clone(),
        };
        let resp = provider.chat(req).await?;
        session.push(resp.assistant.clone());

        // Gather tool_use blocks (clone owned data because we'll borrow `meta`).
        let tool_uses: Vec<(String, String, Vec<u8>)> = resp
            .assistant
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse {
                    id, name, input_json, ..
                } => Some((id.clone(), name.clone(), input_json.clone())),
                _ => None,
            })
            .collect();

        if tool_uses.is_empty() {
            let text = resp
                .assistant
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<String>();
            return Ok(LoopSummary {
                assistant_text: text,
                turns: turn,
            });
        }

        // Dispatch each tool_use sequentially.
        let mut tool_results: Vec<Block> = Vec::with_capacity(tool_uses.len());
        for (id, name, input_bytes) in tool_uses {
            let meta = registry_iter()
                .find(|m| m.name == name)
                .ok_or_else(|| LoopError::UnknownTool(name.clone()))?;
            let args: Value =
                serde_json::from_slice(&input_bytes).map_err(|e| LoopError::BadArgs(e.to_string()))?;
            let preview = args.to_string();

            let decision = check(meta, &preview, prompter).await;
            if decision.outcome == Outcome::Deny {
                return Err(LoopError::Denied(name.clone()));
            }

            let result_text = dispatch_tool(meta, &args).await?;
            let result_bytes = result_text.into_bytes();
            let block = if let Some(cas) = opts.cas.as_ref() {
                let h: Hash = cas
                    .put(&result_bytes)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?;
                Block::ToolResult {
                    tool_use_id: id,
                    handle: Some(*h.as_bytes()),
                    inline: None,
                    cache_marker: None,
                }
            } else {
                Block::ToolResult {
                    tool_use_id: id,
                    handle: None,
                    inline: Some(result_bytes),
                    cache_marker: None,
                }
            };
            tool_results.push(block);
        }

        // Append tool results as a single Role::Tool message (provider crates
        // will translate this to the right wire shape per provider).
        let mut tool_msg = Message::new(Role::Tool);
        tool_msg.blocks = tool_results;
        session.push(tool_msg);
    }
    Err(LoopError::MaxTurns(opts.max_turns))
}

async fn dispatch_tool(meta: &ToolMeta, args: &Value) -> Result<String, LoopError> {
    match meta.name {
        "Read" => {
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Read: missing `path`".into()))?;
            origin_tools::builtins::read::read_tool(path).map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "Glob" => {
            let pat = args
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Glob: missing `pattern`".into()))?;
            let hits = origin_tools::builtins::glob_tool::glob_tool(pat).map_err(LoopError::ToolFailure)?;
            Ok(hits.join("\n"))
        }
        "Grep" => {
            let pat = args
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Grep: missing `pattern`".into()))?;
            let root = args
                .get("root")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Grep: missing `root`".into()))?;
            let hits =
                origin_tools::builtins::grep_tool::grep_tool(pat, root).map_err(LoopError::ToolFailure)?;
            Ok(hits.join("\n"))
        }
        "Edit" => {
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `path`".into()))?;
            let old = args
                .get("old_string")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `old_string`".into()))?;
            let new = args
                .get("new_string")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Edit: missing `new_string`".into()))?;
            origin_tools::builtins::edit::edit_tool(path, old, new)
                .map(|()| "edit ok".to_string())
                .map_err(LoopError::ToolFailure)
        }
        "Bash" => {
            let cmd = args
                .get("command")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Bash: missing `command`".into()))?;
            let out = origin_tools::builtins::bash::bash_tool(cmd)
                .await
                .map_err(LoopError::ToolFailure)?;
            Ok(format!(
                "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                out.exit_code, out.stdout, out.stderr
            ))
        }
        other => Err(LoopError::UnknownTool(other.into())),
    }
}
