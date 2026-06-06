// SPDX-License-Identifier: Apache-2.0
//! Headless one-shot (`origin run`). Connects to the daemon, sends a
//! single Prompt, drains the stream, exits. No Ratatui renderer.
//!
//! Output contract (`--output-format`, with `--json` as a back-compat alias for
//! `stream-json`):
//! - `text` (default): the assistant's text streamed to stdout.
//! - `stream-json`: one JSON object per line for every IPC `StreamEvent`.
//! - `json`: a single final JSON object `{"text": "<full reply>"}`.
//!
//! Structured output (`--json-schema <file>`): the schema is injected into the
//! prompt, the reply is parsed + validated against it, and on failure the model
//! is re-prompted (bounded retries). On success only the validated, pretty
//! JSON value is printed. *Closes: claude/gemini/opencode `--json-schema` +
//! `--output-format json|stream-json` structured output.*

use anyhow::{anyhow, Result};
use jsonschema::JSONSchema;
use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;
use serde_json::{json, Value};

/// Maximum number of corrective re-prompts in `--json-schema` mode (so a model
/// that keeps emitting invalid JSON cannot loop forever).
const MAX_SCHEMA_RETRIES: u32 = 2;

/// Arguments for [`run`], grouped into a struct so the dispatcher does not have
/// to pass nine positional parameters.
pub struct RunArgs {
    /// The user prompt.
    pub text: String,
    /// Back-compat alias for `--output-format stream-json`.
    pub json: bool,
    /// Remote daemon URL (`origin://host:port#fingerprint`).
    pub remote: Option<String>,
    /// Optional bearer token for remote auth (not yet sent on the wire).
    pub bearer: Option<String>,
    /// Model override.
    pub model: Option<String>,
    /// Reasoning-effort token.
    pub effort: Option<String>,
    /// Extended-thinking budget in tokens (aider `--thinking-tokens`), already
    /// validated (`> 0`) and merged with the startup seed by the dispatcher.
    /// `None` ⇒ wire unchanged. Only the Anthropic provider honours it.
    pub thinking_tokens: Option<u32>,
    /// Ad-hoc model alias definitions from repeated `--alias name=target`. Merged
    /// on top of the config `[aliases]` table; resolution runs before the prompt
    /// is sent. Empty ⇒ config-only resolution (or pass-through).
    pub aliases: Vec<String>,
    /// Files to attach as multimodal context.
    pub attach: Vec<String>,
    /// Stdout contract: `text` | `json` | `stream-json`.
    pub output_format: Option<String>,
    /// Path to a JSON Schema enabling structured-output mode.
    pub json_schema: Option<String>,
    /// Extra workspace roots (cline multi-root).
    pub roots: Vec<String>,
}

/// The resolved stdout contract.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    StreamJson,
    Json,
}

impl OutputFormat {
    /// Resolve from `--output-format` (preferred) falling back to the `--json`
    /// boolean alias. An unknown `--output-format` is a hard error.
    fn resolve(json_flag: bool, fmt: Option<&str>) -> Result<Self> {
        match fmt {
            None => Ok(if json_flag { Self::StreamJson } else { Self::Text }),
            Some("text") => Ok(Self::Text),
            Some("stream-json") => Ok(Self::StreamJson),
            Some("json") => Ok(Self::Json),
            Some(other) => Err(anyhow!(
                "unknown --output-format `{other}` (expected one of: text, json, stream-json)"
            )),
        }
    }
}

/// How a single driven turn should emit while draining.
#[derive(Clone, Copy)]
enum Emit {
    /// Stream assistant text deltas to stdout live (default human view).
    Text,
    /// Emit one JSON line per `StreamEvent`.
    StreamJson,
    /// Accumulate only; emit nothing (caller prints the final value).
    Silent,
}

/// Polymorphic wrapper around the two supported transports. Local
/// connections use the named-pipe / Unix-socket [`Connector`]; remote
/// connections come in through QUIC.
enum Conn {
    Local(origin_ipc::transport::Connection),
    Remote(origin_ipc::quic::QuicConnection),
}

impl Conn {
    async fn write_raw(&mut self, raw: &[u8]) -> anyhow::Result<()> {
        match self {
            Self::Local(c) => Ok(c.write_raw(raw).await?),
            Self::Remote(c) => c.write_raw(raw).await.map_err(|e| anyhow::anyhow!("{e}")),
        }
    }

    async fn read_frame(&mut self) -> anyhow::Result<(FrameKind, Vec<u8>)> {
        match self {
            Self::Local(c) => Ok(c.read_frame().await?),
            Self::Remote(c) => c.read_frame().await.map_err(|e| anyhow::anyhow!("{e}")),
        }
    }
}

/// Drive a single prompt through the daemon and exit.
///
/// # Errors
/// Returns when the daemon transport closes or returns an error frame, when an
/// `--output-format` value is unrecognized, or when `--json-schema` validation
/// fails after the retry budget is exhausted.
pub async fn run(args: RunArgs) -> Result<()> {
    let RunArgs {
        text,
        json,
        remote,
        bearer,
        model,
        effort,
        thinking_tokens,
        aliases,
        attach,
        output_format,
        json_schema,
        roots,
    } = args;
    // Future work: send `bearer` as part of the remote handshake.
    let _ = bearer;

    let fmt = OutputFormat::resolve(json, output_format.as_deref())?;
    let raw_model =
        model.unwrap_or_else(|| std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into()));
    // Resolve the requested model against the merged alias map (config
    // `[aliases]` + ad-hoc `--alias`). Undefined alias / literal id ⇒
    // pass-through (byte-identical to no aliases).
    let model = resolve_run_model(&raw_model, &aliases)?;
    let attachments = encode_attachments(&attach)?;

    let mut conn = match remote {
        None => {
            let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
            Conn::Local(Connector::connect(&path).await?)
        }
        Some(url) => {
            let parsed = crate::admin_url::parse_origin_url(&url)?;
            let ca = parsed.fingerprint_to_ca_placeholder();
            let qc = origin_ipc::quic::QuicConnector::connect(parsed.addr, "origin-daemon", &ca)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Conn::Remote(qc)
        }
    };

    if let Some(schema_path) = json_schema {
        return run_structured(
            &mut conn,
            &text,
            &model,
            effort,
            thinking_tokens,
            attachments,
            roots,
            &schema_path,
        )
        .await;
    }

    // Plain (non-schema) path. Attachments ride on the single turn.
    let emit = match fmt {
        OutputFormat::Text => Emit::Text,
        OutputFormat::StreamJson => Emit::StreamJson,
        OutputFormat::Json => Emit::Silent,
    };
    let prompt = PromptRequest {
        system: String::new(),
        model,
        user_text: text,
        session_id: None,
        effort,
        thinking_tokens,
        attachments,
        read_only: false,
        roots,
        permission_ask: false,
        // Headless is one-shot and never switches accounts mid-session, so the
        // daemon resolves the startup/global account ⇒ wire byte-identical.
        account: None,
    };
    let reply = drive_turn(&mut conn, prompt, emit).await?;
    if matches!(fmt, OutputFormat::Json) {
        let line = serde_json::to_string(&json!({ "text": reply }))?;
        println!("{line}");
    }
    Ok(())
}

/// Structured-output mode: inject the schema, validate the reply, and re-prompt
/// on failure up to [`MAX_SCHEMA_RETRIES`] times. Emits only the validated,
/// pretty JSON value on success.
#[allow(clippy::too_many_arguments)]
async fn run_structured(
    conn: &mut Conn,
    text: &str,
    model: &str,
    effort: Option<String>,
    thinking_tokens: Option<u32>,
    attachments: Vec<origin_multimodal::ContentBlock>,
    roots: Vec<String>,
    schema_path: &str,
) -> Result<()> {
    let schema_src = std::fs::read_to_string(schema_path)
        .map_err(|e| anyhow!("read --json-schema `{schema_path}`: {e}"))?;
    let schema_value: Value = serde_json::from_str(&schema_src)
        .map_err(|e| anyhow!("--json-schema `{schema_path}` is not valid JSON: {e}"))?;
    let compiled = JSONSchema::options()
        .compile(&schema_value)
        .map_err(|e| anyhow!("--json-schema `{schema_path}` is not a valid JSON Schema: {e}"))?;

    let mut user_text = format!(
        "{text}\n\nRespond with ONLY a single JSON value (no prose, no code fences) that validates \
         against this JSON Schema:\n{schema_src}"
    );
    for attempt in 0..=MAX_SCHEMA_RETRIES {
        let prompt = PromptRequest {
            system: String::new(),
            model: model.to_string(),
            user_text: user_text.clone(),
            session_id: None,
            effort: effort.clone(),
            thinking_tokens,
            // Attachments only matter on the first attempt's content.
            attachments: if attempt == 0 {
                attachments.clone()
            } else {
                Vec::new()
            },
            read_only: false,
            roots: roots.clone(),
            permission_ask: false,
            account: None,
        };
        let reply = drive_turn(conn, prompt, Emit::Silent).await?;
        let candidate = extract_json(&reply);
        match parse_and_validate(&compiled, candidate) {
            Ok(value) => {
                println!("{}", serde_json::to_string_pretty(&value)?);
                return Ok(());
            }
            Err(errs) if attempt < MAX_SCHEMA_RETRIES => {
                user_text = format!(
                    "Your previous response did not validate against the JSON Schema.\n\
                     Errors: {errs}\n\nThe schema is:\n{schema_src}\n\nYour previous response was:\n{reply}\n\n\
                     Reply with ONLY the corrected JSON value (no prose, no code fences)."
                );
            }
            Err(errs) => {
                return Err(anyhow!(
                    "structured output failed schema validation after {} attempts: {errs}",
                    MAX_SCHEMA_RETRIES + 1
                ));
            }
        }
    }
    unreachable!("loop returns on the final attempt")
}

/// Parse `candidate` as JSON and validate it against `compiled`. Returns the
/// parsed value on success or a joined error string on failure.
fn parse_and_validate(compiled: &JSONSchema, candidate: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(candidate).map_err(|e| format!("reply was not valid JSON: {e}"))?;
    // Collect any validation errors into owned strings first so the borrowing
    // error iterator is dropped before we move `value` out of this function.
    let errors: Vec<String> = compiled
        .validate(&value)
        .err()
        .map(|it| it.map(|e| format!("{e}")).collect())
        .unwrap_or_default();
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(errors.join("; "))
    }
}

/// Strip a surrounding ```json … ``` (or bare ``` … ```) fence if the model
/// wrapped its JSON in one, and trim whitespace. Returns the inner candidate.
fn extract_json(reply: &str) -> &str {
    let trimmed = reply.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop an optional language tag on the opening fence line.
    let after_tag = rest.find('\n').map_or(rest, |nl| &rest[nl + 1..]);
    after_tag.strip_suffix("```").unwrap_or(after_tag).trim()
}

/// Send one `PromptRequest` and drain the stream to its terminal frame,
/// returning the accumulated assistant text. `emit` controls live output.
/// One-shot prompt → assistant text via the LOCAL daemon.
///
/// For in-process callers that need a single model turn without the full `run`
/// surface (e.g. `origin review --llm`). Connects to the local daemon
/// (`ORIGIN_SOCK` or the default path), drives one silent, read-only turn, and
/// returns the assistant text. Errors with actionable guidance when the daemon
/// is unreachable.
///
/// # Errors
/// Returns when the daemon cannot be reached or the turn fails.
pub async fn one_shot_text(model: &str, user_text: String) -> Result<String> {
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut conn = Conn::Local(Connector::connect(&path).await.map_err(|e| {
        anyhow::anyhow!(
            "could not reach the origin daemon ({e}); start a session with `origin` first, \
             or use plain `origin review` (static, no daemon)"
        )
    })?);
    let prompt = PromptRequest {
        system: String::new(),
        model: model.to_string(),
        user_text,
        session_id: None,
        effort: None,
        thinking_tokens: None,
        attachments: Vec::new(),
        // A review never edits, so deny mutating tools for this turn.
        read_only: true,
        roots: Vec::new(),
        permission_ask: false,
        account: None,
    };
    drive_turn(&mut conn, prompt, Emit::Silent).await
}

async fn drive_turn(conn: &mut Conn, prompt: PromptRequest, emit: Emit) -> Result<String> {
    let body = serde_json::to_vec(&ClientMessage::prompt(prompt))?;
    conn.write_raw(&encode(1, FrameKind::Request, &body)).await?;

    let mut acc = String::new();
    loop {
        let (kind, frame) = conn.read_frame().await?;
        // An ErrorFrame carries a UTF-8 loop/provider failure message; surface
        // it as a non-zero exit rather than silently completing.
        if matches!(kind, FrameKind::ErrorFrame) {
            return Err(anyhow!("{}", String::from_utf8_lossy(&frame)));
        }
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&frame) {
            if let StreamEvent::TextDelta { text } = &ev {
                acc.push_str(text);
            }
            match emit {
                Emit::Text => {
                    if let StreamEvent::TextDelta { text } = &ev {
                        use std::io::Write as _;
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        write!(out, "{text}")?;
                        out.flush()?;
                    }
                }
                Emit::StreamJson => {
                    use std::io::Write as _;
                    let stdout = std::io::stdout();
                    let mut out = stdout.lock();
                    writeln!(out, "{}", serde_json::to_string(&ev)?)?;
                }
                Emit::Silent => {}
            }
            continue;
        }
        // Non-`StreamEvent` frame ⇒ the terminal PromptReply.
        break;
    }
    Ok(acc)
}

/// Resolve a requested model id against the merged alias map for `origin run`.
///
/// The config `[aliases]` table (if any) is loaded and merged with the ad-hoc
/// `--alias name=target` specs (ad-hoc wins on collision), then [`crate::config::resolve_alias`]
/// substitutes the target. An undefined alias — or any literal model id — passes
/// through unchanged, so the pre-alias behaviour is byte-identical. A config
/// read failure is non-fatal (treated as no config aliases); only a malformed
/// `--alias` spec is a hard error.
///
/// # Errors
/// Returns an error when a `--alias` spec is malformed (missing `=`, empty name,
/// or empty target).
fn resolve_run_model(raw_model: &str, alias_specs: &[String]) -> Result<String> {
    let base = crate::config::load()
        .ok()
        .flatten()
        .map(|c| c.aliases)
        .unwrap_or_default();
    let merged = crate::config::merge_alias_specs(&base, alias_specs).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(crate::config::resolve_alias(&merged, raw_model))
}

/// Read each path and encode it into an `origin_multimodal::ContentBlock`.
fn encode_attachments(paths: &[String]) -> Result<Vec<origin_multimodal::ContentBlock>> {
    let mut blocks = Vec::with_capacity(paths.len());
    for p in paths {
        let bytes = std::fs::read(p).map_err(|e| anyhow::anyhow!("attach `{p}`: {e}"))?;
        let block = origin_multimodal::to_content_block(&bytes, Some(p.as_str()))
            .map_err(|e| anyhow::anyhow!("attach `{p}`: {e}"))?;
        blocks.push(block);
    }
    Ok(blocks)
}

fn default_path() -> String {
    #[cfg(unix)]
    {
        format!("{}/origin.sock", std::env::temp_dir().display())
    }
    #[cfg(windows)]
    {
        r"\\.\pipe\origin".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_json, parse_and_validate, OutputFormat};
    use jsonschema::JSONSchema;
    use serde_json::json;

    #[test]
    fn output_format_resolves_alias_and_explicit() {
        assert!(matches!(
            OutputFormat::resolve(false, None).expect("text"),
            OutputFormat::Text
        ));
        assert!(matches!(
            OutputFormat::resolve(true, None).expect("alias"),
            OutputFormat::StreamJson
        ));
        assert!(matches!(
            OutputFormat::resolve(false, Some("json")).expect("json"),
            OutputFormat::Json
        ));
        assert!(OutputFormat::resolve(false, Some("bogus")).is_err());
    }

    #[test]
    fn extract_json_strips_fences() {
        assert_eq!(extract_json("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(extract_json("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(extract_json("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(extract_json("  {\"a\":1}  "), "{\"a\":1}");
    }

    #[test]
    fn validate_accepts_conforming_and_rejects_violating() {
        let schema = json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
            "required": ["n"]
        });
        let compiled = JSONSchema::options().compile(&schema).expect("compile");
        assert!(parse_and_validate(&compiled, "{\"n\": 3}").is_ok());
        assert!(parse_and_validate(&compiled, "{\"n\": \"x\"}").is_err());
        assert!(parse_and_validate(&compiled, "not json").is_err());
    }
}
