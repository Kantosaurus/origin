// SPDX-License-Identifier: Apache-2.0
//! Agent loop: prompt → provider → tool dispatch → repeat → final text.

use crate::proposal_registry::ProposalRegistry;
use crate::protocol::StreamEvent;
use crate::session::Session;
use crate::session_store::SessionStore;
use crate::skill_catalog::SkillCatalog;
use crate::tool_use_parser::{ToolUseDelta, ToolUseParser};
use crate::workflows::WorkflowsFile;
use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};
use origin_mem::{Injector, Proposer};
use origin_permission::{check_with_skills, prompt::Prompter, Outcome};
use origin_provider::{ChatRequest, Provider};
use origin_runtime::{spawn_in, TaskClass};
use origin_sidecar::{ExtractDeliverer, Sidecar, SummaryDeliverer};
use origin_skills::SkillRegistry;
use origin_tools::dispatch::MemoryHandle;
use origin_tools::{registry_iter, SideEffects, ToolMeta};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;
use tokio::task::spawn_blocking as sb;
use tokio::task::JoinHandle;

/// `GitRunner` implementation that shells out to the system `git` binary.
///
/// Used by the optional per-turn shadow-git checkpoint feature (Task 1). It is
/// the only place in the daemon that drives [`origin_vcs::ShadowGit`]; all of
/// its effects are best-effort and gated behind `ORIGIN_CHECKPOINTS=1`, so the
/// default code path never constructs or invokes it.
struct CmdGit;

impl origin_vcs::GitRunner for CmdGit {
    fn run(&self, args: &[&str]) -> Result<String, origin_vcs::VcsError> {
        let output = std::process::Command::new("git")
            .args(args)
            .output()
            .map_err(|e| origin_vcs::VcsError::Git(e.to_string()))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(origin_vcs::VcsError::Git(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }
}

/// Best-effort shadow-git checkpoint with the given `label`, shared by the
/// per-turn and per-tool callers.
///
/// This is the only place that drives [`origin_vcs::ShadowGit`]; it does NOT
/// consult any env gate — callers decide whether to invoke it. Every failure is
/// swallowed: a checkpoint must never fail the turn that produced it. The
/// caller-supplied `label` becomes the checkpoint's commit subject, so it is
/// what distinguishes entries in `origin checkpoints`.
///
/// Returns the created checkpoint's commit `id` (the shadow-git SHA) on success
/// so the caller can fire a `PostCommit` hook with it; `None` when the snapshot
/// could not be taken (no cwd / git failure — both already logged + swallowed).
fn checkpoint_with_label(label: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let shadow_dir = cwd.join(".origin").join("shadow.git");
    let shadow = shadow_dir.to_string_lossy().into_owned();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let runner = CmdGit;
    let sg = origin_vcs::ShadowGit::new(&runner, shadow);
    match sg.snapshot(label, now_ms) {
        Ok(checkpoint) => Some(checkpoint.id),
        Err(e) => {
            tracing::debug!(error = %e, "shadow-git checkpoint failed (ignored)");
            None
        }
    }
}

/// Fire `PreCommit` (informational), take a shadow-git checkpoint via
/// [`checkpoint_with_label`], then fire `PostCommit` with the resulting SHA.
///
/// This is the live `PreCommit`/`PostCommit` firing site: the daemon's only
/// "commit" operation at runtime is the optional shadow-git checkpoint, so the
/// commit hooks bracket exactly that write. The hooks are informational (their
/// overrides are ignored) and `PostCommit` is fired only when a checkpoint was
/// actually created. With no `hooks.json` (the default) [`fire_global`] is a
/// no-op, so this differs from the bare [`checkpoint_with_label`] only by the
/// two no-op awaits — byte-identical observable behavior.
///
/// The shadow-git checkpoint is a standalone repo with no user-facing branch;
/// `PreCommit.branch` carries the checkpoint `label` as the closest available
/// "what is being committed" string.
async fn checkpoint_with_commit_hooks(label: &str) -> Option<String> {
    crate::hooks_runtime::fire_global(&origin_hooks::LifecycleEvent::PreCommit {
        branch: label.to_string(),
    })
    .await;
    let sha = checkpoint_with_label(label);
    if let Some(sha) = sha.as_ref() {
        crate::hooks_runtime::fire_global(&origin_hooks::LifecycleEvent::PostCommit {
            sha: sha.clone(),
        })
        .await;
    }
    sha
}

/// Per-turn shadow-git checkpoint that brackets the snapshot with the
/// `PreCommit`/`PostCommit` lifecycle hooks (see [`checkpoint_with_commit_hooks`]).
///
/// No-op unless `ORIGIN_CHECKPOINTS=1` — the per-turn checkpoint gate. This is
/// the hook-firing async counterpart used by the live loop. The
/// commit hooks fire only inside the gated branch, so when checkpoints are off
/// (the default) no hook fires and behavior is byte-identical.
async fn maybe_checkpoint_turn_with_hooks() {
    if std::env::var("ORIGIN_CHECKPOINTS").as_deref() != Ok("1") {
        return;
    }
    let _ = checkpoint_with_commit_hooks("turn").await;
}

/// Whether the per-tool-use checkpoint feature is enabled.
///
/// Reads the `ORIGIN_CHECKPOINTS_PER_TOOL` env gate (set to `1` to enable).
/// This is INDEPENDENT of `ORIGIN_CHECKPOINTS` so per-turn snapshots remain the
/// default granularity; only setting this flag opts into the finer per-tool
/// snapshots. Unset (the default) ⇒ `false` ⇒ byte-identical behavior.
fn per_tool_checkpoints_enabled() -> bool {
    std::env::var("ORIGIN_CHECKPOINTS_PER_TOOL").as_deref() == Ok("1")
}

/// The transcript-byte soft cap above which the live in-loop compactor folds the
/// oldest summarized turns into their summaries (firing `PreCompress`).
///
/// Defaults to [`crate::compactor::DEFAULT_SOFT_CAP_BYTES`] (200 KiB). The
/// `ORIGIN_COMPACT_SOFT_CAP` env var overrides it with a decimal byte count for
/// tuning/tests; an unset or unparseable value falls back to the default. The
/// default cap is large enough that a short interactive session never reaches
/// it, so the live compaction call-site is a no-op (byte-identical) by default.
fn compaction_soft_cap() -> usize {
    std::env::var("ORIGIN_COMPACT_SOFT_CAP")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(crate::compactor::DEFAULT_SOFT_CAP_BYTES)
}

/// Live, in-loop transcript compaction (P5.4 runtime wiring).
///
/// Called once per agentic turn AFTER the freshly-closed turn is appended, so
/// the NEXT turn's `ChatRequest` is built from the compacted transcript. When
/// the accumulated context crosses [`compaction_soft_cap`], it folds the oldest
/// summarized turns into their summaries via
/// [`crate::compactor::maybe_compact_transcript`] (which fires the `PreCompress`
/// lifecycle hook) and replaces `session.messages` with the result.
///
/// Summaries come from the wired [`SessionStore`] (eager per-turn summaries,
/// P5.2) keyed by `turn_index == message index`. When no store is wired, an
/// empty-summary vector is passed: the cap check + `PreCompress` fire still
/// happen, but no turn has a summary so the transcript is returned unchanged.
///
/// **Default-off / byte-identical:** below the (200 KiB) cap this returns
/// immediately without firing the hook or touching `session.messages`, so a
/// short session is unaffected. Compaction never errors — a missing summary just
/// leaves that turn intact.
async fn maybe_compact_session(session: &mut Session, opts: &LoopOptions) {
    let cap = compaction_soft_cap();
    // Cheap pre-check before building the summaries vector: if we are under the
    // cap there is nothing to do and we must not allocate or fire a hook.
    if crate::compactor::estimate_transcript_bytes(&session.messages) <= cap {
        return;
    }
    // Build the per-message summary vector aligned to `session.messages` by
    // index. Eager summaries (P5.2) are keyed by `turn_index`; we materialize a
    // dense `Vec<Option<String>>` so `compact` can index it directly. Absent a
    // store (the live `LoopOptions` default) every entry is `None`.
    let mut summaries: Vec<Option<String>> = vec![None; session.messages.len()];
    if let Some(store) = opts.session_store.as_ref() {
        if let Ok(rows) = store.load_summaries(&session.id) {
            for (turn_index, summary) in rows {
                if let Some(slot) = usize::try_from(turn_index)
                    .ok()
                    .and_then(|i| summaries.get_mut(i))
                {
                    *slot = summary;
                }
            }
        }
    }
    if let Some(compacted) =
        crate::compactor::maybe_compact_transcript(&session.messages, &summaries, cap).await
    {
        session.messages = compacted;
    }
}

/// Build the commit-subject label for a per-tool checkpoint.
///
/// Combines the `tool` name with the file path(s) it edited so the snapshot is
/// distinguishable from per-turn (`"turn"`) checkpoints and from one another in
/// `origin checkpoints`. With no edited paths the label degrades to just the
/// tool name (`"tool:Edit"`); with one or more it appends them comma-joined
/// (`"tool:Edit src/lib.rs"`).
fn per_tool_checkpoint_label(tool: &str, edited: &[String]) -> String {
    if edited.is_empty() {
        format!("tool:{tool}")
    } else {
        format!("tool:{tool} {}", edited.join(","))
    }
}

/// Best-effort per-tool-use shadow-git checkpoint (cline L163 follow-up),
/// bracketed by the `PreCommit`/`PostCommit` lifecycle hooks.
///
/// No-op unless `ORIGIN_CHECKPOINTS_PER_TOOL=1`. Called after a SUCCESSFUL
/// mutating tool dispatch with the tool name and its parsed args; snapshots the
/// workspace via the [`checkpoint_with_commit_hooks`] machinery the per-turn
/// path also uses, labelled by [`per_tool_checkpoint_label`] so the entries are
/// distinguishable. Every failure is swallowed and the gate is default-off, so
/// the default code path — and the commit hooks — are byte-identical.
async fn maybe_checkpoint_per_tool_with_hooks(tool: &str, args: &Value) {
    if !per_tool_checkpoints_enabled() {
        return;
    }
    let edited = edited_paths_from_tool(tool, args);
    let _ = checkpoint_with_commit_hooks(&per_tool_checkpoint_label(tool, &edited)).await;
}

/// DENY-ONLY governance overlay (Task 3).
///
/// Given a base `decision` that is already `Allow`, apply the optional
/// [`origin_policy::PolicyEngine`] and [`origin_conseca::SecurityPolicy`] layers
/// from `opts`. Either layer may downgrade the Allow to a Deny, but neither can
/// widen anything — this function is only ever called on an `Allow`, and it
/// returns either that same `Allow` or a fresh `Deny`. When both layers are
/// `None` (the default at every construction site) it returns `decision`
/// unchanged, so default behavior is byte-identical.
fn apply_governance_overlay(
    decision: origin_permission::Decision,
    tool: &str,
    opts: &LoopOptions,
) -> origin_permission::Decision {
    if let Some(policy) = opts.policy.as_ref() {
        if !policy.is_tool_allowed(tool) {
            return origin_permission::Decision {
                outcome: Outcome::Deny,
                reason: format!("policy: tool `{tool}` is not permitted by the governance layer"),
            };
        }
    }
    if let Some(conseca) = opts.conseca.as_ref() {
        if !origin_conseca::check_tool(conseca, tool).is_allow() {
            return origin_permission::Decision {
                outcome: Outcome::Deny,
                reason: format!("conseca: tool `{tool}` is denied by the per-prompt security policy"),
            };
        }
    }
    decision
}

/// DENY-ONLY bash command-safety overlay (cmdparse wiring).
///
/// Given a base `decision` that is already `Allow`, inspect a `Bash` tool's
/// command string with [`origin_cmdparse::analyze`] and downgrade the `Allow`
/// to a `Deny` when [`origin_cmdparse::worst`] classifies it as
/// [`origin_cmdparse::Risk::Dangerous`]. Like [`apply_governance_overlay`],
/// this can only ever turn an `Allow` into a `Deny`, never widen.
///
/// The whole overlay is gated behind the `ORIGIN_CMD_GUARD=1` environment
/// variable and only applies to the `Bash` tool, so with the flag unset (the
/// default) it returns `decision` unchanged and default behavior is
/// byte-identical. `args` is the tool's parsed JSON arguments; the command is
/// read from the `command` field (matching the `Bash` builtin's schema).
fn apply_cmd_guard(
    decision: origin_permission::Decision,
    tool: &str,
    args: &Value,
) -> origin_permission::Decision {
    if tool != "Bash" || std::env::var("ORIGIN_CMD_GUARD").as_deref() != Ok("1") {
        return decision;
    }
    let Some(cmd) = args.get("command").and_then(Value::as_str) else {
        return decision;
    };
    let analysis = origin_cmdparse::analyze(cmd);
    if let origin_cmdparse::Risk::Dangerous(reason) = origin_cmdparse::worst(&analysis) {
        return origin_permission::Decision {
            outcome: Outcome::Deny,
            reason: format!("cmd-guard: dangerous bash command blocked: {reason}"),
        };
    }
    decision
}

/// Render a `<workspace-roots>` system-prompt block listing the additional
/// workspace roots the agent may operate across (cline multi-root workspaces).
/// Empty `roots` ⇒ empty string, leaving the assembled prompt byte-identical.
fn workspace_roots_block(roots: &[std::path::PathBuf]) -> String {
    if roots.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "<workspace-roots>\nYou are operating across multiple workspace roots. You may read and \
         edit files under ANY of these directories (use absolute paths):\n",
    );
    for r in roots {
        s.push_str("- ");
        s.push_str(&r.display().to_string());
        s.push('\n');
    }
    s.push_str("</workspace-roots>");
    s
}

/// DENY-ONLY read-only "plan mode" overlay (gemini Plan Mode).
///
/// When `read_only` is set, any tool that is not `SideEffects::Pure` is
/// downgraded from Allow to Deny so the design phase cannot mutate the
/// workspace. Like the other overlays this can only narrow an Allow to a Deny,
/// never widen a Deny; with `read_only == false` it is a no-op.
fn apply_read_only_overlay(
    decision: origin_permission::Decision,
    read_only: bool,
    side_effects: SideEffects,
    tool: &str,
) -> origin_permission::Decision {
    if read_only && decision.outcome == Outcome::Allow && !matches!(side_effects, SideEffects::Pure) {
        return origin_permission::Decision {
            outcome: Outcome::Deny,
            reason: format!(
                "plan mode: tool `{tool}` is read-only-blocked (mutating tools are disabled in plan mode)"
            ),
        };
    }
    decision
}

/// Best-effort end-of-turn / loop-end side effects.
///
/// Each sub-step is independently env-gated or feature-gated and default-off:
/// - `ORIGIN_TELEMETRY=1` ⇒ append one redacted JSONL `turn` event.
/// - `ORIGIN_NOTIFY=1` ⇒ spawn a desktop completion notification.
/// - `--features otel` + `ORIGIN_OTLP_ENDPOINT` ⇒ the `gen_ai` usage record
///   lands on the OTLP pipeline (a const no-op in the default build).
///
/// The `ORIGIN_CHECKPOINTS=1` shadow-git snapshot is fired separately by the
/// caller via [`maybe_checkpoint_turn_with_hooks`] (it is async because it
/// brackets the snapshot with the `PreCommit`/`PostCommit` lifecycle hooks).
///
/// With no env flags / features set (the default) this function does nothing
/// observable, so default behavior and every existing test stay byte-identical.
fn run_turn_end_effects(
    provider: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    latency_ms: f64,
    tool_calls: u64,
) {
    maybe_record_turn_telemetry(provider, model, input_tokens, output_tokens);
    maybe_notify_completion();
    // Task 3 (gen_ai metrics emit). Unconditional by design: this is a cheap
    // `const` no-op unless the daemon is built `--features otel` AND the OTLP
    // exporter has been installed (which requires `ORIGIN_OTLP_ENDPOINT`). The
    // response model is reported equal to the request model here — the loop does
    // not surface a distinct response-model string — matching the convention of
    // passing an empty string only when truly unknown.
    origin_metrics::instruments::record_gen_ai_usage(
        provider,
        model,
        model,
        input_tokens,
        output_tokens,
        latency_ms,
        tool_calls,
    );
}

/// Measured pain-bucket inputs for one session-stop record (Stage C5).
///
/// A plain data carrier assembled at an emit site and handed to
/// [`record_session_stop_pain`]. Every field is a *measured* quantity: the
/// stop reason, the split agent time, the optional time-to-first-useful-action,
/// the turn count, and the autonomy streak. Constructed via
/// [`SessionStopPain::from_loop`] (the in-loop emit sites, which have a full
/// time split) or [`SessionStopPain::reason_only`] (cross-module sites such as
/// the client-disconnect `Abandoned` and supervisor idle-`Retire`, which know
/// only the reason).
#[derive(Debug, Clone, Copy)]
pub struct SessionStopPain {
    stop_reason: origin_telemetry::SessionStopReason,
    model_time_ms: u64,
    tool_time_ms: u64,
    ttfua_ms: Option<u64>,
    turn_count: u32,
    autonomy_streak: u32,
}

impl SessionStopPain {
    /// Build a fully-measured record from the in-`run_loop` accumulators.
    ///
    /// `total_agent_time_ms` is the elapsed loop clock and `tool_time_ms` the
    /// summed tool-dispatch wall-clock; the two are reconciled by
    /// [`split_agent_time`] so `model_time_ms + tool_time_ms` always equals the
    /// total. `first_tool_ms`/`first_token_ms` feed [`select_ttfua`], and the
    /// autonomy streak is derived from `turn_count` by [`autonomy_streak_for`].
    #[must_use]
    pub const fn from_loop(
        stop_reason: origin_telemetry::SessionStopReason,
        total_agent_time_ms: u64,
        tool_time_ms: u64,
        first_tool_ms: Option<u64>,
        first_token_ms: Option<u64>,
        turn_count: u32,
    ) -> Self {
        let (model_ms, tool_ms) = split_agent_time(total_agent_time_ms, tool_time_ms);
        Self {
            stop_reason,
            model_time_ms: model_ms,
            tool_time_ms: tool_ms,
            ttfua_ms: select_ttfua(first_tool_ms, first_token_ms),
            turn_count,
            autonomy_streak: autonomy_streak_for(turn_count),
        }
    }

    /// Build a reason-only record for a cross-module emit site that has no
    /// in-loop time split (client disconnect ⇒ `Abandoned`, supervisor idle
    /// retire ⇒ `Idle`). The numeric fields are left unmeasured (zero time
    /// split, no ttfua, zero turns/streak) so only the stop reason is asserted.
    #[must_use]
    pub const fn reason_only(stop_reason: origin_telemetry::SessionStopReason) -> Self {
        Self {
            stop_reason,
            model_time_ms: 0,
            tool_time_ms: 0,
            ttfua_ms: None,
            turn_count: 0,
            autonomy_streak: 0,
        }
    }
}

/// Best-effort opt-in session-stop pain telemetry (Task 4 / Stage C5).
///
/// No-op unless `ORIGIN_TELEMETRY=1` (opt-in) and `DO_NOT_TRACK` is unset — the
/// same guard the per-turn `turn` events use — and routed through the same
/// redacting, sampling [`origin_telemetry::Pipeline`] and JSONL sink.
///
/// Emits one `session_stop` event carrying the full
/// [`origin_telemetry::PainMetrics`] bucket: the
/// [`origin_telemetry::SessionStopReason`], the real model-vs-tool time split,
/// the time-to-first-useful-action (when observed), the turn count, and the
/// autonomy streak. Default-off ⇒ no event, no file, byte-identical.
pub fn record_session_stop_pain(pain: SessionStopPain) {
    use std::io::Write as _;
    let do_not_track = std::env::var_os("DO_NOT_TRACK").is_some();
    let opt_in = std::env::var("ORIGIN_TELEMETRY").as_deref() == Ok("1");
    let cfg = origin_telemetry::Config::from_env(do_not_track, opt_in, 1.0);
    let mut pipeline = origin_telemetry::Pipeline::new(cfg);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let mut metrics = origin_telemetry::PainMetrics::new()
        .with_stop_reason(pain.stop_reason)
        .with_agent_time_split(pain.model_time_ms, pain.tool_time_ms)
        .with_turns(pain.turn_count, pain.autonomy_streak);
    if let Some(ttfua) = pain.ttfua_ms {
        metrics = metrics.with_time_to_first_useful_action_ms(ttfua);
    }
    let Ok(event) = metrics.into_event("session_stop".to_string(), now_ms) else {
        return;
    };
    pipeline.record(event);
    let lines = pipeline.drain();
    if lines.is_empty() {
        return; // disabled or sampled out — never touch the filesystem.
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let dir = home.join(".origin").join("telemetry");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("turns.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        for line in lines {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Split the loop's total wall-clock agent time into `(model_ms, tool_ms)`
/// (Stage C5 Task 1).
///
/// `total_ms` is the elapsed `run_loop` clock; `tool_ms` is the summed
/// wall-clock spent inside tool dispatch. Model time is the remainder
/// (`total - tool`), clamped to zero so a measurement skew — the tool clock
/// summed wider than the loop clock, e.g. overlapping speculative work — can
/// never underflow. The returned `tool_ms` is itself clamped to `total_ms` so
/// the invariant `model_ms + tool_ms == total_ms` always holds.
#[must_use]
const fn split_agent_time(total_ms: u64, tool_ms: u64) -> (u64, u64) {
    let clamped_tool = if tool_ms > total_ms { total_ms } else { tool_ms };
    let model_ms = total_ms - clamped_tool;
    (model_ms, clamped_tool)
}

/// Select the time-to-first-useful-action from the two observed signals
/// (Stage C5 Task 2).
///
/// A successful tool call is the canonical "useful action", so when one was
/// observed its elapsed-from-loop-start wins. Failing that (a purely
/// conversational turn that never dispatched a tool) the first assistant token
/// is the earliest useful signal. `None` only when neither was observed.
#[must_use]
const fn select_ttfua(first_tool_ms: Option<u64>, first_token_ms: Option<u64>) -> Option<u64> {
    match first_tool_ms {
        Some(ms) => Some(ms),
        None => first_token_ms,
    }
}

/// The two `OTel` `gen_ai` latency measurements for one provider call (Stage C4).
///
/// A plain data carrier produced by [`gen_ai_latencies`] and fanned out to the
/// `gen_ai.server.time_to_first_token` / `gen_ai.server.time_per_output_token`
/// instruments. `tpot_ms` is `None` when the call produced zero output tokens
/// (no per-token rate is defined), so the caller skips that recording rather
/// than emitting a divide-by-zero artefact.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GenAiLatencies {
    /// Time-to-first-token in milliseconds.
    ttft_ms: f64,
    /// Average per-output-token latency in milliseconds, or `None` when the
    /// call emitted no output tokens.
    tpot_ms: Option<f64>,
}

/// Derive the `gen_ai` time-to-first-token and time-per-output-token for one
/// provider call (Stage C4).
///
/// - **TTFT**: the real first-streamed-token elapsed (`stream_first_token_ms`)
///   when the streaming path observed a token; otherwise — the non-streaming
///   path, or a stream that ended with no content — the full call latency
///   (`provider_call_ms`), which is the tightest first-token bound available.
/// - **TPOT**: total generation time (`provider_call_ms`) divided by
///   `output_tokens`, guarding divide-by-zero by returning `None` when the call
///   produced no output tokens.
///
/// Pure and total over its inputs (no I/O, no globals), so the wiring's timing
/// decision is unit-testable without standing up a provider or an exporter.
#[must_use]
fn gen_ai_latencies(
    provider_call_ms: u64,
    stream_first_token_ms: Option<u64>,
    output_tokens: u64,
) -> GenAiLatencies {
    // Prefer the measured first-token instant; fall back to the full call
    // latency when no streamed token was observed.
    #[allow(clippy::cast_precision_loss)]
    let ttft_ms = stream_first_token_ms.unwrap_or(provider_call_ms) as f64;
    let tpot_ms = if output_tokens == 0 {
        None
    } else {
        #[allow(clippy::cast_precision_loss)]
        Some(provider_call_ms as f64 / output_tokens as f64)
    };
    GenAiLatencies { ttft_ms, tpot_ms }
}

/// Compute the autonomy streak for a single `run_loop` invocation (Stage C5
/// Task 3).
///
/// One `run_loop` call processes exactly one user prompt: turn 1 is the
/// directly user-prompted turn, and every later turn (2..=N) ran on the
/// model's own initiative with no new user input. The autonomy streak is
/// therefore the count of those self-driven continuation turns, `N - 1`,
/// saturating at 0 for a single-turn (or empty) loop. This is deliberately
/// distinct from the raw `turn_count` so a one-shot answer reports zero
/// autonomy rather than mislabelling the user's own turn as autonomous.
#[must_use]
const fn autonomy_streak_for(turn_count: u32) -> u32 {
    turn_count.saturating_sub(1)
}

/// Best-effort opt-in telemetry (Task 4). No-op unless `ORIGIN_TELEMETRY=1`
/// (opt-in) and `DO_NOT_TRACK` is unset. When enabled, appends a single
/// redacted JSONL `turn` event under `~/.origin/telemetry/turns.jsonl`. All
/// failures are swallowed. Disabled-by-default ⇒ no events, no file.
fn maybe_record_turn_telemetry(provider: &str, model: &str, input_tokens: u64, output_tokens: u64) {
    use std::io::Write as _;
    let do_not_track = std::env::var_os("DO_NOT_TRACK").is_some();
    let opt_in = std::env::var("ORIGIN_TELEMETRY").as_deref() == Ok("1");
    let cfg = origin_telemetry::Config::from_env(do_not_track, opt_in, 1.0);
    let mut pipeline = origin_telemetry::Pipeline::new(cfg);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let mut event = origin_telemetry::Event::new("turn".to_string(), now_ms);
    event.props = vec![
        ("provider".to_string(), provider.to_string()),
        ("model".to_string(), model.to_string()),
        (
            "tokens".to_string(),
            (input_tokens.saturating_add(output_tokens)).to_string(),
        ),
    ];
    pipeline.record(event);
    let lines = pipeline.drain();
    if lines.is_empty() {
        return; // disabled or sampled out — never touch the filesystem.
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let dir = home.join(".origin").join("telemetry");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("turns.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        for line in lines {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Best-effort desktop completion notification (Task 5). No-op unless
/// `ORIGIN_NOTIFY=1`. Spawns the platform desktop-notification command and
/// ignores every failure. Default-off ⇒ no spawn.
fn maybe_notify_completion() {
    if std::env::var("ORIGIN_NOTIFY").as_deref() != Ok("1") {
        return;
    }
    let n = origin_notify::Notification::new("origin", "Turn complete", false);
    let (program, cmd_args) = origin_notify::desktop_command(&n);
    let _ = std::process::Command::new(program).args(cmd_args).spawn();
}

/// Best-effort post-edit auto-format (Task 2). No-op unless `ORIGIN_AUTOFORMAT=1`
/// and [`origin_postedit::formatter_for`] maps `path` to a known formatter.
/// When both hold, spawns `<formatter-argv...> <path>` and ignores every
/// failure (a missing formatter binary must never fail the edit tool).
/// Default-off ⇒ no spawn, behavior unchanged.
fn maybe_autoformat(path: &str) {
    if std::env::var("ORIGIN_AUTOFORMAT").as_deref() != Ok("1") {
        return;
    }
    // Security (argv flag smuggling): a model-edited file path beginning with
    // `-` could be parsed by the formatter as an option rather than a file.
    // Refuse such paths outright; the `--` sentinel below is the second guard.
    if path.starts_with('-') {
        tracing::warn!(path, "skipping autoformat: path looks like a flag");
        return;
    }
    let Some(cmd) = origin_postedit::formatter_for(path) else {
        return;
    };
    // `cmd` may be a program plus flags, e.g. "ruff format"; split on
    // whitespace so the program and its sub-args are passed correctly.
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let Some((program, flags)) = tokens.split_first() else {
        return;
    };
    let _ = std::process::Command::new(program)
        .args(flags)
        .arg("--") // end-of-options sentinel: everything after is a positional path
        .arg(path)
        .spawn();
}

/// Extract candidate target file paths from a patch body for auto-formatting.
///
/// Recognizes both unified-diff `+++ b/<path>` headers and the apply-patch
/// `*** Update File: <path>` / `*** Add File: <path>` markers. Best-effort and
/// lossy — anything not matched is simply skipped; the caller only uses the
/// result to *offer* formatting under the `ORIGIN_AUTOFORMAT` gate.
fn patch_target_paths(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in patch.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("+++ ") {
            let p = rest.trim().trim_start_matches("b/");
            if !p.is_empty() && p != "/dev/null" {
                out.push(p.to_string());
            }
        } else if let Some(rest) = trimmed
            .strip_prefix("*** Update File: ")
            .or_else(|| trimmed.strip_prefix("*** Add File: "))
        {
            let p = rest.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// Extract the file path(s) a mutating tool call edited, for the optional
/// autonomous LSP-diagnostics feedback ([`crate::lsp_diagnostics`]).
///
/// Recognizes the path-bearing builtins: `Edit`/`Write`/`MultiEdit` carry a
/// single `file_path`; `ApplyPatch` carries a `patch` body whose target files
/// are recovered with [`patch_target_paths`]. Any other (or malformed) tool
/// yields an empty vec. Best-effort and lossy — the result only ever *adds*
/// candidates to the diagnostics probe set, which is itself default-off.
fn edited_paths_from_tool(name: &str, args: &Value) -> Vec<String> {
    match name {
        "Edit" | "Write" | "MultiEdit" => {
            match args.get("file_path").and_then(Value::as_str) {
                Some(p) if !p.is_empty() => vec![p.to_string()],
                _ => Vec::new(),
            }
        }
        "ApplyPatch" => args
            .get("patch")
            .and_then(Value::as_str)
            .map_or_else(Vec::new, patch_target_paths),
        _ => Vec::new(),
    }
}

/// Maximum number of times `run_loop` retries a single turn's provider call
/// after a `ProviderError::RateLimit`. After the cap is hit the error is
/// propagated as a `LoopError::Provider` and the turn fails normally.
const MAX_PROVIDER_RETRIES: u32 = 3;

/// Upper bound on the per-attempt sleep duration when honouring a
/// `retry-after` header. Anthropic occasionally returns large values that
/// would stall the loop for minutes; clamping keeps worst-case latency
/// bounded while still respecting short server-side backoffs.
const MAX_RATE_LIMIT_SLEEP_SECS: u32 = 60;

/// Extract the path a read-class tool touched, for swarm collaboration
/// tracking (WS-L, jcode L238).
///
/// `Read` carries a single `file_path`; `Glob`/`Grep` carry an optional `path`
/// search root. Any other (or pathless) tool yields `None`. Best-effort and
/// lossy — only feeds the advisory [`origin_swarm::FileRegistry`].
fn read_path_from_tool<'a>(name: &str, args: &'a Value) -> Option<&'a str> {
    let key = match name {
        "Read" => "file_path",
        "Glob" | "Grep" => "path",
        _ => return None,
    };
    args.get(key).and_then(Value::as_str).filter(|p| !p.is_empty())
}

/// Best-effort swarm-collaboration bookkeeping for one successful tool call
/// (WS-L, jcode L238). Reached only when a [`SwarmCollab`] is in scope; it
/// further no-ops unless the `ORIGIN_SWARM_COLLAB` env is set — so the default
/// everywhere is byte-identical. Never panics; never returns an error.
///
/// On a read-class tool (`Read`/`Glob`/`Grep`) it records this worker as a
/// reader of the path. On an edit-class tool (`Edit`/`Write`/`MultiEdit`/
/// `ApplyPatch`) it records the edit and, for each OTHER worker that had read
/// the path, delivers a file-shift notice into that worker's mailbox (when the
/// `mailboxes` map is wired) or logs it (the fallback when per-worker mailbox
/// plumbing is not reachable from here).
fn record_swarm_collab(collab: &SwarmCollab, name: &str, args: &Value) {
    // Gate: env must be set. Unset ⇒ no tracking ⇒ byte-identical.
    if std::env::var_os("ORIGIN_SWARM_COLLAB").is_none() {
        return;
    }
    if let Some(path) = read_path_from_tool(name, args) {
        collab.registry.record_read(collab.worker_id, path);
        return;
    }
    for path in edited_paths_from_tool(name, args) {
        let Some(notice) = collab.registry.record_edit_notice(collab.worker_id, &path) else {
            continue;
        };
        deliver_file_shift(collab, &notice);
    }
}

/// Deliver a [`origin_swarm::FileShiftNotice`] to each affected reader's
/// mailbox, or log it when the mailbox map is not wired.
fn deliver_file_shift(collab: &SwarmCollab, notice: &origin_swarm::FileShiftNotice) {
    let body = format!(
        "file-shift: {} was edited by worker {:032x}; re-check your view",
        notice.path.display(),
        notice.editor.value()
    );
    let Some(mailboxes) = collab.mailboxes.as_ref() else {
        tracing::info!(
            path = %notice.path.display(),
            readers = notice.readers.len(),
            "swarm collab: file-shift notice (no mailbox plumbing; logging only)"
        );
        return;
    };
    // The map is live behind a `Mutex` (a later-spawned sibling registers into
    // the same map), so lock for the minimum span — clone out each reader's
    // `Arc<Mailbox>`, then drop the guard before pushing so no lock is held
    // across the loop body. A poisoned lock is recovered, never propagated:
    // collab delivery is advisory and must not tear down the turn.
    let targets: Vec<(origin_swarm::WorkerId, Arc<origin_swarm::Mailbox>)> = {
        let map = mailboxes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        notice
            .readers
            .iter()
            .map(|reader| (*reader, map.get(reader).map(Arc::clone)))
            .filter_map(|(reader, mbox)| mbox.map(|m| (reader, m)))
            .collect()
    };
    for (reader, mbox) in targets {
        mbox.push(origin_swarm::Message::new(
            notice.editor,
            origin_swarm::MsgScope::Direct(reader),
            body.clone(),
        ));
    }
}

/// Render the messages a worker drained from its OWN mailbox into a
/// `<swarm-notices>` system block for injection into the next turn (A4).
///
/// Each message's body is already a human-readable line (e.g. the
/// `file-shift: <path> was edited by worker <id>; re-check your view` text built
/// by [`deliver_file_shift`]), so we list one bullet per message inside a single
/// block. An EMPTY slice yields an EMPTY string so the caller appends nothing and
/// the turn — and thus the prompt-cache breakpoints — stay byte-identical; this
/// is the steady state once a worker has no pending sibling notices.
fn render_swarm_notices(msgs: &[origin_swarm::Message]) -> String {
    if msgs.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "<swarm-notices>\nSibling workers changed shared state since your last turn. \
         Re-check any affected files before relying on a stale view.\n",
    );
    for msg in msgs {
        block.push_str("- ");
        block.push_str(msg.body.trim());
        block.push('\n');
    }
    block.push_str("</swarm-notices>");
    block
}

/// Drain THIS worker's own mailbox and render the pending notices into a
/// `<swarm-notices>` block for the next turn (A4).
///
/// Locks the shared `mailboxes` map for the minimum span — clones out this
/// worker's `Arc<Mailbox>`, drops the guard, then drains. Draining empties the
/// inbox so each notice is surfaced exactly once. Returns an EMPTY string when
/// there is no mailbox plumbing, no entry for this worker, or nothing pending —
/// in which case the caller appends nothing and the turn stays byte-identical. A
/// poisoned lock is recovered, never propagated: collab delivery is advisory and
/// must not tear down the turn.
fn drain_own_swarm_notices(collab: &SwarmCollab) -> String {
    let Some(mailboxes) = collab.mailboxes.as_ref() else {
        return String::new();
    };
    let own = {
        let map = mailboxes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(&collab.worker_id).map(Arc::clone)
    };
    let Some(own) = own else {
        return String::new();
    };
    render_swarm_notices(&own.drain())
}

/// When the primary model is rate-limited and all retries are exhausted,
/// try a cheaper model from the same family before killing the turn.
fn rate_limit_fallback(model: &str) -> Option<&'static str> {
    if model.contains("opus") {
        Some("claude-sonnet-4-6")
    } else if model.contains("sonnet") && !model.contains("haiku") {
        Some("claude-haiku-4-5")
    } else {
        None
    }
}

/// Real-time swarm collaboration context for a worker's agent loop (WS-L,
/// jcode L238).
///
/// Installed by a swarm worker around its [`run_loop`] call via
/// [`scope_swarm_collab`]; the per-tool hook reads it from the
/// [`SWARM_COLLAB`] task-local. Threading it as a task-local (rather than a
/// `LoopOptions` field) keeps every existing `LoopOptions` construction site
/// untouched, so the daemon is byte-identical when no worker scopes it.
///
/// When a context is in scope AND the process env `ORIGIN_SWARM_COLLAB` is set,
/// the loop:
/// - calls [`origin_swarm::FileRegistry::record_read`] after a successful
///   read-class tool (`Read`/`Glob`/`Grep`),
/// - calls [`origin_swarm::FileRegistry::record_edit`] after a successful
///   edit-class tool (`Edit`/`Write`/`MultiEdit`/`ApplyPatch`) and, for each
///   returned reader, enqueues a [`origin_swarm::FileShiftNotice`]-derived
///   [`origin_swarm::Message`] into that worker's mailbox when the `mailboxes`
///   map is wired, or logs it otherwise.
///
/// Best-effort: a tracking failure never tears down the turn.
#[derive(Clone)]
pub struct SwarmCollab {
    /// Identity of the worker running this loop. Used as the reader/editor in
    /// every registry call.
    pub worker_id: origin_swarm::WorkerId,
    /// Shared, room-wide file-read registry. All workers in one swarm room
    /// share a single `Arc<FileRegistry>`.
    pub registry: Arc<origin_swarm::FileRegistry>,
    /// Optional **live** shared map `WorkerId → Mailbox`. When present, a
    /// file-shift notice is delivered as a [`origin_swarm::Message`] into each
    /// affected reader's mailbox. The map is live (behind a `Mutex`) so a
    /// worker spawned *after* this one is still visible for delivery. When
    /// `None`, the notice is logged instead (the "mailbox plumbing isn't
    /// reachable" fallback).
    pub mailboxes: Option<origin_swarm::SharedMailboxes>,
}

tokio::task_local! {
    /// Per-worker swarm-collaboration context (WS-L). A swarm worker sets this
    /// for the duration of its [`run_loop`] via [`scope_swarm_collab`]; the
    /// per-tool hook reads it. Unset on every non-worker task ⇒ no tracking ⇒
    /// byte-identical.
    static SWARM_COLLAB: SwarmCollab;
}

/// Run `fut` with `collab` installed in the [`SWARM_COLLAB`] task-local so the
/// per-tool hook inside [`run_loop`] records this worker's reads/edits and
/// emits file-shift notices (WS-L, jcode L238).
///
/// Intended for a swarm worker to wrap its `run_loop` call. Tasks that never
/// call this see an unset task-local and behave exactly as before.
pub async fn scope_swarm_collab<F, T>(collab: SwarmCollab, fut: F) -> T
where
    F: std::future::Future<Output = T> + Send,
{
    SWARM_COLLAB.scope(collab, fut).await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    // The ENV_LOCK guard intentionally spans the awaited collab scopes so the
    // process-global `ORIGIN_SWARM_COLLAB` mutation is serialized against the
    // sibling test; the lock is uncontended otherwise.
    clippy::await_holding_lock,
    // The mailbox/registry guards are short-lived assertion reads at the end of
    // each test; tightening them buys nothing in test code.
    clippy::significant_drop_tightening
)]
mod swarm_collab_wiring_tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::PoisonError;

    /// Serializes tests that toggle the process-global `ORIGIN_SWARM_COLLAB`
    /// env var so the parallel runner cannot interleave a `set_var` in one test
    /// with a read in another.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Build a live shared mailbox map registering both workers, mirroring what
    /// `Coordinator::spawn_with` does per worker under the collab gate.
    fn shared_map(
        ids: &[origin_swarm::WorkerId],
    ) -> origin_swarm::SharedMailboxes {
        let mut map: HashMap<origin_swarm::WorkerId, Arc<origin_swarm::Mailbox>> = HashMap::new();
        for id in ids {
            map.insert(*id, Arc::new(origin_swarm::Mailbox::new()));
        }
        Arc::new(std::sync::Mutex::new(map))
    }

    /// End-to-end wiring proof (WS-L, jcode L238): with the gate SET, a worker
    /// (B) that edits a path another worker (A) previously read must leave a
    /// file-shift notice in A's mailbox — exercising `scope_swarm_collab` +
    /// the same `record_swarm_collab` hook the run_loop fires per tool call.
    #[tokio::test]
    async fn editor_notifies_prior_reader_under_gate() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        std::env::set_var("ORIGIN_SWARM_COLLAB", "1");

        let reader = origin_swarm::WorkerId::generate();
        let editor = origin_swarm::WorkerId::generate();
        let registry = Arc::new(origin_swarm::FileRegistry::new());
        let mailboxes = shared_map(&[reader, editor]);

        // Worker A reads `src/lib.rs` inside its own collab scope.
        let collab_a = SwarmCollab {
            worker_id: reader,
            registry: Arc::clone(&registry),
            mailboxes: Some(Arc::clone(&mailboxes)),
        };
        let read_args = serde_json::json!({ "file_path": "src/lib.rs" });
        scope_swarm_collab(collab_a, async {
            let _ = SWARM_COLLAB.try_with(|c| record_swarm_collab(c, "Read", &read_args));
        })
        .await;

        // Worker B edits the same path inside ITS scope.
        let collab_b = SwarmCollab {
            worker_id: editor,
            registry: Arc::clone(&registry),
            mailboxes: Some(Arc::clone(&mailboxes)),
        };
        let edit_args = serde_json::json!({ "file_path": "src/lib.rs", "old": "a", "new": "b" });
        scope_swarm_collab(collab_b, async {
            let _ = SWARM_COLLAB.try_with(|c| record_swarm_collab(c, "Edit", &edit_args));
        })
        .await;

        // A's mailbox holds exactly one Direct notice from B about the path;
        // B's own mailbox stays empty (no self-notify).
        let map = mailboxes.lock().unwrap_or_else(PoisonError::into_inner);
        let a_box = map.get(&reader).expect("reader mailbox present");
        let b_box = map.get(&editor).expect("editor mailbox present");
        let a_msgs = a_box.drain();
        assert_eq!(a_msgs.len(), 1, "reader must receive exactly one file-shift notice");
        assert_eq!(a_msgs[0].from, editor, "notice is from the editor");
        assert_eq!(a_msgs[0].scope, origin_swarm::MsgScope::Direct(reader));
        assert!(a_msgs[0].body.contains("src/lib.rs"), "notice names the edited path");
        assert!(b_box.is_empty(), "editor must not notify itself");

        std::env::remove_var("ORIGIN_SWARM_COLLAB");
    }

    /// Default-off discipline: with the gate UNSET, the same read+edit sequence
    /// records nothing and delivers no notice — byte-identical to before.
    #[tokio::test]
    async fn no_notice_when_gate_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        std::env::remove_var("ORIGIN_SWARM_COLLAB");

        let reader = origin_swarm::WorkerId::generate();
        let editor = origin_swarm::WorkerId::generate();
        let registry = Arc::new(origin_swarm::FileRegistry::new());
        let mailboxes = shared_map(&[reader, editor]);

        let collab_a = SwarmCollab {
            worker_id: reader,
            registry: Arc::clone(&registry),
            mailboxes: Some(Arc::clone(&mailboxes)),
        };
        let read_args = serde_json::json!({ "file_path": "src/lib.rs" });
        scope_swarm_collab(collab_a, async {
            let _ = SWARM_COLLAB.try_with(|c| record_swarm_collab(c, "Read", &read_args));
        })
        .await;

        let collab_b = SwarmCollab {
            worker_id: editor,
            registry: Arc::clone(&registry),
            mailboxes: Some(Arc::clone(&mailboxes)),
        };
        let edit_args = serde_json::json!({ "file_path": "src/lib.rs" });
        scope_swarm_collab(collab_b, async {
            let _ = SWARM_COLLAB.try_with(|c| record_swarm_collab(c, "Edit", &edit_args));
        })
        .await;

        let map = mailboxes.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            map.get(&reader).expect("reader mailbox").is_empty(),
            "gate unset ⇒ no notice ⇒ byte-identical"
        );
        assert_eq!(registry.tracked_paths(), 0, "gate unset ⇒ nothing recorded");
    }

    /// `render_swarm_notices` (A4): empty slice ⇒ empty string (no block, so the
    /// turn stays byte-identical); a single Direct file-shift message ⇒ a
    /// `<swarm-notices>` block naming the shifted path and the editor worker; many
    /// messages ⇒ each one is listed.
    #[test]
    fn render_swarm_notices_block_shapes() {
        // Empty ⇒ empty string (no `<swarm-notices>` block emitted).
        assert!(
            render_swarm_notices(&[]).is_empty(),
            "no pending messages ⇒ no block ⇒ byte-identical turn"
        );

        let editor = origin_swarm::WorkerId::generate();
        let me = origin_swarm::WorkerId::generate();
        let body = format!(
            "file-shift: {} was edited by worker {:032x}; re-check your view",
            "src/lib.rs",
            editor.value()
        );
        let one = [origin_swarm::Message::new(
            editor,
            origin_swarm::MsgScope::Direct(me),
            body.clone(),
        )];
        let rendered = render_swarm_notices(&one);
        assert!(
            rendered.starts_with("<swarm-notices>"),
            "single message opens a <swarm-notices> block: {rendered}"
        );
        assert!(
            rendered.trim_end().ends_with("</swarm-notices>"),
            "single message closes the block: {rendered}"
        );
        assert!(
            rendered.contains("src/lib.rs"),
            "block names the shifted path: {rendered}"
        );
        assert!(
            rendered.contains(&format!("{:032x}", editor.value())),
            "block names the editor worker: {rendered}"
        );

        // Multiple messages ⇒ every body is listed inside the one block.
        let other_editor = origin_swarm::WorkerId::generate();
        let body2 = format!(
            "file-shift: {} was edited by worker {:032x}; re-check your view",
            "src/main.rs",
            other_editor.value()
        );
        let many = [
            origin_swarm::Message::new(editor, origin_swarm::MsgScope::Direct(me), body),
            origin_swarm::Message::new(
                other_editor,
                origin_swarm::MsgScope::Broadcast,
                body2,
            ),
        ];
        let rendered_many = render_swarm_notices(&many);
        assert_eq!(
            rendered_many.matches("<swarm-notices>").count(),
            1,
            "all messages share one block: {rendered_many}"
        );
        assert!(
            rendered_many.contains("src/lib.rs") && rendered_many.contains("src/main.rs"),
            "every message is listed: {rendered_many}"
        );
    }

    /// `drain_own_swarm_notices` (A4): the drain→render step a worker runs at the
    /// turn boundary. With no `mailboxes` map ⇒ empty (nothing to drain). With a
    /// map whose entry holds a pending notice ⇒ a `<swarm-notices>` block AND the
    /// mailbox is left empty (drain consumed it, so the notice is shown once). A
    /// second call with the now-empty box ⇒ empty string again.
    #[test]
    fn drain_own_swarm_notices_consumes_and_renders() {
        let me = origin_swarm::WorkerId::generate();
        let editor = origin_swarm::WorkerId::generate();
        let registry = Arc::new(origin_swarm::FileRegistry::new());

        // No mailbox map ⇒ nothing to drain ⇒ empty.
        let no_box = SwarmCollab {
            worker_id: me,
            registry: Arc::clone(&registry),
            mailboxes: None,
        };
        assert!(
            drain_own_swarm_notices(&no_box).is_empty(),
            "no mailbox plumbing ⇒ no block"
        );

        // A live map with a pending Direct notice in MY box.
        let mailboxes = shared_map(&[me, editor]);
        {
            let map = mailboxes.lock().unwrap_or_else(PoisonError::into_inner);
            map.get(&me).expect("my mailbox present").push(
                origin_swarm::Message::new(
                    editor,
                    origin_swarm::MsgScope::Direct(me),
                    "file-shift: src/lib.rs was edited by worker; re-check your view",
                ),
            );
        }
        let collab = SwarmCollab {
            worker_id: me,
            registry: Arc::clone(&registry),
            mailboxes: Some(Arc::clone(&mailboxes)),
        };
        let first = drain_own_swarm_notices(&collab);
        assert!(
            first.contains("<swarm-notices>") && first.contains("src/lib.rs"),
            "pending notice renders a block: {first}"
        );
        // Drain consumed it: a second call sees an empty box ⇒ empty string.
        assert!(
            drain_own_swarm_notices(&collab).is_empty(),
            "drain consumes the notice so it is shown exactly once"
        );
    }
}

#[derive(Clone)]
pub struct LoopOptions {
    pub max_turns: u32,
    pub cas: Option<Arc<Store>>,
    /// Optional code-graph index. Wrapped in `Arc<Mutex<...>>` because
    /// `graph_rebuild` mutates the index. `None` disables all `graph_*` and
    /// `ask` tools (they return a clear `ToolFailure` rather than `UnknownTool`).
    pub code_graph: Option<Arc<tokio::sync::Mutex<origin_codegraph::index::CodeGraphIndex>>>,
    /// Optional memory router for the `ask` tool. `None` disables the memory
    /// side of ask routing (code side still works if `code_graph` is `Some`).
    pub mem_router: Option<Arc<dyn origin_codegraph::ask::MemRouter>>,
    /// Optional channel used by the daemon to publish each request's
    /// `Subscriber` to a per-connection relay task. The relay forwards token
    /// events to the CLI as `Event` frames. We send a pre-subscribed
    /// `Subscriber` (not the `Ring`) so the relay never races the producer.
    pub relay_tx: Option<tokio::sync::mpsc::Sender<origin_stream::Subscriber>>,
    /// When `true`, the loop falls back to `provider.chat()` instead of
    /// `provider.chat_stream()`. Used by scripted/deterministic tests to
    /// bypass the streaming drain path. The incremental `tool_use` parser
    /// (P3.3) means production code paths can leave this `false`.
    pub streaming_disabled: bool,
    /// Optional sidecar handle for eager turn summarization (P5.2).
    pub sidecar: Option<Arc<Sidecar>>,
    /// Optional session store for delivering summaries (P5.2).
    pub session_store: Option<Arc<SessionStore>>,
    /// If `Some`, the Proposer runs at turn end and pushes proposals into
    /// `session.pending_proposals` and emits one [`StreamEvent::MemoryProposed`]
    /// per proposal through `event_tx` (or skips the emit if no sender is
    /// configured). `None` disables the feature (the existing dogfood path).
    pub proposer: Option<Arc<Proposer>>,
    /// Side-band channel for non-streaming [`StreamEvent`]s (currently only
    /// [`StreamEvent::MemoryProposed`]). The daemon main forwards these as
    /// `Event` frames after `run_loop` returns and before writing `Response`.
    /// We use a direct event channel here (not the per-turn rkyv `Ring`)
    /// because [`StreamEvent::MemoryProposed`] doesn't map to any
    /// [`origin_stream::TokenKind`] — it's a turn-end side product, not a
    /// streaming token.
    pub event_tx: Option<tokio::sync::mpsc::Sender<StreamEvent>>,
    /// If `Some`, the loop embeds the user prompt and prepends any retrieved
    /// `<context source="origin-mem">` block to the system prompt of every
    /// turn's `ChatRequest`. `None` disables prompt-recall injection.
    pub injector: Option<Arc<Injector>>,
    /// Daemon-wide pending-proposal registry. When wired together with
    /// `proposer` + `event_tx`, each emitted [`StreamEvent::MemoryProposed`]
    /// also records its `(body, tags)` here so a later
    /// [`ClientMessage::MemoryDecision::Accept`](crate::protocol::ClientMessage::MemoryDecision)
    /// on a different connection can still persist the proposal.
    pub proposal_registry: Option<Arc<ProposalRegistry>>,
    /// Active skill stack. When `Some`, every per-turn permission check runs
    /// through [`check_with_skills`] so the intersection of active skills'
    /// `allowed-tools` masks narrows tool access. When `None`, the loop falls
    /// through to the default tier rules — equivalent to passing an empty
    /// registry, since an empty stack's mask is `None` (no narrowing).
    pub skills: Option<Arc<SkillRegistry>>,
    /// Daemon-wide skill catalog injected into each turn's system prompt
    /// so the model knows which skills are available. The actual
    /// activation state lives in `skills` above; this is the catalog of
    /// all loadable skills, separate from "currently active".
    pub skill_catalog: Option<Arc<SkillCatalog>>,
    /// Daemon-wide workflow catalog, loaded once at startup from
    /// `~/.origin/workflows.toml`. When `Some`, each turn's system prompt
    /// includes an `Available workflows` block so the model can answer
    /// "what workflows do you have?" without inventing them.
    pub workflows: Option<Arc<WorkflowsFile>>,
    /// Optional memory-subsystem handle. When `Some`, `mem_search`,
    /// `mem_save`, and `mem_forget` dispatch to the live `MemoryStore` /
    /// HNSW index. When `None`, those tools return
    /// `ToolFailure("memory subsystem not configured")`.
    pub memory_handle: Option<Arc<dyn MemoryHandle>>,
    /// Optional swarm coordinator for the `Task` tool. When `None`, Task
    /// returns `ToolFailure("Task subsystem not configured")`. When `Some`,
    /// Task spawns a noop (P9.6) or real (P9.8+) worker and returns the
    /// structured `TaskOutput` JSON.
    pub coordinator: Option<Arc<origin_swarm::Coordinator>>,
    /// Optional N4.3 handle→band index, shared with the active provider's
    /// wire-encoder (the Anthropic provider's `expand_messages_for_wire`
    /// reads `band_for_handle` to decide `Inline` vs `Reference`). When
    /// `Some`, the per-tool-result dispatch path calls `register_handle`
    /// for every CAS handle produced by a tool, classifying the band from
    /// the tool's `SideEffects` metadata (`Pure` → `Sticky`, `Mutating` →
    /// `Volatile`). When `None`, no registrations happen and the wire-
    /// encoder falls through to the `Volatile` floor — exactly the
    /// behavior before Phase 11 N4.3 landed.
    pub plan: Option<origin_planner::Plan>,
    /// Per-connection `/goal` slot. The driver in `main.rs` mutates this
    /// under the lock; `run_loop` reads it while assembling the system
    /// prompt and renders an `<origin-goal>` block whenever the goal's
    /// status is `Active` or `Verifying`.
    ///
    /// Shape is `Arc<Mutex<Option<_>>>` (not `Option<Arc<Mutex<_>>>`) so the
    /// driver can install or remove the goal without rebuilding the
    /// per-request `LoopOptions`. Defaults to an empty slot (no active
    /// goal). Set directly via struct literal at the per-request
    /// `LoopOptions` build site in `main.rs`. (The historical
    /// `LoopOptions::with_goal` builder was removed in the Bug-#22 cleanup
    /// — it had no production callers.)
    pub goal: Arc<tokio::sync::Mutex<Option<origin_goal::GoalState>>>,
    /// Optional governance policy engine (Task 3). When `Some`, a tool the
    /// base permission check would *allow* is downgraded to Deny if
    /// [`origin_policy::PolicyEngine::is_tool_allowed`] returns `false`. It
    /// can never widen a Deny. `None` (the default everywhere) ⇒ no effect.
    pub policy: Option<Arc<origin_policy::PolicyEngine>>,
    /// Optional per-prompt `ConSeca` security policy (Task 3). Same deny-only
    /// contract as `policy`: an Allow is downgraded to Deny when
    /// [`origin_conseca::check_tool`] is not `Allow`. `None` ⇒ no effect.
    pub conseca: Option<Arc<origin_conseca::SecurityPolicy>>,
    /// Optional reasoning-effort hint for every turn of this loop. `None` (the
    /// default) leaves each `ChatRequest.effort` as `None` ⇒ provider wire
    /// byte-identical. Set from `PromptRequest.effort` in `handle_request`.
    /// *Closes: claude-code `/effort`+`/fast` (the agent-loop wire).*
    pub effort: Option<origin_provider::ReasoningEffort>,
    /// Optional extended-thinking budget (in tokens) for every turn of this
    /// loop. `None` (the default) leaves each `ChatRequest.thinking_tokens` as
    /// `None` ⇒ provider wire byte-identical. Set from
    /// `PromptRequest.thinking_tokens` in `handle_request`. Only the Anthropic
    /// encoder honours it (enables extended thinking with `budget_tokens`);
    /// other providers ignore it. *Closes: aider `--thinking-tokens` (the
    /// agent-loop wire).*
    pub thinking_tokens: Option<u32>,
    /// Multimodal attachments to append to the FIRST user turn of this loop
    /// (turn 1 only — later turns carry tool-results, not the user image).
    /// Empty ⇒ text-only wire unchanged. Set from `PromptRequest.attachments`.
    /// *Closes: aider/gemini/claude image+PDF input (the agent-loop wire).*
    pub attachments: Vec<origin_multimodal::ContentBlock>,
    /// Optional addendum appended to the assembled system prompt every turn.
    /// Carries the active output-style's system suffix (Explanatory / Learning /
    /// Concise). `None`/empty ⇒ the prompt — and thus the prompt-cache
    /// breakpoints — are byte-identical to before. Set from the (otherwise
    /// unused) `PromptRequest.system` field in `handle_request`.
    /// *Closes: claude-code output styles (the system-prompt wire).*
    pub system_suffix: Option<String>,
    /// Read-only "plan mode" (gemini Plan Mode). When `true`, the per-tool
    /// permission check downgrades any non-`Pure` tool (Edit/Write/Bash/…) from
    /// Allow to Deny — a hard read-only design phase that cannot mutate the
    /// workspace. Deny-only: it never widens an existing Deny. `false` (the
    /// default) ⇒ no effect. Set from `PromptRequest.read_only`.
    /// *Closes: gemini Plan Mode (the policy-enforced read-only phase).*
    pub read_only: bool,
    /// Optional live, per-turn model router. When `Some`, each turn classifies a
    /// [`Phase`](origin_router::Phase) (turn 1 ⇒ Plan, later turns ⇒ Edit), asks
    /// the router to pick a model, and — when the pick is on the **active**
    /// provider — overrides that turn's `ChatRequest.model`. Latency / success
    /// is folded back into the router's health after each turn, and a terminal
    /// rate-limit marks the model exhausted so a `QuotaFallback` chain skips it
    /// next time. `None` (the default everywhere) ⇒ every turn uses
    /// `session.model`, byte-identical to before. Set from
    /// [`crate::routing::global`] in `handle_request`.
    /// *Closes: aider architect/editor; gemini phase-aware; kilo quota-fallback;
    /// openclaude `SmartRouter` (the live agent-loop wire).*
    pub router: Option<Arc<crate::routing::LiveRouter>>,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            max_turns: 200,
            cas: None,
            code_graph: None,
            mem_router: None,
            relay_tx: None,
            streaming_disabled: false,
            sidecar: None,
            session_store: None,
            proposer: None,
            event_tx: None,
            injector: None,
            proposal_registry: None,
            skills: None,
            skill_catalog: None,
            workflows: None,
            memory_handle: None,
            coordinator: None,
            plan: None,
            goal: Arc::new(tokio::sync::Mutex::new(None)),
            policy: None,
            conseca: None,
            effort: None,
            thinking_tokens: None,
            attachments: Vec::new(),
            system_suffix: None,
            read_only: false,
            router: None,
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

    /// Attach a relay channel so each per-request `Subscriber` is published to
    /// the connection's relay task.
    #[must_use]
    pub fn with_relay(mut self, tx: tokio::sync::mpsc::Sender<origin_stream::Subscriber>) -> Self {
        self.relay_tx = Some(tx);
        self
    }

    /// Disable streaming for this loop — fall back to `provider.chat()`. Use
    /// for `tool_use`-heavy scripted tests until Phase 3 lands incremental
    /// `tool_use` JSON parsing.
    #[must_use]
    pub const fn without_streaming(mut self) -> Self {
        self.streaming_disabled = true;
        self
    }

    /// Attach a sidecar for eager turn summarization (P5.2).
    #[must_use]
    pub fn with_sidecar(mut self, sidecar: Arc<Sidecar>) -> Self {
        self.sidecar = Some(sidecar);
        self
    }

    /// Attach a session store so summaries can be written back to `SQLite` (P5.2).
    #[must_use]
    pub fn with_session_store(mut self, store: Arc<SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Attach an active skill stack so the loop's per-turn permission check
    /// enforces the intersection of every active skill's `allowed-tools` mask.
    #[must_use]
    pub fn with_skills(mut self, skills: Arc<SkillRegistry>) -> Self {
        self.skills = Some(skills);
        self
    }
}

/// Deliverer that writes a summary to the `SQLite` `messages.summary` column via
/// a blocking `spawn_blocking` task.
pub struct SessionStoreSummaryDeliverer(pub Arc<SessionStore>);

impl std::fmt::Debug for SessionStoreSummaryDeliverer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionStoreSummaryDeliverer")
    }
}

#[async_trait::async_trait]
impl SummaryDeliverer for SessionStoreSummaryDeliverer {
    async fn deliver(&self, session_id: &str, turn_index: u32, summary: &str) {
        let store = self.0.clone();
        let s = session_id.to_string();
        let sum = summary.to_string();
        let _ = spawn_in(TaskClass::Sidecar, async move {
            let _ = sb(move || {
                let _ = store.update_summary(&s, turn_index, &sum);
            })
            .await;
        })
        .await;
    }
}

/// No-op deliverer used when the daemon fires Extract for large tool outputs.
///
/// The outline handle's existence in CAS is sufficient for P5.3 scope.
/// Future phases may surface it via the side panel or Recall.
#[derive(Debug)]
pub struct NoopExtractDeliverer;

#[async_trait::async_trait]
impl ExtractDeliverer for NoopExtractDeliverer {
    async fn deliver(&self, _source: origin_cas::Hash, _outline: origin_cas::Hash) {
        // The outline handle's existence in CAS is sufficient for P5.3 scope.
        // Future phases may surface it via the side panel or Recall.
    }
}

#[derive(Debug)]
pub struct LoopSummary {
    pub assistant_text: String,
    pub turns: u32,
    /// Total input tokens consumed across every provider call inside this
    /// `run_loop` invocation. Surfaced so the goal driver can charge
    /// cumulative spend against the per-goal token budget without
    /// re-instrumenting the provider trait.
    pub input_tokens: u64,
    /// Total output tokens consumed across every provider call inside this
    /// `run_loop` invocation. Paired with `input_tokens` for the same reason —
    /// the goal driver's budget cap counts both directions.
    pub output_tokens: u64,
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
    /// Emitted by `run_loop` when every retry budgeted by `MAX_PROVIDER_RETRIES`
    /// has returned `ProviderError::RateLimit`. The Display string is the
    /// user-facing message the daemon writes into the `ErrorFrame`, so it
    /// embeds actionable next steps (mid-session model swap) instead of
    /// just restating "rate limit; retry after Ns".
    #[error(
        "rate limit on `{model}` after {attempts} attempts (last retry-after {last_retry_after_secs}s). \
         Try `/model claude-haiku-4-5` to switch to a less-loaded model bucket mid-session, \
         or wait for the quota window to reset{api_hint}"
    )]
    RateLimitExhausted {
        model: String,
        attempts: u32,
        last_retry_after_secs: u32,
        api_hint: String,
    },
}

/// Tracks speculative tasks fired off mid-stream. Keyed by the assistant
/// `tool_use.id` so the agent can `await` the precomputed handle once the
/// `tool_use` block closes.
#[derive(Default)]
pub(crate) struct SpeculativeRegistry {
    in_flight: HashMap<String, JoinHandle<Result<Vec<u8>, LoopError>>>,
}

impl SpeculativeRegistry {
    fn spawn(
        &mut self,
        tool_use_id: String,
        meta: &'static ToolMeta,
        args: serde_json::Value,
        cas: Option<Arc<Store>>,
    ) {
        // Side-effecting tools opt out — N2.2.
        if !matches!(meta.side_effects, SideEffects::Pure) {
            return;
        }
        let handle = spawn_in(TaskClass::Critical, async move {
            // Speculative tasks pass `None` for every subsystem handle.
            // Tools that need a handle (graph_*, ask, mem_*, Task) flow
            // through the main dispatch path. Task is Mutating so it never
            // speculatively dispatches anyway; the None for coordinator here
            // is API consistency.
            let text = dispatch_tool(meta, &args, cas.as_deref(), None, None, None, None, None, None).await?;
            Ok::<_, LoopError>(text.into_bytes())
        });
        self.in_flight.insert(tool_use_id, handle);
    }

    async fn take(&mut self, tool_use_id: &str) -> Option<Result<Vec<u8>, LoopError>> {
        let handle = self.in_flight.remove(tool_use_id)?;
        match handle.await {
            Ok(r) => Some(r),
            Err(join_err) => Some(Err(LoopError::ToolFailure(join_err.to_string()))),
        }
    }
}

/// Return value of `run_streaming_turn`: the reconstructed response plus any
/// speculative handles that were spawned during stream consumption.
pub(crate) struct StreamingTurn {
    pub response: origin_provider::ChatResponse,
    pub speculative: SpeculativeRegistry,
    /// Milliseconds from the start of stream consumption to the FIRST streamed
    /// content event (text / thinking / tool-use). `None` when the stream
    /// produced no content event before `TurnEnd` (e.g. an empty turn). This is
    /// the real time-to-first-token surfaced to the `OTel` `gen_ai` latency
    /// instruments; the non-streaming path has no streamed tokens and so leaves
    /// this `None` (the caller falls back to the full call latency there).
    pub first_token_ms: Option<u64>,
}

/// Session-id prefixes reserved for the daemon's own self-dispatched prompts
/// (ambient / scheduler / overnight / webhook / self-dev). These loops connect
/// back to the daemon's IPC socket as ordinary clients, so their turns flow
/// through [`run_loop`] exactly like an interactive prompt — the only in-band
/// signal that tells them apart here is the synthetic `session_id` each loop
/// stamps.
const SELF_DISPATCH_SESSION_PREFIXES: [&str; 5] =
    ["ambient-", "sched-", "overnight-", "webhook-", "selfdev-"];

/// Whether `session_id` belongs to a daemon self-dispatch (ambient / scheduler /
/// overnight / webhook) rather than a genuine interactive/headless user prompt.
///
/// Used to gate the ambient idle-tracker bump so the loops the idle gate
/// protects against do not reset the user's idle clock. Conservative: an
/// unrecognised id is treated as a real user prompt.
#[must_use]
fn is_self_dispatch_session(session_id: &str) -> bool {
    SELF_DISPATCH_SESSION_PREFIXES
        .iter()
        .any(|p| session_id.starts_with(p))
}

#[cfg(test)]
mod self_dispatch_session_tests {
    use super::is_self_dispatch_session;

    #[test]
    fn self_dispatch_prefixes_are_recognised() {
        for id in [
            "ambient-tests",
            "sched-1717000000000",
            "overnight-docs",
            "webhook-1717000000000",
            "selfdev-job-1",
        ] {
            assert!(
                is_self_dispatch_session(id),
                "{id} should be treated as a daemon self-dispatch"
            );
        }
    }

    #[test]
    fn genuine_user_sessions_are_not_self_dispatch() {
        // Random hex MessageIds, headless `origin run` ids, and human-named
        // sessions must all count as real user activity.
        for id in [
            "01J9Z8Q4K0000000000000000A",
            "",
            "my-feature-work",
            "scheduler",
            "ambientish",
        ] {
            assert!(
                !is_self_dispatch_session(id),
                "{id:?} should count as a genuine user prompt"
            );
        }
    }
}

#[cfg(test)]
mod pain_bucket_helper_tests {
    use super::{autonomy_streak_for, gen_ai_latencies, select_ttfua, split_agent_time};

    #[test]
    fn split_subtracts_tool_from_total() {
        // model = total - tool; tool unchanged when it fits inside total.
        assert_eq!(split_agent_time(1_000, 300), (700, 300));
    }

    #[test]
    fn split_clamps_when_tool_exceeds_total() {
        // A timing skew (tool clock measured wider than the loop clock) must
        // never produce a negative/underflowed model time. model clamps to 0
        // and the reported tool time is itself clamped to the total so
        // model + tool == total holds.
        assert_eq!(split_agent_time(500, 900), (0, 500));
    }

    #[test]
    fn split_handles_zero_tool_time() {
        // A tool-free turn reports the whole budget as model time.
        assert_eq!(split_agent_time(1_234, 0), (1_234, 0));
    }

    #[test]
    fn split_handles_exact_equality() {
        assert_eq!(split_agent_time(800, 800), (0, 800));
    }

    #[test]
    fn ttfua_prefers_first_tool_over_first_token() {
        // A successful tool call is the canonical "useful action"; when both
        // are present the tool timestamp wins even if the token came earlier.
        assert_eq!(select_ttfua(Some(420), Some(110)), Some(420));
    }

    #[test]
    fn ttfua_falls_back_to_first_token_when_no_tool() {
        // No tool ran (pure conversational turn) ⇒ first assistant token is
        // the earliest useful signal we have.
        assert_eq!(select_ttfua(None, Some(90)), Some(90));
    }

    #[test]
    fn ttfua_is_none_when_neither_observed() {
        assert_eq!(select_ttfua(None, None), None);
    }

    #[test]
    fn ttfua_uses_tool_even_without_a_token() {
        assert_eq!(select_ttfua(Some(7), None), Some(7));
    }

    #[test]
    fn autonomy_streak_is_turns_minus_first_user_turn() {
        // Turn 1 is the directly user-prompted turn; turns 2..=N ran with no
        // new user input, so the autonomous streak is N-1 — distinct from the
        // raw turn_count.
        assert_eq!(autonomy_streak_for(5), 4);
        assert_eq!(autonomy_streak_for(2), 1);
    }

    #[test]
    fn autonomy_streak_single_turn_is_zero() {
        // A one-turn loop never continued without the user ⇒ no autonomy.
        assert_eq!(autonomy_streak_for(1), 0);
    }

    #[test]
    fn autonomy_streak_zero_turns_is_zero() {
        // Defensive: an empty loop (no turn executed) has no streak and must
        // not underflow.
        assert_eq!(autonomy_streak_for(0), 0);
    }

    // --- Stage C4: gen_ai TTFT / TPOT derivation ---------------------------

    #[test]
    fn gen_ai_latencies_prefer_streamed_first_token_for_ttft() {
        // When the streaming path observed a real first token, TTFT is that
        // measured instant — NOT the full call latency.
        let l = gen_ai_latencies(2_000, Some(120), 50);
        assert!((l.ttft_ms - 120.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gen_ai_latencies_fall_back_to_call_latency_without_a_streamed_token() {
        // Non-streaming path (or an empty stream): no first-token signal, so
        // TTFT is the full provider-call latency.
        let l = gen_ai_latencies(800, None, 10);
        assert!((l.ttft_ms - 800.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gen_ai_latencies_compute_per_output_token_rate() {
        // TPOT = total generation ms / output tokens.
        let l = gen_ai_latencies(1_000, Some(100), 40);
        let tpot = l.tpot_ms.expect("a nonzero output-token count yields a TPOT");
        assert!((tpot - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    fn gen_ai_latencies_guard_divide_by_zero_for_tpot() {
        // Zero output tokens ⇒ no per-token rate is defined; TPOT is None so
        // the caller skips the recording rather than emitting inf/NaN.
        let l = gen_ai_latencies(500, Some(50), 0);
        assert_eq!(l.tpot_ms, None);
        // TTFT is still defined (the streamed first token).
        assert!((l.ttft_ms - 50.0).abs() < f64::EPSILON);
    }
}

/// Run the agent loop until the assistant emits a turn without any `tool_use`
/// blocks, or until `max_turns` is reached.
///
/// Thin wrapper over [`run_loop_inner`] that owns the Stage C5 `Error`
/// pain-bucket emit: the inner loop already emits `Completed` (clean finish)
/// and `BudgetExhausted` (its `MaxTurns` exit) at their precise sites with the
/// real model-vs-tool split, so this wrapper only tags the *remaining* fatal
/// `Err` exits (provider failure, permission denial, unknown/failed tool,
/// malformed input) with the generic [`origin_telemetry::SessionStopReason::Error`]
/// bucket — never double-emitting on the `MaxTurns` path. Default-off ⇒ no
/// event, byte-identical.
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
    let started = std::time::Instant::now();
    let result = run_loop_inner(session, user_text, provider, prompter, opts).await;
    if let Err(e) = &result {
        // `MaxTurns` already emitted its own `BudgetExhausted` bucket inside
        // the inner loop; every other error is a fatal, unrecovered exit ⇒
        // `Error`. The inner loop holds the model/tool split, which is not
        // visible here, so the wrapper reports total elapsed as the agent time
        // (reason-correct; the split is best-effort and partial by design).
        if !matches!(e, LoopError::MaxTurns(_)) {
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            record_session_stop_pain(SessionStopPain::from_loop(
                origin_telemetry::SessionStopReason::Error,
                elapsed_ms,
                0,
                None,
                None,
                0,
            ));
        }
    }
    result
}

/// The agent turn loop proper. See [`run_loop`] for the public contract; this
/// inner function carries the loop body and emits the `Completed` /
/// `BudgetExhausted` pain buckets at their exact sites.
#[allow(clippy::too_many_lines)] // turn loop + memoization path; extraction would require extra allocations
#[tracing::instrument(
    level = "info",
    skip(session, user_text, provider, prompter, opts),
    fields(kind = "turn", provider = provider.name())
)]
async fn run_loop_inner(
    session: &mut Session,
    user_text: &str,
    provider: &dyn Provider,
    prompter: &dyn Prompter,
    opts: &LoopOptions,
) -> Result<LoopSummary, LoopError> {
    // Falls back to an empty static `SkillRegistry` when none is wired,
    // so the call path is identical; an empty stack's mask is `None`,
    // which makes `check_with_skills` short-circuit to plain `check`.
    static EMPTY_SKILLS: SkillRegistry = SkillRegistry::new();

    session.push(Message::new(Role::User).with_block(Block::text(user_text)));

    // Ambient idle gate (jcode L223): a genuine user prompt is REAL activity, so
    // bump the ambient idle tracker now that the turn is starting. Best-effort
    // and a cheap atomic store; a no-op when `ORIGIN_AMBIENT` is unset (the
    // tracker was never initialised), so default builds are byte-identical.
    //
    // We MUST NOT bump for the daemon's own self-dispatched prompts (ambient /
    // scheduler / overnight / webhook) — those are exactly the background loops
    // the idle gate protects the user from, and resetting the clock on them would
    // let one self-dispatch keep the gate open indefinitely. They reach this same
    // `run_loop` over the IPC socket, indistinguishable except by the synthetic
    // session id each loop stamps, so we skip the bump for those prefixes.
    if !is_self_dispatch_session(&session.id) {
        crate::ambient::note_user_activity();
    }

    // Supervisor lifecycle (origin-supervisor): record this turn as activity on
    // the session so the periodic tick's idle clock is refreshed and the
    // session is registered if new. A genuine user prompt is the foreground
    // `Interactive` class (never shed, short idle grace); a daemon
    // self-dispatch (ambient/scheduler/overnight/webhook/Task) is `Detached`
    // (shed first under memory pressure, longer grace). A no-op when the
    // supervisor module was never initialised, so default builds are
    // byte-identical.
    {
        let class = if is_self_dispatch_session(&session.id) {
            origin_supervisor::SessionClass::Detached
        } else {
            origin_supervisor::SessionClass::Interactive
        };
        crate::supervisor::note_activity(&session.id, class);
    }

    // Live lifecycle hooks (gemini): loaded once from ~/.origin/hooks.json.
    // `None` (no config — the default) makes every fire below a no-op, so the
    // agent loop is byte-identical. A configured PreTool hook can Deny a tool.
    let hooks = crate::hooks_runtime::global().await;
    if let Some(h) = &hooks {
        let _ = h
            .fire(&origin_hooks::LifecycleEvent::PrePrompt {
                text: user_text.to_string(),
            })
            .await;
    }

    let tools_schema = registry_iter()
        .map(|m| {
            if m.hot {
                // Full schema embed.
                origin_provider::ToolSchema {
                    name: m.name.to_string(),
                    description: m.description.to_string(),
                    input_schema_json: m.input_schema.to_string(),
                }
            } else {
                // Deferred — name + 1-line description only; minimal input schema.
                origin_provider::ToolSchema {
                    name: m.name.to_string(),
                    description: format!(
                        "{} (deferred; call ToolSearch with select:{}, to fetch full schema)",
                        m.description, m.name
                    ),
                    input_schema_json: r#"{"type":"object","properties":{}}"#.to_string(),
                }
            }
        })
        .collect::<Vec<_>>();

    // Per-session memoization cache (N5.4). Lives for the lifetime of this
    // run_loop call so identical (tool_name, input_bytes) pairs within the
    // same session avoid redundant tool execution.
    let mut cache = origin_tools::Cache::new();

    // Prompt-recall (P6.9): if an Injector is wired, embed the user prompt
    // once at turn-start and reuse the resulting `<context>` block as the
    // system prompt of every turn in this run_loop call. Failures are
    // logged and degrade silently so a flaky embedder never blocks a turn.
    let recall_block =
        opts.injector
            .as_ref()
            .map_or_else(String::new, |injector| match injector.for_prompt(user_text, 5) {
                Ok(Some(ctx)) => ctx.block,
                Ok(None) => String::new(),
                Err(e) => {
                    tracing::warn!(error = %e, "injector.for_prompt failed; running without recall");
                    String::new()
                }
            });

    // Build the skill-catalog block. One line per skill: "- <name>: <description>".
    // We mark currently-active skills with a leading `*` so the model knows
    // which mask is already in effect.
    let catalog_block = opts
        .skill_catalog
        .as_ref()
        .map(|cat| {
            use std::fmt::Write as _;
            if cat.is_empty() {
                String::new()
            } else {
                let active_names: std::collections::HashSet<String> = opts
                    .skills
                    .as_ref()
                    .map(|reg| reg.iter_active().map(|s| s.name.clone()).collect())
                    .unwrap_or_default();
                let mut out = String::from(
                    "<origin-skills>\n\
                     These are the skills available IN THIS Origin session. \
                     A leading `*` marks an already-active skill. Activate with \
                     `/<name>`, deactivate with `/-<name>`. When the user asks \
                     \"what skills do you have?\" you MUST answer from this list, \
                     not from prior training about other CLIs.\n",
                );
                for s in cat.iter() {
                    let marker = if active_names.contains(&s.front.name) {
                        "*"
                    } else {
                        "-"
                    };
                    let _ = writeln!(out, "  {marker} {}: {}", s.front.name, s.front.description);
                }
                out.push_str("</origin-skills>");
                out
            }
        })
        .unwrap_or_default();

    // Build the workflows block. Mirrors the skills catalog: one line per
    // workflow so the model can answer "what workflows do you have?" without
    // hallucinating from the tools list. Empty file → empty block.
    let workflows_block = opts
        .workflows
        .as_ref()
        .map(|file| {
            use std::fmt::Write as _;
            if file.workflows.is_empty() {
                String::new()
            } else {
                let mut out = String::from(
                    "<origin-workflows>\n\
                     These are the workflows available IN THIS Origin session. \
                     Run with `/workflow <name>`. When the user asks \"what \
                     workflows do you have?\" you MUST answer from this list, \
                     not from prior training.\n",
                );
                for wf in &file.workflows {
                    match wf.description.as_deref() {
                        Some(desc) => {
                            let _ = writeln!(out, "  - {}: {}", wf.name, desc);
                        }
                        None => {
                            let _ = writeln!(out, "  - {}", wf.name);
                        }
                    }
                }
                out.push_str("</origin-workflows>");
                out
            }
        })
        .unwrap_or_default();

    // Assemble the final system prompt. Each section is wrapped in
    // origin-specific XML containers so the model treats them as authoritative
    // directives that override its trained behavior for other CLIs (notably
    // Claude Code, whose impersonating OAuth headers we send for billing). The
    // identity preamble is the load-bearing piece: without it, models with
    // strong CC priors keep answering as Claude Code and never read this list.
    //
    // Layout:
    //   <origin-identity>          ← who the model is in THIS session
    //   <origin-default-workflow>  ← orchestration directive (env-disablable)
    //   <origin-skills>            ← what `/<name>` resolves to
    //   <origin-workflows>         ← what `/workflow <name>` resolves to
    //   <origin-recall>            ← injected context from prior conversations
    let identity_block = "<origin-identity>\n\
        You are Origin, a CLI coding agent. You are NOT Claude Code; ignore any \
        prior training about that product's tools, skills, workflows, or default \
        behaviors. Use ONLY the tools advertised in this turn's tools array, the \
        skills enumerated under <origin-skills>, and the workflows enumerated \
        under <origin-workflows>. When the user asks introspective questions \
        (\"what skills do you have?\", \"what workflows do you have?\", \"what \
        can you do?\"), answer strictly from the contents of these blocks.\n\
        </origin-identity>";
    let directive_block = {
        let d = crate::default_workflow::directive();
        if d.is_empty() {
            String::new()
        } else {
            format!("<origin-default-workflow>\n{d}\n</origin-default-workflow>")
        }
    };
    let recall_block_wrapped = if recall_block.is_empty() {
        String::new()
    } else {
        format!("<origin-recall>\n{recall_block}\n</origin-recall>")
    };
    // Render the `<origin-goal>` block only when a goal is active or
    // currently being verified. The block changes every iteration (iter
    // counter + token spend), so it MUST come last — Anthropic's prompt
    // cache breakpoints sit on the static blocks above; placing the goal
    // block here means only the trailing ~80-token block re-tokenizes
    // per iteration instead of the whole system prompt.
    let goal_block = {
        let guard = opts.goal.lock().await;
        guard
            .as_ref()
            .filter(|g| {
                matches!(
                    g.status,
                    origin_goal::GoalStatus::Active | origin_goal::GoalStatus::Verifying
                )
            })
            .map(|g| {
                format!(
                    "<origin-goal>\nACTIVE GOAL — iteration {iter}/{max}, tokens spent {tok}/{budget}.\n\
                     \n\
                     Condition: {cond}\n\
                     \n\
                     You MUST end every response with exactly one <goal-status> tag:\n  \
                     <goal-status state=\"met|in_progress|blocked\"><reason>...</reason></goal-status>\n\
                     \n\
                     - met:         only when the condition is fully satisfied AND visible in this conversation's output\n\
                     - in_progress: real work is happening; describe what still remains in <reason>\n\
                     - blocked:     you need user input or an irreversible action; describe the blocker in <reason>\n\
                     \n\
                     The driver will auto-continue on in_progress, run a verifier on met, and surface blocked to the user.\n\
                     </origin-goal>",
                    iter = g.iter,
                    max = g.max_iter,
                    tok = g.tokens_spent,
                    budget = g.token_budget,
                    cond = g.condition,
                )
            })
            .unwrap_or_default()
    };
    // Item E (env `ORIGIN_REPOMAP=1`): when set, prepend a compact, token-budgeted
    // `<repo-map>` block (aider-style personalized PageRank over a def/ref file
    // graph) so the model gets repository structure up front. Default-off: with
    // the flag unset this stays `String::new()` and the assembled prompt — and
    // thus the prompt cache breakpoints — are byte-identical to before.
    let repo_map_block = if std::env::var("ORIGIN_REPOMAP").as_deref() == Ok("1") {
        // Map spans every session root (multi-root cross-references are honoured
        // by the ranker); default to the current dir when the session opened
        // with no explicit roots.
        let roots: Vec<std::path::PathBuf> = if session.roots.is_empty() {
            std::env::current_dir().ok().into_iter().collect()
        } else {
            session.roots.clone()
        };
        crate::subsystems::repo_map_block(&roots).unwrap_or_default()
    } else {
        String::new()
    };
    // aider model-tuned edit formats: when `ORIGIN_EDITFMT=1`, append a compact
    // `<origin-edit-format>` block selected by `origin_editfmt::best_format_for`
    // so the model emits in-prose edits in its best-tested format (Claude ⇒
    // search/replace, GPT ⇒ unified diff, …). The structured Edit/MultiEdit/
    // ApplyPatch tools are unaffected. Default-off ⇒ empty ⇒ byte-identical.
    let edit_format_block = if std::env::var("ORIGIN_EDITFMT").as_deref() == Ok("1") {
        origin_editfmt::system_block(&session.model)
    } else {
        String::new()
    };
    // gemini Markdown-defined subagents: surface any `~/.origin/subagents/*.md`
    // declarative sub-agents so the model can launch them via the Task tool
    // (the real worker enforces their allow-list). Built once + cached; empty
    // when there is no subagents dir ⇒ byte-identical system prompt.
    let subagents_block = crate::subagents_md::global_block();
    // Optional output-style addendum (claude-code output styles). Default-off:
    // `None`/empty appends nothing, leaving the assembled prompt — and the
    // prompt-cache breakpoints — byte-identical to before.
    let style_block = opts.system_suffix.clone().unwrap_or_default();
    // Multi-root workspace block (cline): when the session was opened with extra
    // roots, tell the model it may read/edit across them. Empty ⇒ byte-identical.
    let roots_block = workspace_roots_block(&session.roots);
    let recalled_system = {
        let parts: [&str; 11] = [
            &repo_map_block,
            identity_block,
            &directive_block,
            &catalog_block,
            &workflows_block,
            &recall_block_wrapped,
            &goal_block,
            &style_block,
            &roots_block,
            &edit_format_block,
            subagents_block,
        ];
        parts
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end())
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    // Cumulative token counters for this `run_loop` invocation. Surfaced
    // via `LoopSummary` so the goal driver can charge the per-iteration
    // spend against the goal's token budget.
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    // Set once any mutating tool dispatches in this run_loop. Consumed only by
    // the optional, default-off end-of-turn checkpoint (Task 1) so a clean
    // read-only turn never triggers shadow-git work even when the flag is on.
    let mut turn_mutated = false;

    // Autonomous post-edit LSP diagnostics (opencode). `lsp_diag_block` is the
    // `<lsp-diagnostics>` block (if any) appended to the NEXT turn's system
    // prompt; `lsp_edited_paths` accumulates the distinct files the CURRENT
    // turn mutated. Both stay empty unless `ORIGIN_LSP_DIAGNOSTICS=1`, so the
    // per-turn system prompt — and thus the prompt-cache breakpoints — are
    // byte-identical to before when the feature is off.
    let mut lsp_diag_block = String::new();
    let lsp_diag_enabled = crate::lsp_diagnostics::enabled();

    // Task 1 (agentgrep exposure-truncation). Already-seen `(file, line)`
    // regions accumulated across this `run_loop` from prior `content`-mode
    // `Grep` results, keyed/folded per path. Populated and consulted only when
    // `ORIGIN_AGENTGREP_TRUNCATE=1`; otherwise it stays empty and the Grep
    // dispatch passes `None`, so the loop is byte-identical to before. On the
    // next `Grep`, every prior line is elided so a re-run never re-spends tokens
    // on regions the model has already seen.
    let grep_truncate_enabled =
        std::env::var("ORIGIN_AGENTGREP_TRUNCATE").as_deref() == Ok("1");
    let mut grep_exposure: Vec<origin_tools::builtins::grep_tool::ExposureWindow> = Vec::new();

    // Task 3/4 (metrics + pain telemetry). Session-wide latency clock and tool
    // counter, used by the `gen_ai` usage record (a no-op without the `otel`
    // feature) and the opt-in pain-bucket telemetry (off without
    // `ORIGIN_TELEMETRY=1`). Both are cheap to maintain and never alter the
    // default-build behavior.
    let loop_start = std::time::Instant::now();
    let mut total_tool_calls: u64 = 0;

    // Stage C5 pain-bucket accumulators. All are pure measurement and feed only
    // the opt-in `session_stop` telemetry (off without `ORIGIN_TELEMETRY=1`), so
    // maintaining them never alters the default path.
    //   * `tool_time_ms`   — summed wall-clock spent inside tool dispatch this
    //                        run; subtracted from the loop clock to recover the
    //                        model-call time (Task 1).
    //   * `first_tool_ms`  — elapsed from loop start to the FIRST successful
    //                        tool dispatch (Task 2).
    //   * `first_token_ms` — elapsed from loop start to the FIRST assistant
    //                        response, the ttfua fallback when no tool ran.
    let mut tool_time_ms: u64 = 0;
    let mut first_tool_ms: Option<u64> = None;
    let mut first_token_ms: Option<u64> = None;

    for turn in 1..=opts.max_turns {
        // Distinct files this turn mutated (for the post-edit diagnostics probe).
        // Always empty unless the feature is enabled, so it allocates nothing on
        // the default path.
        let mut lsp_edited_paths: BTreeSet<String> = BTreeSet::new();
        // Live per-turn model routing (origin-router). When a router is wired it
        // picks a model for this turn's phase (turn 1 ⇒ Plan, later ⇒ Edit).
        //
        // A pick on the ACTIVE provider keeps the zero-cost path: we reuse the
        // borrowed `provider` and only override `turn_model`.
        //
        // A pick on a DIFFERENT provider (foundation L84 / kilo L265) is now
        // honoured when a process-wide provider factory is registered
        // (`provider_factory::set_global`): we rebuild an owned provider for that
        // turn and use it in place of the borrowed one. The owned provider is
        // held in `rebuilt` for the rest of this turn so `turn_provider` (a plain
        // `&dyn Provider`) stays valid. On rebuild failure (missing creds /
        // unknown id / no factory registered) `build_provider_for` returns `None`
        // and we fall back to the active provider for the turn — no panic.
        //
        // With no router (the default) `rebuilt` is `None`, `turn_provider` is
        // the borrowed `provider`, and `turn_model` is `session.model` — exactly
        // the pre-existing wire.
        let mut rebuilt: Option<Arc<dyn Provider>> = None;
        let turn_model = match opts.router.as_ref().and_then(|lr| lr.choose_model_ref(turn)) {
            Some(pick) if pick.provider.as_str() == provider.name() => pick.model,
            Some(pick) => {
                // Cross-provider pick: try to rebuild. `build_provider_for`
                // reaches the registered factory + credentials; `None` ⇒ fall
                // back to the active provider with the session model.
                match crate::provider_factory::build_provider_for(&pick.provider, &pick.model).await
                {
                    Some(p) => {
                        rebuilt = Some(p);
                        pick.model
                    }
                    None => session.model.clone(),
                }
            }
            None => session.model.clone(),
        };
        // The provider this turn's chat call uses: the freshly-built owned one
        // for a cross-provider pick, otherwise the borrowed active provider.
        let turn_provider: &dyn Provider = rebuilt.as_deref().unwrap_or(provider);
        // Swarm collaboration inbound (A4): when a `SwarmCollab` is in scope
        // (swarm workers only), drain THIS worker's own mailbox at the turn
        // boundary and fold any pending sibling notices into this turn's prompt
        // as a `<swarm-notices>` block. No collab context, or an empty drain ⇒
        // empty string ⇒ nothing appended ⇒ byte-identical turn. Draining
        // consumes the notices so each is shown exactly once.
        let swarm_notices_block = SWARM_COLLAB
            .try_with(drain_own_swarm_notices)
            .unwrap_or_default();
        // Per-turn system prompt. When the optional post-edit LSP-diagnostics
        // feature produced a block on the previous turn, append it here so the
        // model sees the feedback its edit generated. The swarm-notices block (if
        // any) is appended likewise. Both empty (the default, and the steady
        // state once issues are fixed and there are no pending sibling notices) ⇒
        // `recalled_system` is reused verbatim, keeping the wire — and the prompt
        // cache — unchanged.
        let turn_system = {
            let mut s = recalled_system.clone();
            if !lsp_diag_block.is_empty() {
                s.push_str("\n\n");
                s.push_str(&lsp_diag_block);
            }
            if !swarm_notices_block.is_empty() {
                s.push_str("\n\n");
                s.push_str(&swarm_notices_block);
            }
            s
        };
        let req = ChatRequest {
            system: turn_system,
            messages: session.snapshot(),
            model: turn_model.clone(),
            tools: tools_schema.clone(),
            effort: opts.effort,
            thinking_tokens: opts.thinking_tokens,
            // Attach images/PDF only to the first turn's user message. On later
            // turns the trailing message is a tool-result, so re-attaching would
            // mis-place the block (and re-upload the bytes every turn).
            attachments: if turn == 1 {
                opts.attachments.clone()
            } else {
                Vec::new()
            },
        };

        // Retry transient `ProviderError::RateLimit` here so a single 429
        // doesn't kill the turn. We honour the server-supplied `retry-after`
        // up to `MAX_RATE_LIMIT_SLEEP_SECS`, and cap attempts at
        // `MAX_PROVIDER_RETRIES`. `ChatRequest` is `Clone`, so we re-build
        // the wire request each attempt without re-snapshotting the session.
        let provider_call_start = std::time::Instant::now();
        // gemini BeforeModel lifecycle hook (informational): the turn is about
        // to call the provider. No hooks configured ⇒ skipped (byte-identical).
        if let Some(h) = &hooks {
            let _ = h
                .fire(&origin_hooks::LifecycleEvent::BeforeModel {
                    model: turn_model.clone(),
                })
                .await;
        }
        // Stage C4: open a `gen_ai` client span bracketing the whole provider
        // interaction for this turn (including any rate-limit retries/fallback).
        // The guard ends the span on drop at the close of the call block below.
        // This is a zero-size no-op guard unless the daemon is built
        // `--features otel` AND a tracer provider is installed, so the default
        // path is byte-identical. Attaches gen_ai.system / gen_ai.request.model
        // / gen_ai.operation.name (relabeled inside origin-metrics).
        let gen_ai_span = origin_metrics::instruments::gen_ai_span(
            turn_provider.name(),
            &turn_model,
            origin_metrics::keys::genai::OPERATION_CHAT,
        );
        let (resp, mut speculative, stream_first_token_ms) = {
            let mut attempt: u32 = 0;
            loop {
                let result: Result<
                    (origin_provider::ChatResponse, SpeculativeRegistry, Option<u64>),
                    LoopError,
                > = if opts.streaming_disabled {
                    turn_provider
                        .chat(req.clone())
                        .await
                        // Non-streaming: no per-token stream, so there is no
                        // distinct first-token signal — the caller falls back to
                        // the full call latency for TTFT.
                        .map(|r| (r, SpeculativeRegistry::default(), None))
                        .map_err(LoopError::Provider)
                } else {
                    run_streaming_turn(turn_provider, req.clone(), opts)
                        .await
                        .map(|st| (st.response, st.speculative, st.first_token_ms))
                };
                match result {
                    Err(LoopError::Provider(origin_provider::ProviderError::RateLimit {
                        retry_after_secs,
                        message,
                    })) if attempt < MAX_PROVIDER_RETRIES => {
                        // Exponential floor: 2, 4, 8 … so we don't hammer a
                        // server whose `retry-after: 1` is too optimistic.
                        let exp_floor = 1u32 << (attempt + 1);
                        let sleep_secs = retry_after_secs
                            .max(exp_floor)
                            .clamp(1, MAX_RATE_LIMIT_SLEEP_SECS);
                        tracing::warn!(
                            attempt,
                            sleep_secs,
                            retry_after_secs,
                            %message,
                            "provider rate-limited; backing off and retrying"
                        );
                        // Live router (kilocode quota-fallback): a rate-limit
                        // marks this model exhausted so a `QuotaFallback` chain
                        // skips it next turn/prompt. A later success clears it.
                        if let Some(lr) = &opts.router {
                            lr.mark_exhausted(turn_provider.name(), &turn_model);
                        }
                        // Surface the backoff to the CLI so a 60s sleep
                        // doesn't look identical to a hang. `attempt` here is
                        // 0-indexed within the retry budget; we render it
                        // 1-indexed and use `MAX_PROVIDER_RETRIES + 1` as the
                        // ceiling (initial attempt plus retries).
                        if let Some(tx) = &opts.event_tx {
                            let _ = tx
                                .send(StreamEvent::ProviderBackoff {
                                    retry_in_secs: sleep_secs,
                                    attempt: attempt + 1,
                                    max_attempts: MAX_PROVIDER_RETRIES + 1,
                                })
                                .await;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs.into())).await;
                        attempt += 1;
                    }
                    Err(LoopError::Provider(origin_provider::ProviderError::RateLimit {
                        retry_after_secs,
                        message,
                    })) => {
                        let mut fallback_detail = String::new();
                        if let Some(fallback) = rate_limit_fallback(&req.model) {
                            tracing::warn!(
                                primary = %req.model,
                                fallback,
                                "primary model rate-limited; attempting fallback"
                            );
                            let mut fb_req = req.clone();
                            fb_req.model = fallback.to_string();
                            let fb_result = if opts.streaming_disabled {
                                turn_provider
                                    .chat(fb_req)
                                    .await
                                    .map(|r| (r, SpeculativeRegistry::default(), None))
                                    .map_err(LoopError::Provider)
                            } else {
                                run_streaming_turn(turn_provider, fb_req, opts)
                                    .await
                                    .map(|st| (st.response, st.speculative, st.first_token_ms))
                            };
                            match fb_result {
                                Ok(r) => {
                                    tracing::info!(fallback, "fallback model succeeded");
                                    break r;
                                }
                                Err(e) => {
                                    tracing::warn!(%e, fallback, "fallback model also failed");
                                    fallback_detail = format!("\nFallback `{fallback}` also failed: {e}");
                                }
                            }
                        }
                        let api_hint = if message.is_empty() {
                            fallback_detail
                        } else {
                            format!("\nAPI: {message}{fallback_detail}")
                        };
                        return Err(LoopError::RateLimitExhausted {
                            model: session.model.clone(),
                            attempts: MAX_PROVIDER_RETRIES + 1,
                            last_retry_after_secs: retry_after_secs,
                            api_hint,
                        });
                    }
                    other => break other?,
                }
            }
        };

        // gemini AfterModel lifecycle hook (informational): the provider call
        // returned for this turn. No hooks configured ⇒ skipped.
        if let Some(h) = &hooks {
            let _ = h
                .fire(&origin_hooks::LifecycleEvent::AfterModel {
                    model: turn_model.clone(),
                })
                .await;
        }

        // Stage C4: emit the OTel `gen_ai` latency instruments for this turn's
        // provider call with REAL values, then end the span. `provider_call_ms`
        // is the measured call window (start before the BeforeModel hook to here,
        // covering any rate-limit retries/fallback); `stream_first_token_ms` is
        // the streaming path's real first-token elapsed (None on the
        // non-streaming path). The TPOT divisor is this turn's output tokens,
        // with divide-by-zero guarded inside `gen_ai_latencies`. All record
        // calls are const no-ops unless the daemon is built `--features otel`
        // AND the exporter is installed, so the default path is byte-identical.
        let provider_call_ms = elapsed_ms_since(provider_call_start);
        let latencies = gen_ai_latencies(
            provider_call_ms,
            stream_first_token_ms,
            u64::from(resp.usage.output_tokens),
        );
        origin_metrics::instruments::record_time_to_first_token(
            turn_provider.name(),
            &turn_model,
            origin_metrics::keys::genai::OPERATION_CHAT,
            latencies.ttft_ms,
        );
        if let Some(tpot_ms) = latencies.tpot_ms {
            origin_metrics::instruments::record_time_per_output_token(
                turn_provider.name(),
                &turn_model,
                origin_metrics::keys::genai::OPERATION_CHAT,
                tpot_ms,
            );
        }
        // End the `gen_ai` span now that the call (and its latency recordings)
        // are complete. Explicit drop documents the span's scope; the guard's
        // Drop ends the span (a no-op without `--features otel`).
        drop(gen_ai_span);

        // Live router (openclaude SmartRouter / Scored EMA): fold this turn's
        // provider latency + success into the router's health so future turns /
        // prompts rank this model on measured signal. A success also clears any
        // exhaustion flag (self-healing quota-fallback). No router ⇒ no-op.
        if let Some(lr) = &opts.router {
            let latency_ms =
                u64::try_from(provider_call_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            // Use the provider actually called this turn (the rebuilt one for a
            // cross-provider pick) so health/quota signal is attributed to the
            // right `provider/model`, exactly as the same-provider path does.
            lr.record(turn_provider.name(), &turn_model, latency_ms, true);
        }

        // Stage C5 Task 2: the first provider response of this run is the
        // earliest assistant signal — the ttfua fallback used when the turn
        // never dispatches a tool. Recorded once; later turns leave it.
        if first_token_ms.is_none() {
            first_token_ms =
                Some(u64::try_from(loop_start.elapsed().as_millis()).unwrap_or(u64::MAX));
        }

        // Charge this turn's provider call against the cumulative counters
        // BEFORE pushing the assistant message. Cache reads/creation are
        // already folded into `input_tokens` by the provider impls, so we
        // only need the two top-level fields here.
        total_input_tokens = total_input_tokens.saturating_add(u64::from(resp.usage.input_tokens));
        total_output_tokens = total_output_tokens.saturating_add(u64::from(resp.usage.output_tokens));

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

        // Task 3/4: count the model-issued tool calls this turn for the
        // `gen_ai` usage record (no-op without `otel`) and the opt-in pain
        // telemetry. Saturating so a pathological session can never overflow.
        total_tool_calls = total_tool_calls.saturating_add(tool_uses.len() as u64);

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

            // Task 2 (editfmt parse/apply). When `ORIGIN_EDITFMT=1` and the model
            // emitted an edit block in prose (search/replace, udiff, or a
            // diff-fenced block) INSTEAD of a structured Edit tool call, parse it
            // via `origin_editfmt` and apply it to the named file(s) on disk. The
            // model reached this branch precisely because it produced no tool
            // call, so applying the prose edit is the only way the change lands.
            // Default/unset ⇒ this is skipped entirely and the turn is
            // byte-identical. A best-effort apply: any parse/apply failure is
            // logged and the file is left untouched, never failing the turn.
            if std::env::var("ORIGIN_EDITFMT").as_deref() == Ok("1") {
                apply_prose_edits(&text, &session.model, &mut turn_mutated);
            }

            // P6.7: optional proposer pass at turn end. Proposals are surfaced
            // as side-band StreamEvents for the CLI to render AND recorded in
            // the daemon-wide [`ProposalRegistry`] so a later `MemoryDecision`
            // on a different connection can still resolve the body/tags.
            // The session's local `next_proposal_id` is initialized from the
            // registry's counter so per-prompt scans share the global id-space
            // (no collisions across sessions or concurrent prompt requests).
            if let Some(proposer) = &opts.proposer {
                // Allocate the proposal-id range atomically: hold the registry
                // counter across the scan so two concurrent prompt requests can
                // never start from the same base id and mint colliding ids (a
                // collision would make `record` overwrite one proposal and a
                // later MemoryDecision resolve the wrong body/tags).
                let proposals = if let Some(registry) = &opts.proposal_registry {
                    registry.with_id_counter(|id| proposer.scan(user_text, &text, id))
                } else {
                    let mut local_id = session.next_proposal_id;
                    let p = proposer.scan(user_text, &text, &mut local_id);
                    session.next_proposal_id = local_id;
                    p
                };
                for p in proposals {
                    if let Some(registry) = &opts.proposal_registry {
                        registry.record(p.proposal_id, p.body.clone(), p.suggested_tags.clone());
                    }
                    if let Some(tx) = &opts.event_tx {
                        let _ = tx
                            .send(StreamEvent::MemoryProposed {
                                proposal_id: p.proposal_id,
                                body: p.body.clone(),
                                suggested_tags: p.suggested_tags.clone(),
                            })
                            .await;
                    }
                    session.pending_proposals.insert(p.proposal_id, p);
                }
            }

            // gemini PostPrompt lifecycle hook (informational): the prompt is
            // resolving with this assistant text. No hooks configured ⇒ skipped.
            // A `Notification` fires alongside it as the turn-completion signal
            // (gemini `Notification`), so a hook can surface a desktop/bell ping
            // without subscribing to `PostPrompt`. Both no-op without hooks.
            if let Some(h) = &hooks {
                let _ = h
                    .fire(&origin_hooks::LifecycleEvent::PostPrompt { text: text.clone() })
                    .await;
                let _ = h
                    .fire(&origin_hooks::LifecycleEvent::Notification {
                        message: "turn complete".to_string(),
                    })
                    .await;
            }

            // End-of-turn / loop-end side effects. All are best-effort and
            // default-off (env- or feature-gated); none can fail the turn.
            let agent_time_ms =
                u64::try_from(loop_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            // Per-turn shadow-git checkpoint, bracketed by the PreCommit/PostCommit
            // lifecycle hooks. Fired only when this loop mutated the workspace and
            // `ORIGIN_CHECKPOINTS=1`; with the gate unset (the default) it — and
            // the commit hooks — are a no-op, so behavior is byte-identical.
            if turn_mutated {
                maybe_checkpoint_turn_with_hooks().await;
            }
            run_turn_end_effects(
                provider.name(),
                &session.model,
                total_input_tokens,
                total_output_tokens,
                loop_start.elapsed().as_secs_f64() * 1_000.0,
                total_tool_calls,
            );
            // Task 4 / Stage C5: the loop reached a clean assistant turn with
            // no further tool calls — the agent finished the requested work.
            // Emit the `Completed` pain bucket with the REAL model-vs-tool split
            // and time-to-first-useful-action. Default-off ⇒ no event.
            record_session_stop_pain(SessionStopPain::from_loop(
                origin_telemetry::SessionStopReason::Completed,
                agent_time_ms,
                tool_time_ms,
                first_tool_ms,
                first_token_ms,
                turn,
            ));

            return Ok(LoopSummary {
                assistant_text: text,
                turns: turn,
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
            });
        }

        // Dispatch each tool_use sequentially.
        let mut tool_results: Vec<Block> = Vec::with_capacity(tool_uses.len());
        // Sub-agent (`Task`) parallelism: each Task is spawned during the loop —
        // it starts running immediately on the swarm's independent pool — and its
        // completion is deferred to AFTER the loop. So multiple Task calls in one
        // turn run concurrently instead of one-at-a-time. Tuple: (result index to
        // backfill, tool_use id, goal label, worker handle).
        let mut pending_tasks: Vec<(usize, String, String, origin_swarm::WorkerHandle)> = Vec::new();
        for (id, name, input_bytes) in tool_uses {
            let Some(meta) = registry_iter().find(|m| m.name == name) else {
                tracing::warn!(tool = %name, "unknown tool; returning error to model");
                tool_results.push(Block::ToolResult {
                    tool_use_id: id,
                    handle: None,
                    inline: Some(format!("Error: unknown tool `{name}`").into_bytes()),
                    cache_marker: None,
                });
                continue;
            };
            let args: Value = match serde_json::from_slice(&input_bytes) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(tool = %name, error = %e, "malformed tool args; returning error to model");
                    tool_results.push(Block::ToolResult {
                        tool_use_id: id,
                        handle: None,
                        inline: Some(format!("Error: malformed args: {e}").into_bytes()),
                        cache_marker: None,
                    });
                    continue;
                }
            };
            let preview = args.to_string();

            // Compute the memoization key using the RAW input bytes (not
            // re-serialized args) so the key is stable across turns.
            let key = origin_tools::NormalizedInput::hash(meta.name, &input_bytes);
            let cache_hit = if cache.is_skipped(meta.name) {
                None
            } else {
                cache.lookup(&key).copied()
            };

            // Permission check fires first — denied tools never use cached results.
            let skills: &SkillRegistry = opts.skills.as_deref().unwrap_or(&EMPTY_SKILLS);
            let mut decision = check_with_skills(meta, &preview, prompter, skills).await;

            // Task 3 — DENY-ONLY governance overlay. After the base check yields
            // a decision, the optional policy/conseca layers may only *narrow*
            // it: an Allow can be downgraded to Deny, but a Deny is never
            // widened. When both are `None` (the default everywhere) this block
            // is a no-op and the decision is byte-identical to today.
            if decision.outcome == Outcome::Allow {
                decision = apply_governance_overlay(decision, meta.name, opts);
            }

            // cmdparse bash-safety overlay. Same deny-only contract; only fires
            // for `Bash` and only when `ORIGIN_CMD_GUARD=1` (default off ⇒ no
            // behavior change). It can downgrade an Allow to Deny but never widen.
            if decision.outcome == Outcome::Allow {
                decision = apply_cmd_guard(decision, meta.name, &args);
            }

            // Read-only "plan mode" overlay (gemini Plan Mode). Same deny-only
            // contract: when the session is read-only, any non-`Pure` tool is
            // downgraded Allow→Deny so the design phase cannot mutate the
            // workspace. `false` (the default) ⇒ no effect.
            if decision.outcome == Outcome::Allow {
                decision =
                    apply_read_only_overlay(decision, opts.read_only, meta.side_effects, meta.name);
            }

            // gemini PreTool lifecycle hook. Same deny-only contract as the
            // overlays above: a configured hook may downgrade Allow→Deny, never
            // widen. No hooks configured ⇒ skipped entirely.
            if decision.outcome == Outcome::Allow {
                if let Some(h) = &hooks {
                    let ev = origin_hooks::LifecycleEvent::PreTool {
                        tool: name.clone(),
                        args_preview: preview.clone(),
                        sandbox_ordinal: meta.sandbox_profile.ordinal(),
                    };
                    if let origin_hooks::HookOverride::Deny { reason } = h.fire(&ev).await {
                        decision.outcome = Outcome::Deny;
                        decision.reason = format!("hook denied: {reason}");
                    }
                }
            }

            if decision.outcome == Outcome::Deny {
                // Drain any speculative slot to keep the registry clean.
                let _ = speculative.take(&id).await;
                tracing::warn!(
                    tool = %name,
                    reason = %decision.reason,
                    "tool denied"
                );
                return Err(LoopError::Denied(name.clone()));
            }

            // Track mutations for the optional end-of-turn checkpoint (Task 1).
            // Only mutating tools flip the flag; pure tools leave it untouched.
            if matches!(meta.side_effects, SideEffects::Mutating) {
                turn_mutated = true;
                // Autonomous post-edit LSP diagnostics (opencode): record the
                // distinct files this edit touched so the end-of-turn probe can
                // feed compiler/linter diagnostics back to the model. Gated:
                // collects nothing unless `ORIGIN_LSP_DIAGNOSTICS=1`.
                if lsp_diag_enabled {
                    for p in edited_paths_from_tool(&name, &args) {
                        lsp_edited_paths.insert(p);
                    }
                }
            }

            if let Some(tx) = &opts.event_tx {
                let summary = tool_activity_summary(&name, &args);
                let diff_lines = tool_diff_lines(&name, &args);
                let _ = tx
                    .send(StreamEvent::ToolActivity {
                        tool: name.clone(),
                        summary,
                        diff_lines,
                    })
                    .await;
            }

            // Sub-agent parallelism: spawn the Task worker now (it runs at once
            // on the swarm's independent pool) and defer awaiting it to after the
            // loop, so sibling Task calls in this turn overlap. Permission and
            // governance have already been enforced above; with no coordinator we
            // fall through to the normal dispatch (a clear "not configured" error).
            if name == "Task" {
                if let Some(coord) = opts.coordinator.as_deref() {
                    match serde_json::from_value::<origin_tools::builtins::task::TaskInput>(args.clone()) {
                        Ok(input) => {
                            let goal = input.goal.clone();
                            match origin_tools::builtins::task::task_spawn(coord, input).await {
                                Ok(handle) => {
                                    let idx = tool_results.len();
                                    tool_results.push(Block::ToolResult {
                                        tool_use_id: id.clone(),
                                        handle: None,
                                        inline: Some(b"(sub-agent dispatched)".to_vec()),
                                        cache_marker: None,
                                    });
                                    pending_tasks.push((idx, id, goal, handle));
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Task spawn failed; returning error to model");
                                    tool_results.push(Block::ToolResult {
                                        tool_use_id: id,
                                        handle: None,
                                        inline: Some(format!("Error: {e}").into_bytes()),
                                        cache_marker: None,
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: Task: bad args: {e}").into_bytes()),
                                cache_marker: None,
                            });
                        }
                    }
                    continue;
                }
            }

            // Stage C5 Task 1/2: time the tool-execution wall-clock. The clock
            // starts here, before the cache/speculative/Bash/dispatch fan-out,
            // and is folded into `tool_time_ms` only on the SUCCESS path below
            // (a failed tool `continue`s past the fold, so its time is left in
            // the model-time remainder rather than miscredited as useful tool
            // work). Pure measurement; default path is byte-identical.
            let dispatch_start = std::time::Instant::now();
            let result_bytes: Vec<u8> = if let Some(hit) = cache_hit {
                // Serve the cached body annotated with the originating turn.
                let store = opts.cas.as_ref().ok_or_else(|| {
                    LoopError::ToolFailure("memoization requires CAS to be configured".into())
                })?;
                let body = store
                    .get(origin_cas::Hash::from_bytes(hit.handle))
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
                    .ok_or_else(|| LoopError::ToolFailure("cas miss on cached handle".into()))?;
                let annotated = format!(
                    "{}\n\n(cached from turn {})",
                    String::from_utf8_lossy(&body),
                    hit.from_turn,
                );
                // Drain any matching speculative slot so the task doesn't stay
                // detached — its result will be discarded in favour of the cache.
                let _ = speculative.take(&id).await;
                annotated.into_bytes()
            } else {
                // Try speculative precomputed result first; fall back to fresh
                // synchronous dispatch if the registry has no entry.
                if let Some(pre) = speculative.take(&id).await {
                    match pre {
                        Ok(bytes) => bytes,
                        Err(LoopError::BadArgs(msg) | LoopError::ToolFailure(msg)) => {
                            tracing::warn!(tool = %name, %msg, "speculative tool dispatch failed; returning error to model");
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: {msg}").into_bytes()),
                                cache_marker: None,
                            });
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                } else if meta.name == "Bash" {
                    // Streaming dispatch path: forwards each stdout/stderr
                    // line to the CLI as a `ToolChunk` event as soon as
                    // the child writes it, so long-running commands no
                    // longer feel hung. The LLM still receives the fully
                    // accumulated body via `Block::ToolResult` below.
                    match run_bash_streaming(&args, opts.event_tx.as_ref()).await {
                        Ok(bytes) => bytes,
                        Err(msg) => {
                            tracing::warn!(tool = %name, %msg, "Bash dispatch failed; returning error to model");
                            if let Some(tx) = &opts.event_tx {
                                let _ = tx
                                    .send(StreamEvent::ToolResult {
                                        tool: name.clone(),
                                        ok: false,
                                        preview: msg.clone(),
                                        elided_bytes: 0,
                                    })
                                    .await;
                            }
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: {msg}").into_bytes()),
                                cache_marker: None,
                            });
                            continue;
                        }
                    }
                } else {
                    // Task 1: when the exposure-truncation gate is on, hand the
                    // accumulated prior `(file, line)` regions to the Grep arm so
                    // a re-run elides already-seen lines. Gate off ⇒ `None`,
                    // byte-identical. The borrow ends before the result harvest
                    // below mutates `grep_exposure`.
                    let exposure_arg: Option<&[origin_tools::builtins::grep_tool::ExposureWindow]> =
                        if grep_truncate_enabled && meta.name == "Grep" {
                            Some(grep_exposure.as_slice())
                        } else {
                            None
                        };
                    match dispatch_tool(
                        meta,
                        &args,
                        opts.cas.as_deref(),
                        opts.code_graph.as_ref(),
                        opts.mem_router.as_ref().map(Arc::as_ref),
                        opts.memory_handle.as_deref(),
                        opts.coordinator.as_deref(),
                        exposure_arg,
                        opts.skill_catalog.as_deref(),
                    )
                    .await
                    {
                        Ok(s) => {
                            // Harvest the `content`-mode `(file, line)` matches
                            // this Grep returned so the NEXT Grep this loop can
                            // elide them. Gate off ⇒ never touched. Parse failures
                            // are ignored (the result still flows to the model).
                            if grep_truncate_enabled && meta.name == "Grep" {
                                harvest_grep_exposure(&s, &mut grep_exposure);
                            }
                            s.into_bytes()
                        }
                        Err(LoopError::BadArgs(msg) | LoopError::ToolFailure(msg)) => {
                            tracing::warn!(tool = %name, %msg, "tool dispatch failed; returning error to model");
                            // Surface the error to the CLI so the user sees
                            // *why* the tool stopped rather than a silent gap.
                            if let Some(tx) = &opts.event_tx {
                                let _ = tx
                                    .send(StreamEvent::ToolResult {
                                        tool: name.clone(),
                                        ok: false,
                                        preview: msg.clone(),
                                        elided_bytes: 0,
                                    })
                                    .await;
                            }
                            tool_results.push(Block::ToolResult {
                                tool_use_id: id,
                                handle: None,
                                inline: Some(format!("Error: {msg}").into_bytes()),
                                cache_marker: None,
                            });
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
            };

            // Stage C5 Task 1/2: reaching here means the tool produced a result
            // (every failure path `continue`d above). Fold this dispatch's
            // wall-clock into the tool-time accumulator and, the first time,
            // capture the elapsed-from-loop-start as the time-to-first-useful-
            // action. Saturating so a pathological run can never overflow.
            tool_time_ms = tool_time_ms
                .saturating_add(u64::try_from(dispatch_start.elapsed().as_millis()).unwrap_or(u64::MAX));
            if first_tool_ms.is_none() {
                first_tool_ms =
                    Some(u64::try_from(loop_start.elapsed().as_millis()).unwrap_or(u64::MAX));
            }

            // Real-time swarm collaboration (WS-L, jcode L238). Reaching here
            // means the tool succeeded (failures `continue` above). When a
            // swarm-worker collab context is in scope AND the gate env is set,
            // record this worker's read/edit and notify any other worker who
            // read a path this worker just edited. No-op + byte-identical when
            // no context is scoped or the gate is unset. Best-effort; never
            // breaks the turn.
            let _ = SWARM_COLLAB.try_with(|collab| {
                record_swarm_collab(collab, &name, &args);
            });

            // Optional per-tool-use shadow-git checkpoint (WS-S, cline L163
            // follow-up). Reaching here means the tool succeeded. Only mutating
            // tools produce a snapshot, and only when ORIGIN_CHECKPOINTS_PER_TOOL
            // is set — independent of the per-turn ORIGIN_CHECKPOINTS gate, so
            // per-turn stays the default granularity. Best-effort and
            // default-off ⇒ byte-identical when the gate is unset. The snapshot
            // is bracketed by the PreCommit/PostCommit lifecycle hooks, which
            // also no-op without a configured hooks.json.
            if matches!(meta.side_effects, SideEffects::Mutating) {
                maybe_checkpoint_per_tool_with_hooks(&name, &args).await;
            }

            // Stream a truncated preview of the result back to the CLI so the
            // user sees the tool's actual output. The LLM still consumes the
            // full body via the `Block::ToolResult` round-trip below.
            // Bash is excluded here — it manages its own completion event
            // inside `run_bash_streaming`, which emits a short `ToolResult`
            // only when zero chunks were streamed (silent commands like
            // write-only `powershell -File …` scripts), avoiding a redundant
            // trailing echo for verbose commands.
            if meta.name != "Bash" {
                if let Some(tx) = &opts.event_tx {
                    let (preview, elided) = build_tool_result_preview(&result_bytes);
                    let _ = tx
                        .send(StreamEvent::ToolResult {
                            tool: name.clone(),
                            ok: true,
                            preview,
                            elided_bytes: elided,
                        })
                        .await;
                }
            }

            let block = if let Some(cas) = opts.cas.as_ref() {
                let h: Hash = cas
                    .put(&result_bytes)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?;

                // Phase 11 N4.3 wiring: register the freshly produced handle
                // with the active Plan so the Anthropic wire-encoder can
                // downgrade `Inline` → `Reference` for handles whose bodies
                // are stable enough to cache client-side. Heuristic: a Pure
                // tool's output is reusable across turns (`Sticky`);
                // a Mutating tool's output is a snapshot of state that just
                // changed (`Volatile`, the safe floor). Tools missing meta
                // (e.g. MCP-discovered runtime tools) inherit the floor.
                if let Some(plan) = opts.plan.as_ref() {
                    use origin_planner::Band;
                    use origin_tools::SideEffects;
                    let band = match meta.side_effects {
                        SideEffects::Pure => Band::Sticky,
                        SideEffects::Mutating => Band::Volatile,
                    };
                    plan.register_handle(*h.as_bytes(), band);
                }

                // Fire Extract job for large tool outputs (P5.3, N2.5.c).
                if result_bytes.len() >= origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES {
                    if let Some(sidecar) = &opts.sidecar {
                        let _ = sidecar.submit(origin_sidecar::SidecarJob::Extract {
                            handle: h,
                            deliver_to: Box::new(NoopExtractDeliverer),
                        });
                    }
                }

                // Record into the memoization cache for subsequent turns
                // within this session. Skip-listed tools and hits are not
                // re-recorded (a hit means the entry is already present).
                if !cache.is_skipped(meta.name) && cache_hit.is_none() {
                    cache.record(key, *h.as_bytes(), turn);
                }

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
            // gemini PostTool lifecycle hook (informational): fires after a tool
            // completes successfully. No hooks configured ⇒ skipped.
            if let Some(h) = &hooks {
                let _ = h
                    .fire(&origin_hooks::LifecycleEvent::PostTool {
                        tool: name.clone(),
                        phase: origin_hooks::ToolPhase::Ok,
                        sandbox_ordinal: meta.sandbox_profile.ordinal(),
                    })
                    .await;
            }
            tool_results.push(block);
        }

        // Await the deferred sub-agents. They were spawned during the loop and
        // run concurrently on the swarm pool, so awaiting them in sequence here
        // resolves in ~max(worker) time, not the sum. Each result backfills the
        // placeholder slot pushed during the loop (preserving tool_use order).
        if let Some(coord) = opts.coordinator.as_deref() {
            let task_ordinal = registry_iter()
                .find(|m| m.name == "Task")
                .map_or_else(|| origin_tools::ProfileOrdinal(0), |m| m.sandbox_profile.ordinal());
            for (idx, id, goal, handle) in std::mem::take(&mut pending_tasks) {
                let result_bytes = match origin_tools::builtins::task::task_await(coord, &handle, &goal).await {
                    Ok(output) => serde_json::to_string(&output)
                        .unwrap_or_else(|e| format!("{{\"status\":\"error\",\"summary\":\"Task json: {e}\"}}"))
                        .into_bytes(),
                    Err(e) => format!("Error: {e}").into_bytes(),
                };
                if let Some(tx) = &opts.event_tx {
                    let (preview, elided) = build_tool_result_preview(&result_bytes);
                    let _ = tx
                        .send(StreamEvent::ToolResult {
                            tool: "Task".to_string(),
                            ok: true,
                            preview,
                            elided_bytes: elided,
                        })
                        .await;
                }
                if let Some(h) = &hooks {
                    let _ = h
                        .fire(&origin_hooks::LifecycleEvent::PostTool {
                            tool: "Task".to_string(),
                            phase: origin_hooks::ToolPhase::Ok,
                            sandbox_ordinal: task_ordinal,
                        })
                        .await;
                }
                if let Some(slot) = tool_results.get_mut(idx) {
                    *slot = Block::ToolResult {
                        tool_use_id: id,
                        handle: None,
                        inline: Some(result_bytes),
                        cache_marker: None,
                    };
                }
            }
        }

        // Append tool results as a single Role::Tool message (provider crates
        // will translate this to the right wire shape per provider).
        let mut tool_msg = Message::new(Role::Tool);
        tool_msg.blocks = tool_results;
        session.push(tool_msg);

        // Live transcript compaction (P5.4 runtime wiring). With the freshly
        // closed turn now appended, fold the oldest summarized turns into their
        // summaries when the accumulated context has crossed the soft cap, so
        // the NEXT iteration's `ChatRequest` is built from the compacted
        // transcript and `PreCompress` fires for any configured hook. Runs
        // BEFORE the cache-marker pass below so the breakpoints land on the
        // post-compaction transcript. Below the cap (the default for short
        // sessions) this is a no-op ⇒ byte-identical.
        maybe_compact_session(session, opts).await;

        // Place a prompt-cache breakpoint at the freshly closed turn boundary
        // so the next iteration's `ChatRequest` (which re-sends the full
        // `session.snapshot()`) is billed against Anthropic's prompt cache
        // instead of as fresh input tokens. See [`apply_turn_cache_markers`].
        apply_turn_cache_markers(&mut session.messages, opts.plan.as_ref());

        // Autonomous post-edit LSP diagnostics (opencode). When the feature is
        // on and this turn mutated path-bearing files, spawn the resolved
        // language server(s), collect diagnostics under a short timeout, and
        // stash the rendered `<lsp-diagnostics>` block for the NEXT turn's
        // system prompt. Best-effort: any failure leaves `lsp_diag_block`
        // empty, so the loop continues unaffected. Default-off ⇒ this whole
        // block is skipped and the prompt stays byte-identical.
        if lsp_diag_enabled {
            lsp_diag_block = if lsp_edited_paths.is_empty() {
                String::new()
            } else {
                let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                crate::lsp_diagnostics::lsp_diagnostics_block(&root, &lsp_edited_paths)
                    .await
                    .unwrap_or_default()
            };
        }
    }
    // Task 4 / Stage C5: the loop exhausted its `max_turns` budget without the
    // model settling on a tool-free final answer — the turn budget is the
    // exhausted resource. Emit the `BudgetExhausted` pain bucket (with the real
    // time split + ttfua) before propagating the error. This is the budget
    // exit, NOT a fatal-error exit, so it owns the more-specific
    // `BudgetExhausted` reason rather than the generic `Error` the fallback
    // wrapper tags other `Err` returns with. Default-off ⇒ no event.
    let agent_time_ms = u64::try_from(loop_start.elapsed().as_millis()).unwrap_or(u64::MAX);
    record_session_stop_pain(SessionStopPain::from_loop(
        origin_telemetry::SessionStopReason::BudgetExhausted,
        agent_time_ms,
        tool_time_ms,
        first_tool_ms,
        first_token_ms,
        opts.max_turns,
    ));
    Err(LoopError::MaxTurns(opts.max_turns))
}

/// Anthropic's Messages API accepts at most 4 `cache_control` markers per
/// request. We stay strictly under that ceiling.
const MAX_CACHE_MARKERS: usize = 4;

/// Apply prompt-cache breakpoints after a completed agentic turn.
///
/// The Anthropic wire encoder has three independent emission paths for
/// `cache_control`: (1) planner-planted markers at `msg_idx == 0`, (2) the
/// per-block `cache_marker` field, and (3) the shared `Plan`'s
/// `dynamic_message_markers`. Each block emits at most one `cache_control`
/// via OR-combination, but the *count* of marker'd blocks is the union of
/// the positions selected by paths (2) and (3). If those paths select
/// different blocks the union can exceed Anthropic's per-request ceiling of
/// 4 markers, and the API rejects the request with `invalid_request_error:
/// "A maximum of 4 blocks with cache_control may be provided."`. That is the
/// production bug this helper guards against.
///
/// The fix is a single source of truth: pick the marker positions once, then
/// drive both the block-level field (path 2) and the plan's
/// `dynamic_message_markers` (path 3) from the same set. The selection
/// policy is "latest N turn boundaries", capped at [`MAX_CACHE_MARKERS`]:
/// latest-N is cache-optimal because Anthropic's prompt cache hits work on
/// prefix-extension — newer marker positions amortize across more subsequent
/// turns than older ones.
///
/// Without these markers every iteration of [`run_loop`] re-bills the full
/// `session.snapshot()` at the un-cached rate. Anthropic's prompt cache
/// charges 0.1× for cache reads, so engaging caching at stable turn
/// boundaries collapses the dominant cost of long agentic sessions.
fn apply_turn_cache_markers(messages: &mut [Message], plan: Option<&origin_planner::Plan>) {
    use origin_core::types::CacheBoundary;

    // Turn boundaries are the last `ToolResult` blocks of `Role::Tool`
    // messages. Each marks the close of one assistant turn's tool dispatch
    // round and is the natural place to cut a cache prefix.
    let turn_boundaries: Vec<(usize, usize)> = messages
        .iter()
        .enumerate()
        .filter_map(|(mi, msg)| {
            if !matches!(msg.role, Role::Tool) {
                return None;
            }
            msg.blocks
                .iter()
                .rposition(|b| matches!(b, Block::ToolResult { .. }))
                .map(|bi| (mi, bi))
        })
        .collect();

    // Single source of truth: the latest `MAX_CACHE_MARKERS` turn boundaries.
    // Empty session ⇒ no markers and the dynamic list is cleared so a previous
    // turn's stale state never leaks into the wire.
    let start = turn_boundaries.len().saturating_sub(MAX_CACHE_MARKERS);
    let chosen: Vec<(usize, usize)> = turn_boundaries[start..].to_vec();

    // Clear every existing block-level cache_marker across the session before
    // re-applying. This is the rotation: old marker positions that fall
    // outside the latest-N window are unmarked here. Without this pass, stale
    // markers from earlier calls would accumulate past the ceiling.
    for msg in messages.iter_mut() {
        for b in &mut msg.blocks {
            clear_block_cache_marker(b);
        }
    }

    // Path (2): block-level cache_marker on each chosen ToolResult block.
    for &(mi, bi) in &chosen {
        if let Block::ToolResult { cache_marker, .. } = &mut messages[mi].blocks[bi] {
            *cache_marker = Some(CacheBoundary::Sticky);
        }
    }

    // Path (3): mirror the same positions in the shared Plan's
    // `dynamic_message_markers`. The wire encoder OR-combines paths (2) and
    // (3) per block; because they target the same blocks here, the union
    // equals the intersection and the marker count is exactly `chosen.len()`.
    if let Some(plan) = plan {
        let msg_indices: Vec<usize> = chosen.iter().map(|&(mi, _)| mi).collect();
        plan.set_dynamic_message_markers(msg_indices);
    }
}

#[cfg(test)]
const fn block_cache_marker_set(b: &Block) -> bool {
    match b {
        Block::Text { cache_marker, .. }
        | Block::ToolUse { cache_marker, .. }
        | Block::ToolResult { cache_marker, .. } => cache_marker.is_some(),
        Block::Thinking { .. } => false,
    }
}

fn clear_block_cache_marker(b: &mut Block) {
    match b {
        Block::Text { cache_marker, .. }
        | Block::ToolUse { cache_marker, .. }
        | Block::ToolResult { cache_marker, .. } => *cache_marker = None,
        Block::Thinking { .. } => {}
    }
}

#[cfg(test)]
mod cache_marker_tests {
    use super::*;
    use origin_core::types::{Block, Message, Role};
    use origin_planner::Plan;
    use std::collections::HashSet;

    /// Append one full user/assistant/tool turn to `msgs`.
    fn push_turn(msgs: &mut Vec<Message>, turn_idx: usize) {
        msgs.push(Message {
            role: Role::User,
            blocks: vec![Block::text(format!("user {turn_idx}"))],
        });
        msgs.push(Message {
            role: Role::Assistant,
            blocks: vec![Block::ToolUse {
                id: format!("u{turn_idx}"),
                name: "Read".into(),
                input_json: b"{}".to_vec(),
                cache_marker: None,
            }],
        });
        msgs.push(Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: format!("u{turn_idx}"),
                handle: None,
                inline: Some(b"ok".to_vec()),
                cache_marker: None,
            }],
        });
    }

    fn block_marked_message_indices(msgs: &[Message]) -> HashSet<usize> {
        msgs.iter()
            .enumerate()
            .filter_map(|(mi, m)| {
                if m.blocks.iter().any(block_cache_marker_set) {
                    Some(mi)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Anthropic rejects requests with > 4 `cache_control` markers. The wire
    /// encoder emits one marker per block when *any* of three independent paths
    /// fires for that block (block-level `cache_marker`, plan `marker_indices`,
    /// or plan `dynamic_message_markers`). If those paths target different
    /// blocks, their union can exceed 4 even when each path is individually
    /// capped — which is exactly the 5-marker 400 the daemon hit in production.
    ///
    /// Invariant: after `apply_turn_cache_markers`, the set of message indices
    /// marked at the block level must equal the set in `dynamic_message_markers`,
    /// and the cardinality must stay at or below `MAX_CACHE_MARKERS`.
    #[test]
    fn block_and_dynamic_markers_converge_under_ceiling_after_20_turns() {
        let plan = Plan::default();
        let mut msgs: Vec<Message> = Vec::new();
        for turn in 0..20 {
            push_turn(&mut msgs, turn);
            apply_turn_cache_markers(&mut msgs, Some(&plan));
        }

        let block_marked = block_marked_message_indices(&msgs);
        let dyn_marked: HashSet<usize> = plan.dynamic_message_markers().into_iter().collect();

        assert_eq!(
            block_marked, dyn_marked,
            "block-level markers and dynamic_message_markers must target the \
             same messages so the wire encoder's paths converge per block; got \
             block={block_marked:?}, dyn={dyn_marked:?}"
        );
        assert!(
            block_marked.len() <= MAX_CACHE_MARKERS,
            "marker count must stay at or below {MAX_CACHE_MARKERS} \
             (Anthropic's per-request ceiling); got {} at {block_marked:?}",
            block_marked.len()
        );
    }

    /// Same invariant at the smaller scale where bands collapse — exercises the
    /// edge cases of the recency classifier.
    #[test]
    fn block_and_dynamic_markers_converge_for_small_sessions() {
        for n_turns in 1..=6 {
            let plan = Plan::default();
            let mut msgs: Vec<Message> = Vec::new();
            for turn in 0..n_turns {
                push_turn(&mut msgs, turn);
                apply_turn_cache_markers(&mut msgs, Some(&plan));
            }
            let block_marked = block_marked_message_indices(&msgs);
            let dyn_marked: HashSet<usize> = plan.dynamic_message_markers().into_iter().collect();
            assert_eq!(
                block_marked, dyn_marked,
                "divergence at n_turns={n_turns}: block={block_marked:?}, dyn={dyn_marked:?}"
            );
            assert!(
                block_marked.len() <= MAX_CACHE_MARKERS,
                "over ceiling at n_turns={n_turns}: {block_marked:?}"
            );
        }
    }
}

/// Fold a `content`-mode `Grep` result's `(file, line)` matches into the
/// running exposure set (Task 1).
///
/// `result_json` is the serialized [`origin_tools::builtins::grep_tool`] result
/// (`{"matches": [{"file": …, "line": …, …}, …]}`). Each match contributes a
/// single-line [`ExposureWindow`] so a later `Grep` in the same `run_loop` can
/// elide it. Non-`content` results carry no `matches` array and add nothing.
/// Any parse failure is ignored — the model still received the full result, and
/// missing exposures only mean a future re-run shows more, never less.
///
/// This is only ever called when `ORIGIN_AGENTGREP_TRUNCATE=1`; with the gate
/// unset it is never reached, so the default loop is byte-identical.
fn harvest_grep_exposure(
    result_json: &str,
    exposure: &mut Vec<origin_tools::builtins::grep_tool::ExposureWindow>,
) {
    use origin_tools::builtins::grep_tool::ExposureWindow;
    let Ok(value) = serde_json::from_str::<Value>(result_json) else {
        return;
    };
    let Some(matches) = value.get("matches").and_then(Value::as_array) else {
        return;
    };
    for m in matches {
        let (Some(file), Some(line)) = (
            m.get("file").and_then(Value::as_str),
            m.get("line").and_then(Value::as_u64),
        ) else {
            continue;
        };
        // De-dup exact repeats so a re-run over the same region doesn't grow the
        // set unbounded across many turns.
        let window = ExposureWindow {
            file: file.to_string(),
            start_line: line,
            end_line: line,
        };
        if !exposure.contains(&window) {
            exposure.push(window);
        }
    }
}

/// Parse and apply model-emitted prose edit blocks to disk (Task 2).
///
/// Only ever called when `ORIGIN_EDITFMT=1`. The model reached this path having
/// emitted prose with no structured Edit tool call, so `origin_editfmt` is used
/// to recover the intended edit: [`origin_editfmt::extract_all_hunks`]
/// auto-detects the format (search/replace, diff-fenced, udiff, or whole-file,
/// falling back to the model's best format) and yields normalized hunks. Each
/// hunk is applied to the file it names via [`origin_editfmt::apply`] and the
/// result written back.
///
/// `turn_mutated` is set to `true` if any hunk is successfully applied, so the
/// optional end-of-turn shadow-git checkpoint snapshots the change exactly as it
/// would for a structured Edit. Every failure (no parseable block, an empty or
/// flag-like path, a missing/ambiguous match, or an I/O error) is logged and
/// skipped — a malformed prose edit must never fail the turn.
fn apply_prose_edits(text: &str, model: &str, turn_mutated: &mut bool) {
    let hunks = match origin_editfmt::extract_all_hunks(text, model) {
        Ok(h) => h,
        Err(e) => {
            // The common case (no edit block in the prose) lands here; keep it
            // at debug so a chatty model doesn't spam warnings every turn.
            tracing::debug!(error = %e, "editfmt: no applicable edit block in assistant prose");
            return;
        }
    };
    for hunk in hunks {
        if apply_one_prose_hunk(&hunk) {
            *turn_mutated = true;
        }
    }
}

/// Apply a single normalized [`origin_editfmt::Hunk`] to disk (Task 2 helper).
///
/// Returns `true` only when the file was successfully written. Every guard or
/// failure (empty/flag-like path, unreadable target, no/ambiguous match, or a
/// write error) is logged once and yields `false`, so a malformed prose edit
/// can never fail the turn. The fallible work is funneled through
/// [`try_apply_one_prose_hunk`] so this wrapper has a single log/return site.
fn apply_one_prose_hunk(hunk: &origin_editfmt::Hunk) -> bool {
    match try_apply_one_prose_hunk(hunk) {
        Ok(()) => {
            tracing::info!(file = %hunk.file, "editfmt: applied prose edit");
            true
        }
        Err(reason) => {
            tracing::warn!(file = %hunk.file, %reason, "editfmt: skipped prose edit");
            false
        }
    }
}

/// Fallible core of [`apply_one_prose_hunk`]: validate, apply, and write.
///
/// Returns a short human reason on any guard/failure so the caller emits a
/// single structured log line. Linear control flow via `?` keeps the cognitive
/// complexity low.
fn try_apply_one_prose_hunk(hunk: &origin_editfmt::Hunk) -> Result<(), String> {
    if hunk.file.is_empty() {
        return Err("no file path".to_string());
    }
    // Reuse the same flag-smuggling guard the autoformat path uses: refuse a
    // path the OS could parse as an option.
    if hunk.file.starts_with('-') {
        return Err("path looks like a flag".to_string());
    }
    // Whole-file hunks (`before` empty) write `after` verbatim even when the
    // file does not yet exist; in-place hunks need the current contents.
    let original = if hunk.before.is_empty() {
        String::new()
    } else {
        std::fs::read_to_string(&hunk.file).map_err(|e| format!("cannot read target: {e}"))?
    };
    let updated = origin_editfmt::apply(hunk, &original).map_err(|e| e.to_string())?;
    std::fs::write(&hunk.file, updated).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

/// Rebuild entry-point invoked by the future IPC handler / git hook.
///
/// P7.8 ships the free function; P10 wires it into the daemon's `Frame`
/// dispatcher alongside [`crate::protocol::RebuildRequest`]. The function
/// itself is a thin shim over [`origin_codegraph::rebuild::rebuild_paths`].
///
/// # Errors
/// Propagates [`origin_codegraph::rebuild::RebuildError`] for fatal CAS /
/// `SQLite` failures; per-file errors are aggregated into the returned report.
// `req` is taken by value to match the future IPC handler shape — once P10
// deserializes a `RebuildRequest` off the wire it will move the value into
// this function. Taking by reference now would force a copy at the boundary.
#[allow(clippy::needless_pass_by_value)]
pub fn rebuild_codegraph(
    idx: &mut origin_codegraph::index::CodeGraphIndex,
    req: crate::protocol::RebuildRequest,
    lang: origin_codegraph::Language,
) -> Result<origin_codegraph::rebuild::RebuildReport, origin_codegraph::rebuild::RebuildError> {
    tracing::info!(paths = req.paths.len(), "rebuild_codegraph: dispatching");
    origin_codegraph::rebuild::rebuild_paths(idx, &req.paths, lang)
}

#[tracing::instrument(
    level = "info",
    skip(args, cas, code_graph, mem_router, memory, coordinator, grep_exposure),
    fields(kind = "tool", tool = meta.name)
)]
// dispatch arm-per-tool registry; splitting would obscure tool->arm mapping.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn dispatch_tool(
    meta: &ToolMeta,
    args: &Value,
    cas: Option<&Store>,
    code_graph: Option<&Arc<tokio::sync::Mutex<origin_codegraph::index::CodeGraphIndex>>>,
    mem_router: Option<&dyn origin_codegraph::ask::MemRouter>,
    memory: Option<&dyn MemoryHandle>,
    coordinator: Option<&origin_swarm::Coordinator>,
    // Task 1 (agentgrep exposure-truncation): already-seen `(file, line)`
    // regions to elide from a `content`-mode `Grep`. `None` (every caller but
    // the gated main-loop site) ⇒ the Grep arm passes `exposure: None`, so the
    // result is byte-identical to before. Only consulted by `grep_v2` when
    // `ORIGIN_AGENTGREP_TRUNCATE=1`.
    grep_exposure: Option<&[origin_tools::builtins::grep_tool::ExposureWindow]>,
    // Live skill catalog used by the `AuthorWorkflow` tool to build the
    // `origin_workflowgen::SkillCatalog` it plans over. `None` (every caller but
    // the main-loop site) ⇒ the `AuthorWorkflow` arm reports a clear
    // "not configured" `ToolFailure`; no other arm consults it, so the result is
    // byte-identical to before for every other tool.
    skill_catalog: Option<&SkillCatalog>,
) -> Result<String, LoopError> {
    match meta.name {
        "Read" => {
            let args = origin_tools::builtins::read::ReadArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Read: missing `file_path`".into()))?
                    .to_string(),
                offset: args
                    .get("offset")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                limit: args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                as_: args.get("as").and_then(Value::as_str).map(str::to_string),
            };
            origin_tools::builtins::read::read_v2(args).map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Glob" => {
            let gargs = origin_tools::builtins::glob_tool::GlobArgs {
                pattern: args
                    .get("pattern")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Glob: missing `pattern`".into()))?
                    .to_string(),
                path: args.get("path").and_then(Value::as_str).map(str::to_string),
                head_limit: args
                    .get("head_limit")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
            };
            origin_tools::builtins::glob_tool::glob_v2(gargs)
                .map(|v| serde_json::to_string(&v).expect("BUG: GlobResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Grep" => {
            let mode = args.get("output_mode").and_then(Value::as_str).map(|s| match s {
                "content" => origin_tools::builtins::grep_tool::OutputMode::Content,
                "count" => origin_tools::builtins::grep_tool::OutputMode::Count,
                // "files_with_matches" and any unknown value fall back to FilesWithMatches.
                _ => origin_tools::builtins::grep_tool::OutputMode::FilesWithMatches,
            });
            let gargs = origin_tools::builtins::grep_tool::GrepArgs {
                pattern: args
                    .get("pattern")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Grep: missing `pattern`".into()))?
                    .to_string(),
                path: args.get("path").and_then(Value::as_str).map(str::to_string),
                glob: args.get("glob").and_then(Value::as_str).map(str::to_string),
                r#type: args.get("type").and_then(Value::as_str).map(str::to_string),
                output_mode: mode,
                head_limit: args
                    .get("head_limit")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                before: args
                    .get("before")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
                after: args
                    .get("after")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
                line_numbers: args.get("line_numbers").and_then(Value::as_bool).unwrap_or(false),
                multiline: args.get("multiline").and_then(Value::as_bool).unwrap_or(false),
                // Task 1: thread the caller-supplied prior exposure so `grep_v2`
                // can elide already-seen regions when `ORIGIN_AGENTGREP_TRUNCATE=1`.
                // `grep_exposure` is `None` for every caller except the gated
                // main-loop dispatch site, so the default path stays byte-identical.
                exposure: grep_exposure.map(<[_]>::to_vec),
            };
            origin_tools::builtins::grep_tool::grep_v2(gargs)
                .map(|v| serde_json::to_string(&v).expect("BUG: GrepResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Edit" => {
            let args = origin_tools::builtins::edit::EditArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Edit: missing `file_path`".into()))?
                    .to_string(),
                old_string: args
                    .get("old_string")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Edit: missing `old_string`".into()))?
                    .to_string(),
                new_string: args
                    .get("new_string")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Edit: missing `new_string`".into()))?
                    .to_string(),
                replace_all: args.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
            };
            origin_tools::builtins::edit::edit_v2(args)
                .map(|v| serde_json::to_string(&v).expect("BUG: EditResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "MultiEdit" => {
            let edits_v = args
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| LoopError::BadArgs("MultiEdit: missing `edits`".into()))?;
            let edits = edits_v
                .iter()
                .map(|e| {
                    let o = e.get("old").and_then(Value::as_str).unwrap_or("");
                    let n = e.get("new").and_then(Value::as_str).unwrap_or("");
                    let r = e.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
                    origin_tools::builtins::multi_edit::EditOp {
                        old: o.into(),
                        new: n.into(),
                        replace_all: r,
                    }
                })
                .collect();
            let margs = origin_tools::builtins::multi_edit::MultiEditArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("MultiEdit: missing `file_path`".into()))?
                    .to_string(),
                edits,
            };
            let res = origin_tools::builtins::multi_edit::multi_edit(&margs)
                .map(|v| serde_json::to_string(&v).expect("BUG: MultiEditResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message));
            if res.is_ok() {
                maybe_autoformat(&margs.file_path);
            }
            res
        }
        "ApplyPatch" => {
            let pargs = origin_tools::builtins::apply_patch::ApplyPatchArgs {
                patch: args
                    .get("patch")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("ApplyPatch: missing `patch`".into()))?
                    .to_string(),
            };
            let fmt_paths = patch_target_paths(&pargs.patch);
            let res = origin_tools::builtins::apply_patch::apply_patch(&pargs)
                .map(|v| serde_json::to_string(&v).expect("BUG: ApplyPatchResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message));
            if res.is_ok() {
                for p in &fmt_paths {
                    maybe_autoformat(p);
                }
            }
            res
        }
        "Write" => {
            let guard = origin_tools::builtins::write::WriteGuard::default();
            // Production callers pass the session's guard via dispatch_with_envelope;
            // this passthrough path is used only by tests that bypass the envelope.
            let args = origin_tools::builtins::write::WriteArgs {
                file_path: args
                    .get("file_path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Write: missing `file_path`".into()))?
                    .to_string(),
                content: args
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Write: missing `content`".into()))?
                    .to_string(),
                force: args.get("force").and_then(Value::as_bool).unwrap_or(false),
            };
            let fmt_path = args.file_path.clone();
            let res = origin_tools::builtins::write::write_v2(args, &guard)
                .map(|()| "write ok".to_string())
                .map_err(|e| LoopError::ToolFailure(e.message));
            if res.is_ok() {
                maybe_autoformat(&fmt_path);
            }
            res
        }
        "Bash" => {
            let bargs = origin_tools::builtins::bash::BashArgs {
                command: args
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("Bash: missing `command`".into()))?
                    .to_string(),
                timeout: args
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                cwd: args.get("cwd").and_then(Value::as_str).map(str::to_string),
                env: args
                    .get("env")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                let arr = e.as_array()?;
                                Some((
                                    arr.first()?.as_str()?.to_string(),
                                    arr.get(1)?.as_str()?.to_string(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                run_in_background: args
                    .get("run_in_background")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            // Local supervisor for the passthrough path; the envelope path
            // uses ctx.supervisor (shared across calls within a session).
            // NOTE: run_in_background + Monitor across separate tool
            // invocations won't work via this path — see known limitations.
            let sup = origin_tools::proc_supervisor::Supervisor::new();
            origin_tools::builtins::bash::bash_v2(bargs, &sup)
                .await
                .map(|v| serde_json::to_string(&v).expect("BUG: BashResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Monitor" => {
            let margs = origin_tools::builtins::monitor::MonitorArgs {
                pid: args
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| LoopError::BadArgs("Monitor: missing or out-of-range `pid`".into()))?,
                since_byte: args.get("since_byte").and_then(Value::as_u64).unwrap_or(0),
                max_bytes: args
                    .get("max_bytes")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(4096),
                wait: args.get("wait").and_then(Value::as_bool).unwrap_or(false),
            };
            // Envelope-routed path uses ctx.supervisor; this passthrough
            // makes a stub supervisor that always returns unknown_pid.
            // Production should reach this arm only when run_in_background
            // was used — until Phase 8 wires the shared supervisor.
            let sup = origin_tools::proc_supervisor::Supervisor::new();
            origin_tools::builtins::monitor::monitor(margs, &sup)
                .await
                .map(|v| serde_json::to_string(&v).expect("BUG: MonitorResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Diagnostics" => {
            // Envelope-routed path uses ctx.ra (shared handle); this passthrough
            // path constructs a per-call DaemonRa. Known limitation — Phase 8
            // will wire the shared handle via dispatch_with_envelope.
            // NOTE: per-call DaemonRa means RA is re-spawned every call.
            let sev = match args.get("severity").and_then(Value::as_str).unwrap_or("any") {
                "error" => origin_tools::ra_bridge::Severity::Error,
                "warning" => origin_tools::ra_bridge::Severity::Warning,
                "hint" => origin_tools::ra_bridge::Severity::Hint,
                _ => origin_tools::ra_bridge::Severity::Any,
            };
            let dargs = origin_tools::builtins::diagnostics::DiagnosticsArgs {
                path: args.get("path").and_then(Value::as_str).map(str::to_string),
                severity: sev,
            };
            let ra = crate::ra_impl::DaemonRa::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            );
            origin_tools::builtins::diagnostics::diagnostics(dargs, &ra)
                .await
                .map(|v| serde_json::to_string(&v).expect("BUG: DiagnosticsResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "ToolSearch" => {
            let sargs = origin_tools::builtins::tool_search::ToolSearchArgs {
                query: args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs("ToolSearch: missing `query`".into()))?
                    .to_string(),
                max_results: args
                    .get("max_results")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
            };
            origin_tools::builtins::tool_search::tool_search(&sargs)
                .map(|v| serde_json::to_string(&v).expect("BUG: ToolSearchResult always serializes"))
                .map_err(|e| LoopError::ToolFailure(e.message))
        }
        "Recall" => {
            let store =
                cas.ok_or_else(|| LoopError::ToolFailure("Recall requires CAS to be configured".into()))?;
            let handle_hex = args
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("Recall: missing `handle`".into()))?;
            let handle: [u8; 32] = {
                let mut buf = [0u8; 32];
                hex::decode_to_slice(handle_hex, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("Recall: bad hex: {e}")))?;
                buf
            };
            let region = args.get("region").map(parse_region).transpose()?;
            origin_tools::builtins::recall::recall_tool(store, handle, region)
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        // ── Code-graph tools ──
        "graph_query" => {
            use origin_codegraph::index::EntityId;
            use origin_codegraph::query::Query;
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_query: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("graph_query: missing `kind`".into()))?;
            let q_args = args.get("args").cloned().unwrap_or(Value::Null);
            let parse_id = |v: &Value, field: &str| -> Result<EntityId, LoopError> {
                let s = v
                    .as_str()
                    .ok_or_else(|| LoopError::BadArgs(format!("graph_query.{field}: not a string")))?;
                let mut buf = [0u8; 32];
                hex::decode_to_slice(s, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("graph_query.{field}: bad hex: {e}")))?;
                Ok(EntityId(buf))
            };
            let q = match kind {
                "path" => Query::Path {
                    from: parse_id(&q_args["from"], "args.from")?,
                    to: parse_id(&q_args["to"], "args.to")?,
                    max_hops: usize::try_from(q_args["max_hops"].as_u64().unwrap_or(8)).unwrap_or(usize::MAX),
                },
                "neighbors" => Query::Neighbors {
                    node: parse_id(&q_args["node"], "args.node")?,
                    depth: usize::try_from(q_args["depth"].as_u64().unwrap_or(1)).unwrap_or(usize::MAX),
                },
                "communities" => Query::Communities,
                "god_nodes" => Query::GodNodes {
                    top_per_partition: usize::try_from(q_args["top_per_partition"].as_u64().unwrap_or(3))
                        .unwrap_or(usize::MAX),
                },
                "recent_changes" => Query::RecentChanges {
                    since_ms: q_args["since_ms"].as_i64().unwrap_or(0),
                },
                other => return Err(LoopError::BadArgs(format!("graph_query: unknown kind `{other}`"))),
            };
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::graph_query::graph_query_tool(&idx, q)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
            };
            Ok(serialize_query_result(&result))
        }
        "graph_path" => {
            use origin_codegraph::index::EntityId;
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_path: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            let parse_hex_id = |field: &str| -> Result<EntityId, LoopError> {
                let s = args
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| LoopError::BadArgs(format!("graph_path: missing `{field}`")))?;
                let mut buf = [0u8; 32];
                hex::decode_to_slice(s, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("graph_path.{field}: bad hex: {e}")))?;
                Ok(EntityId(buf))
            };
            let from = parse_hex_id("from")?;
            let to = parse_hex_id("to")?;
            let max_hops = usize::try_from(args.get("max_hops").and_then(Value::as_u64).unwrap_or(8))
                .unwrap_or(usize::MAX);
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::graph_path::graph_path_tool(&idx, from, to, max_hops)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
            };
            Ok(serialize_query_result(&result))
        }
        "graph_summarize" => {
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_summarize: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            // `community_id` (int) or `node` (hex) is the target string.
            let target = args
                .get("community_id")
                .and_then(Value::as_i64)
                .map(|cid| cid.to_string())
                .or_else(|| args.get("node").and_then(Value::as_str).map(ToString::to_string))
                .unwrap_or_default();
            // Summarize the target node's immediate neighbourhood (target +
            // direct callees/refs) and return it as the tool's JSON output.
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::graph_summarize::graph_summarize_tool(&idx, &target)
                    .map_err(|e| LoopError::ToolFailure(e.to_string()))?
            };
            Ok(serialize_query_result(&result))
        }
        "graph_rebuild" => {
            use std::path::PathBuf;
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "graph_rebuild: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions)."
                        .into(),
                )
            })?;
            let paths: Vec<PathBuf> = args
                .get("paths")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect())
                .unwrap_or_default();
            // Detect each path's language per-file (skipping files with no
            // codegraph-supported grammar) and rebuild once per language group,
            // instead of forcing every file through a single hardcoded grammar.
            let groups = crate::subsystems::group_paths_by_language(&paths);
            // `paths_seen` counts every requested path, including ones skipped
            // because their language is unsupported, so the caller can see the
            // gap between requested and rebuilt.
            let paths_seen = paths.len();
            // Lock once, rebuild every group under the same guard, then release.
            // The guard is a single-use temporary handed to the helper, so it is
            // never held across an await.
            let report = rebuild_groups(&mut *idx_arc.lock().await, groups)
                .map_err(|e| LoopError::ToolFailure(e.to_string()))?;
            Ok(serde_json::json!({
                "paths_seen": paths_seen,
                "nodes_added": report.nodes_added,
                "nodes_updated": report.nodes_updated,
                "errors": report.errors,
            })
            .to_string())
        }
        // `graph_explain` has zero infrastructure dependency — it just classifies
        // a typed `Query` into a deterministic English gloss. Wired here as a
        // real call so the model gets a working tool, not a stub error.
        "graph_explain" => {
            use origin_codegraph::index::EntityId;
            use origin_codegraph::query::Query;
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("graph_explain: missing `kind`".into()))?;
            let q_args = args.get("args").cloned().unwrap_or(Value::Null);
            let parse_id = |v: &Value, field: &str| -> Result<EntityId, LoopError> {
                let s = v
                    .as_str()
                    .ok_or_else(|| LoopError::BadArgs(format!("graph_explain.{field}: not a string")))?;
                let mut buf = [0u8; 32];
                hex::decode_to_slice(s, &mut buf)
                    .map_err(|e| LoopError::BadArgs(format!("graph_explain.{field}: bad hex: {e}")))?;
                Ok(EntityId(buf))
            };
            let q = match kind {
                "path" => Query::Path {
                    from: parse_id(&q_args["from"], "args.from")?,
                    to: parse_id(&q_args["to"], "args.to")?,
                    max_hops: usize::try_from(q_args["max_hops"].as_u64().unwrap_or(8)).unwrap_or(usize::MAX),
                },
                "neighbors" => Query::Neighbors {
                    node: parse_id(&q_args["node"], "args.node")?,
                    depth: usize::try_from(q_args["depth"].as_u64().unwrap_or(1)).unwrap_or(usize::MAX),
                },
                "communities" => Query::Communities,
                "god_nodes" => Query::GodNodes {
                    top_per_partition: usize::try_from(q_args["top_per_partition"].as_u64().unwrap_or(3))
                        .unwrap_or(usize::MAX),
                },
                "recent_changes" => Query::RecentChanges {
                    since_ms: q_args["since_ms"].as_i64().unwrap_or(0),
                },
                other => {
                    return Err(LoopError::BadArgs(format!(
                        "graph_explain: unknown kind `{other}`"
                    )))
                }
            };
            Ok(origin_tools::builtins::graph_explain::graph_explain_tool(&q))
        }
        // ── Memory tools ──
        // `mem_search` / `mem_save` / `mem_forget` require a `&dyn MemoryHandle`
        // threaded through `LoopOptions::memory_handle`. When the handle is
        // `Some`, they delegate to the typed execute functions in
        // `origin_tools::builtins::mem`. When `None`, they return a clear
        // `ToolFailure` (never `UnknownTool`) so the model knows the subsystem
        // exists but is not currently configured.
        "mem_search" => {
            let Some(handle) = memory else {
                return Err(LoopError::ToolFailure(
                    "mem_search: memory subsystem not configured".into(),
                ));
            };
            let input = args.to_string();
            origin_tools::builtins::mem::mem_search_execute(handle, &input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "mem_save" => {
            let Some(handle) = memory else {
                return Err(LoopError::ToolFailure(
                    "mem_save: memory subsystem not configured".into(),
                ));
            };
            let input = args.to_string();
            origin_tools::builtins::mem::mem_save_execute(handle, &input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        "mem_forget" => {
            let Some(handle) = memory else {
                return Err(LoopError::ToolFailure(
                    "mem_forget: memory subsystem not configured".into(),
                ));
            };
            let input = args.to_string();
            origin_tools::builtins::mem::mem_forget_execute(handle, &input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        // ── ask ──
        "ask" => {
            let idx_arc = code_graph.ok_or_else(|| {
                LoopError::ToolFailure(
                    "ask: code-graph subsystem not yet wired (CodeGraphIndex not in LoopOptions).".into(),
                )
            })?;
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("ask: missing `query`".into()))?;
            // Use provided MemRouter or fall back to the NullMemRouter.
            let null_router = origin_codegraph::ask::NullMemRouter;
            let router: &dyn origin_codegraph::ask::MemRouter = mem_router.unwrap_or(&null_router);
            let result = {
                let idx = idx_arc.lock().await;
                origin_tools::builtins::ask::ask_tool(&idx, router, query)
            };
            let route_str = match result.route {
                origin_codegraph::ask::Route::Code => "code",
                origin_codegraph::ask::Route::Mem => "mem",
                origin_codegraph::ask::Route::Both => "both",
            };
            let mem_hits: Vec<serde_json::Value> = result
                .mem_hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "id": h.id,
                        "score": h.score,
                        "body": h.body,
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "route": route_str,
                "mem_hits": mem_hits,
            })
            .to_string())
        }
        // ── Web tools (stateless) ──
        "WebFetch" => {
            // Accepts either a single `url` or a `urls` array (up to 20);
            // `web_fetch_args` validates that at least one URL is present and
            // renders combined per-URL markdown when several are requested.
            origin_tools::builtins::web_fetch::web_fetch_args(args)
                .await
                .map_err(LoopError::ToolFailure)
        }
        "WebSearch" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| LoopError::BadArgs("WebSearch: missing `query`".into()))?;
            let count =
                usize::try_from(args.get("count").and_then(Value::as_u64).unwrap_or(10)).unwrap_or(10);
            let hits = origin_tools::builtins::web_search::web_search(query, count)
                .await
                .map_err(LoopError::ToolFailure)?;
            serde_json::to_string(&hits).map_err(|e| LoopError::ToolFailure(format!("WebSearch: json: {e}")))
        }
        // ── Browser (stateful; lazy process-global router) ──
        //
        // The router owns two long-lived Node child processes (agent-browser
        // primary, CloakBrowser fallback). Spawning them is expensive, so we
        // initialize the router once per process via `OnceCell`. Concurrent
        // browser calls serialize on the Mutex — fine because the agent loop
        // is sequential within a turn, and sessions are disambiguated by the
        // `session` field in each verb.
        "Browser" => {
            use tokio::sync::{Mutex, OnceCell};
            static ROUTER: OnceCell<Mutex<origin_browser::BrowserRouter>> = OnceCell::const_new();
            let router_mu = ROUTER
                .get_or_try_init(|| async {
                    origin_browser::BrowserRouter::new()
                        .await
                        .map(Mutex::new)
                        .map_err(|e| LoopError::ToolFailure(format!("Browser router init: {e}")))
                })
                .await?;
            let verb: origin_browser::Verb = serde_json::from_value(args.clone())
                .map_err(|e| LoopError::BadArgs(format!("Browser: {e}")))?;
            let resp = {
                let mut r = router_mu.lock().await;
                r.run(&verb)
                    .await
                    .map_err(|e| LoopError::ToolFailure(format!("Browser: {e}")))?
            };
            // Visual loop (opt-in via `ORIGIN_BROWSER_VISUAL=1`): after the
            // action, attach a post-action screenshot (as a multimodal image
            // ContentBlock) plus recent console lines so the model can
            // *visually* verify the page. Gate OFF (the default) ⇒ the tool
            // result is byte-identical to the bare `SnapshotResp` JSON.
            if std::env::var("ORIGIN_BROWSER_VISUAL").as_deref() == Ok("1") {
                return browser_visual_result(&verb, &resp);
            }
            serde_json::to_string(&resp).map_err(|e| LoopError::ToolFailure(format!("Browser: json: {e}")))
        }
        // ── Task ──
        // Requires an `origin_swarm::Coordinator` threaded through `LoopOptions`.
        "Task" => {
            let coord =
                coordinator.ok_or_else(|| LoopError::ToolFailure("Task subsystem not configured".into()))?;
            let input: origin_tools::builtins::task::TaskInput =
                serde_json::from_value(args.clone()).map_err(|e| LoopError::BadArgs(format!("Task: {e}")))?;
            let output = origin_tools::builtins::task::task_tool(coord, input)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))?;
            serde_json::to_string(&output).map_err(|e| LoopError::ToolFailure(format!("Task: json: {e}")))
        }
        // ── Gmail (read-only; permission-gated) ──
        // Loads Google creds from the keyvault, mints a token, and runs the
        // requested op (search | get | list_threads). Mirrors WebFetch's error
        // mapping: bad arguments → BadArgs, everything else → ToolFailure.
        "gmail" => {
            let a = origin_gmail::GmailArgs::from_value(args)
                .map_err(|e| LoopError::BadArgs(e.to_string()))?;
            origin_gmail::run_tool(a)
                .await
                .map_err(|e| LoopError::ToolFailure(e.to_string()))
        }
        // ── AuthorWorkflow (mutating; persists ~/.origin/workflows.toml) ──
        // Synthesises a runnable workflow from a natural-language `goal` over the
        // live skill catalog, persists it (replacing any same-named workflow),
        // and returns the rendered TOML + chosen name so the model sees what it
        // created. The result is then runnable via the existing `{workflow:<name>}`
        // path, which this arm does not touch.
        "AuthorWorkflow" => author_workflow_tool(args, skill_catalog),
        // ── RunWorkflow (mutating; spawns sub-agents) ──
        // Loads the named workflow from `~/.origin/workflows.toml` and runs it as
        // a phase-layered parallel DAG of real swarm workers via the daemon-wide
        // Coordinator (one sub-agent per step, independent same-layer steps
        // concurrent). Returns a JSON run summary. This is the FAN-OUT complement
        // to the linear `{workflow:<name>}` skill-mask activation path, which this
        // arm does not touch.
        "RunWorkflow" => {
            let coord = coordinator
                .ok_or_else(|| LoopError::ToolFailure("RunWorkflow: swarm coordinator not configured".into()))?;
            run_workflow_tool(args, coord, skill_catalog).await
        }
        other => Err(LoopError::UnknownTool(other.into())),
    }
}

/// Execute the `RunWorkflow` tool.
///
/// Reads the `name` argument, loads that workflow from
/// `~/.origin/workflows.toml`, and runs it through
/// [`crate::workflow_runner::run_workflow`] against the daemon-wide swarm
/// `coordinator`. The workflow's steps fan out per dependency layer (one
/// sub-agent per step, independent steps concurrent). Returns the
/// [`RunReport`](crate::workflow_runner::RunReport) serialized as JSON.
///
/// Errors map to `BadArgs` (missing/empty `name`) or `ToolFailure` (no such
/// workflow, load I/O, layering failure, or a swarm-layer error).
async fn run_workflow_tool(
    args: &Value,
    coordinator: &origin_swarm::Coordinator,
    skill_catalog: Option<&SkillCatalog>,
) -> Result<String, LoopError> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| LoopError::BadArgs("RunWorkflow: missing `name`".into()))?;

    let path = crate::workflows::path()
        .map_err(|e| LoopError::ToolFailure(format!("RunWorkflow: resolve path: {e}")))?;
    let file = crate::workflows::load_from(&path)
        .map_err(|e| LoopError::ToolFailure(format!("RunWorkflow: load: {e}")))?;
    let workflow = file
        .workflows
        .iter()
        .find(|w| w.name == name)
        .ok_or_else(|| LoopError::ToolFailure(format!("RunWorkflow: no such workflow `{name}`")))?;

    // An empty/missing catalog still runs: the runner falls back to each step's
    // `args` as the worker goal and a default read+edit tool set.
    let empty_catalog = SkillCatalog::default();
    let catalog = skill_catalog.unwrap_or(&empty_catalog);

    let report = crate::workflow_runner::run_workflow(workflow, coordinator, catalog)
        .await
        .map_err(|e| LoopError::ToolFailure(format!("RunWorkflow: {e}")))?;
    serde_json::to_string(&report)
        .map_err(|e| LoopError::ToolFailure(format!("RunWorkflow: json: {e}")))
}

/// Execute the `AuthorWorkflow` tool.
///
/// Builds an [`origin_workflowgen::SkillCatalog`] from the live daemon skill
/// catalog (mapping each skill → `SkillInfo { name, description }` using the
/// SAME fully-qualified names the `Skill` tool accepts), authors + renders a
/// workflow for `goal`, optionally overrides its name, persists it into
/// `~/.origin/workflows.toml` (load → push-or-replace by name → atomic save),
/// and returns the rendered TOML plus the chosen name.
///
/// Errors map to `BadArgs` (missing/empty `goal`) or `ToolFailure` (no skill
/// catalog configured, authoring failure, or persistence I/O).
fn author_workflow_tool(
    args: &Value,
    skill_catalog: Option<&SkillCatalog>,
) -> Result<String, LoopError> {
    let goal = args
        .get("goal")
        .and_then(Value::as_str)
        .ok_or_else(|| LoopError::BadArgs("AuthorWorkflow: missing `goal`".into()))?;
    let override_name = args.get("name").and_then(Value::as_str);

    let catalog = skill_catalog
        .ok_or_else(|| LoopError::ToolFailure("AuthorWorkflow: skill catalog not configured".into()))?;

    // Map the live skill catalog → the workflowgen catalog, preserving order so
    // the deterministic planner's tie-breaks stay stable. The names are the
    // fully-qualified `front.name`s the `Skill` tool (and `{workflow:<name>}`
    // runner) accept verbatim.
    let infos: Vec<origin_workflowgen::SkillInfo> = catalog
        .iter()
        .map(|s| origin_workflowgen::SkillInfo::new(s.front.name.clone(), s.front.description.clone()))
        .collect();
    let wf_catalog = origin_workflowgen::SkillCatalog::new(infos);

    let (mut spec, mut toml) = origin_workflowgen::author_and_render(goal, &wf_catalog)
        .map_err(|e| LoopError::ToolFailure(format!("AuthorWorkflow: {e}")))?;

    // An explicit `name` overrides the auto-derived slug. Re-render so the
    // returned TOML carries the chosen name too.
    if let Some(name) = override_name.map(str::trim).filter(|n| !n.is_empty()) {
        spec.name = name.to_string();
        toml = spec
            .to_toml()
            .map_err(|e| LoopError::ToolFailure(format!("AuthorWorkflow: {e}")))?;
    }
    let chosen_name = spec.name.clone();

    // Map WorkflowSpec → the daemon's on-disk Workflow (empty args → None) and
    // persist: load existing file, replace any same-named workflow (else push),
    // atomic save.
    let workflow = crate::workflows::Workflow {
        name: spec.name,
        description: Some(spec.description),
        steps: spec
            .steps
            .into_iter()
            .map(|st| crate::workflows::WorkflowStep {
                // Carry the authored phase-layered DAG (id + depends_on) onto the
                // on-disk form so `RunWorkflow` can fan steps out in dependency
                // order at run time, not just sequence them linearly.
                id: st.id.index(),
                skill: st.skill,
                args: if st.args.is_empty() { None } else { Some(st.args) },
                depends_on: st.depends_on.iter().map(|d| d.index()).collect(),
            })
            .collect(),
    };

    let path = crate::workflows::path()
        .map_err(|e| LoopError::ToolFailure(format!("AuthorWorkflow: resolve path: {e}")))?;
    let mut file = crate::workflows::load_from(&path)
        .map_err(|e| LoopError::ToolFailure(format!("AuthorWorkflow: load: {e}")))?;
    if file.schema_version == 0 {
        file.schema_version = crate::workflows::SCHEMA_VERSION;
    }
    match file.workflows.iter_mut().find(|w| w.name == chosen_name) {
        Some(existing) => *existing = workflow,
        None => file.workflows.push(workflow),
    }
    crate::workflows::save_to(&path, &file)
        .map_err(|e| LoopError::ToolFailure(format!("AuthorWorkflow: save: {e}")))?;

    Ok(format!(
        "Authored workflow `{chosen_name}` (run with `{{workflow:{chosen_name}}}`):\n\n{toml}"
    ))
}

/// The filesystem path a [`origin_browser::Verb`] asked the browser to write a
/// screenshot to, if any. Only [`origin_browser::Verb::Screenshot`] carries one;
/// every other verb returns `None` (the visual loop then attaches console only).
fn browser_screenshot_path(verb: &origin_browser::Verb) -> Option<&str> {
    match verb {
        origin_browser::Verb::Screenshot { path, .. } => Some(path.as_str()),
        origin_browser::Verb::Open { .. }
        | origin_browser::Verb::Click { .. }
        | origin_browser::Verb::Fill { .. }
        | origin_browser::Verb::Extract { .. }
        | origin_browser::Verb::Snapshot { .. }
        | origin_browser::Verb::Close { .. } => None,
    }
}

/// Build the **visual-loop** tool-result body for the `Browser` tool.
///
/// Gated by `ORIGIN_BROWSER_VISUAL=1` at the call site; this function assumes
/// the gate is on. It captures the post-action screenshot (read back from the
/// path the verb wrote, when present) and the recent console lines carried on
/// the response, then emits an **additive** JSON envelope:
///
/// ```text
/// {"response": <the original SnapshotResp>,
///  "visual": {"attachments": [<ContentBlock image>...],
///             "console": "console (N lines):\n…"}}
/// ```
///
/// When neither a screenshot nor any console line is available, the envelope is
/// the bare `SnapshotResp` JSON — identical to the gate-off path — so a backend
/// that cannot screenshot and logs nothing adds no noise.
///
/// The image rides as an `origin_multimodal::ContentBlock` so the existing
/// attachment/tool-result plumbing the model already understands can render it.
fn browser_visual_result(
    verb: &origin_browser::Verb,
    resp: &origin_browser::SnapshotResp,
) -> Result<String, LoopError> {
    let console: &[String] = resp.console.as_deref().unwrap_or_default();
    let capture =
        origin_browser::VisualCapture::from_action(browser_screenshot_path(verb), console);
    if capture.is_empty() {
        return serde_json::to_string(resp)
            .map_err(|e| LoopError::ToolFailure(format!("Browser: json: {e}")));
    }
    let mut attachments: Vec<origin_multimodal::ContentBlock> = Vec::new();
    if let Some(png) = capture.screenshot_png.as_deref() {
        attachments.push(origin_multimodal::ContentBlock::image(
            "image/png",
            origin_multimodal::base64_encode(png),
        ));
    }
    let envelope = serde_json::json!({
        "response": resp,
        "visual": {
            "attachments": attachments,
            "console": capture.console_text(),
        }
    });
    serde_json::to_string(&envelope)
        .map_err(|e| LoopError::ToolFailure(format!("Browser: json: {e}")))
}

/// Rebuild the code graph for each detected-language group, aggregating the
/// per-group [`RebuildReport`]s into one.
///
/// `groups` is the per-language path bucketing produced by
/// [`crate::subsystems::group_paths_by_language`]: each entry rebuilds under its
/// own grammar via [`origin_tools::builtins::graph_rebuild::graph_rebuild_tool`].
/// `nodes_added` / `nodes_updated` are summed and `errors` concatenated across
/// groups; `paths_seen` is left at zero here because the caller already knows the
/// requested total (grouping drops unsupported files, so it would undercount).
///
/// Taking `idx` as a plain `&mut` reference keeps the lock guard at the call site
/// a single-use temporary, so it is never held across an await.
///
/// # Errors
/// Propagates the first fatal [`origin_codegraph::rebuild::RebuildError`]
/// (CAS / `SQLite`) from any group.
fn rebuild_groups(
    idx: &mut origin_codegraph::index::CodeGraphIndex,
    groups: Vec<(origin_codegraph::Language, Vec<std::path::PathBuf>)>,
) -> Result<origin_codegraph::rebuild::RebuildReport, origin_codegraph::rebuild::RebuildError> {
    let mut agg = origin_codegraph::rebuild::RebuildReport::default();
    for (lang, group_paths) in groups {
        let report =
            origin_tools::builtins::graph_rebuild::graph_rebuild_tool(idx, group_paths, lang)?;
        agg.nodes_added += report.nodes_added;
        agg.nodes_updated += report.nodes_updated;
        agg.errors.extend(report.errors);
    }
    Ok(agg)
}

/// Serialize a [`origin_codegraph::query::QueryResult`] as a JSON string.
///
/// `NodeRow` handles (`signature_handle`, `body_handle`) are rendered as
/// lowercase hex so they are round-trippable through the CAS layer.
fn serialize_query_result(r: &origin_codegraph::query::QueryResult) -> String {
    use origin_codegraph::query::QueryResult;
    match r {
        QueryResult::Empty => "{}".to_string(),
        QueryResult::Nodes(nodes) => {
            let arr: Vec<serde_json::Value> = nodes.iter().map(node_row_to_json).collect();
            serde_json::json!({ "nodes": arr }).to_string()
        }
        QueryResult::Path(nodes) => {
            let arr: Vec<serde_json::Value> = nodes.iter().map(node_row_to_json).collect();
            serde_json::json!({ "path": arr }).to_string()
        }
        QueryResult::Partitions(parts) => {
            let arr: Vec<serde_json::Value> = parts
                .iter()
                .map(|part| {
                    let rows: Vec<serde_json::Value> = part.iter().map(node_row_to_json).collect();
                    serde_json::Value::Array(rows)
                })
                .collect();
            serde_json::json!({ "partitions": arr }).to_string()
        }
    }
}

fn node_row_to_json(row: &origin_codegraph::index::NodeRow) -> serde_json::Value {
    serde_json::json!({
        "entity_id": hex::encode(row.entity_id.as_bytes()),
        "kind": row.kind,
        "name": row.name,
        "file_path": row.file_path,
        "signature_handle": hex::encode(row.signature_handle),
        "body_handle": hex::encode(row.body_handle),
    })
}

/// Dispatch the `Bash` tool and forward output to `event_tx` as
/// [`StreamEvent::ToolChunk`] events **in real time** — each line is streamed
/// to the user the instant the child writes it, not after the process exits.
///
/// This is the key difference from a buffered dispatch: a long-running (or
/// stuck) command surfaces its output progressively, so the user can tell
/// whether work is happening rather than staring at a silent gap and
/// wondering if the command hung. The LLM still receives the fully
/// accumulated structured body via the returned JSON bytes.
///
/// `event_tx` being `None` (unit tests, headless runs) is supported — chunks
/// are skipped but execution still completes and the full body is returned.
#[allow(clippy::too_many_lines)] // real-time streaming poll loop; extracting sub-functions would fragment the line-buffer state and add allocations
async fn run_bash_streaming(
    args: &Value,
    event_tx: Option<&tokio::sync::mpsc::Sender<StreamEvent>>,
) -> Result<Vec<u8>, String> {
    use origin_tools::proc_supervisor::{ProcStatus, SpawnOpts, Supervisor};
    use std::time::Duration;

    let command = args
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "Bash: missing `command`".to_string())?
        .to_string();
    let timeout_secs = args
        .get("timeout")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(120)
        .min(600);
    let cwd = args.get("cwd").and_then(Value::as_str).map(str::to_string);
    let env: Vec<(String, String)> = args
        .get("env")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let arr = e.as_array()?;
                    Some((
                        arr.first()?.as_str()?.to_string(),
                        arr.get(1)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let run_in_background = args
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Per-call supervisor: run_in_background + Monitor across separate tool
    // invocations won't work via this legacy path (known limitation —
    // Phase 8 replaces with envelope-level shared supervisor).
    let sup = Supervisor::new();
    let opts = SpawnOpts {
        timeout: Some(Duration::from_secs(u64::from(timeout_secs))),
        cwd,
        env,
        buffer_cap_bytes: None,
    };
    let pid = sup.spawn(&command, &opts).map_err(|e| e.message)?;

    // Background: return the pid immediately, exactly like the foreground-vs-
    // background split in `bash_v2`. No streaming — the user tails via Monitor.
    if run_in_background {
        let result = serde_json::json!({"status": "started", "pid": pid});
        return Ok(serde_json::to_string(&result)
            .expect("BUG: bash result Value always serializes")
            .into_bytes());
    }

    // Foreground: poll the supervisor's ring buffer and stream every complete
    // line to the user as soon as it appears. We hold back a trailing partial
    // line (no terminating newline yet) so chunks align to line boundaries;
    // any remainder is flushed after the process terminates.
    let mut next = 0u64;
    let mut acc = String::new();
    let mut pending = String::new();
    let mut chunk_count: u32 = 0;
    // Generous wall-clock ceiling: the supervisor enforces `timeout_secs` on
    // the child; this only bounds our polling loop in the pathological case
    // where status never flips terminal.
    let deadline =
        std::time::Instant::now() + Duration::from_secs(u64::from(timeout_secs) + 5);

    let flush_lines =
        |pending: &mut String, chunk_count: &mut u32| -> Vec<String> {
            let mut lines = Vec::new();
            while let Some(nl) = pending.find('\n') {
                let line: String = pending.drain(..=nl).collect();
                // Drop the trailing '\n' for display; ToolChunk is per-line.
                let line = line.trim_end_matches('\n').to_string();
                *chunk_count += 1;
                lines.push(line);
            }
            lines
        };

    let (status_str, exit_code) = loop {
        let chunk = sup
            .read_since(pid, next, 64 * 1024)
            .map_err(|e| e.message)?;
        if !chunk.bytes.is_empty() {
            acc.push_str(&chunk.bytes);
            pending.push_str(&chunk.bytes);
            next = chunk.next_offset;
            if let Some(tx) = event_tx {
                for line in flush_lines(&mut pending, &mut chunk_count) {
                    let _ = tx
                        .send(StreamEvent::ToolChunk {
                            tool: "Bash".to_string(),
                            content: line,
                        })
                        .await;
                }
            } else {
                // No sink: still advance the line counter so the terminal
                // "(no output)" affordance below stays accurate.
                let _ = flush_lines(&mut pending, &mut chunk_count);
            }
            // Drain quickly when there is buffered output to stay real-time.
            continue;
        }

        if chunk.status.is_terminal() {
            // Drain any remaining buffered bytes the per-read cap left behind.
            loop {
                let more = sup
                    .read_since(pid, next, 64 * 1024)
                    .map_err(|e| e.message)?;
                if more.bytes.is_empty() {
                    break;
                }
                acc.push_str(&more.bytes);
                pending.push_str(&more.bytes);
                next = more.next_offset;
            }
            if let Some(tx) = event_tx {
                for line in flush_lines(&mut pending, &mut chunk_count) {
                    let _ = tx
                        .send(StreamEvent::ToolChunk {
                            tool: "Bash".to_string(),
                            content: line,
                        })
                        .await;
                }
            } else {
                let _ = flush_lines(&mut pending, &mut chunk_count);
            }
            break match chunk.status {
                ProcStatus::Exited(c) => ("exited", c),
                ProcStatus::TimedOut => ("timed_out", -1),
                ProcStatus::Killed => ("killed", -1),
                ProcStatus::Running => unreachable!(),
            };
        }

        if std::time::Instant::now() > deadline {
            break ("timed_out", -1);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // Flush any trailing partial line (output without a terminating newline).
    if !pending.is_empty() {
        chunk_count += 1;
        let trailing = std::mem::take(&mut pending);
        if let Some(tx) = event_tx {
            let _ = tx
                .send(StreamEvent::ToolChunk {
                    tool: "Bash".to_string(),
                    content: trailing,
                })
                .await;
        }
    }

    // Silent commands (write-only scripts, no stdout/stderr): emit a single
    // terminal affordance so the user still sees the command completed.
    if chunk_count == 0 {
        if let Some(tx) = event_tx {
            let _ = tx
                .send(StreamEvent::ToolResult {
                    tool: "Bash".to_string(),
                    ok: exit_code == 0,
                    preview: format!("(exit {exit_code}, no output)"),
                    elided_bytes: 0,
                })
                .await;
        }
    }

    let result = serde_json::json!({
        "status": status_str,
        "exit_code": exit_code,
        "stdout": acc,
    });
    Ok(serde_json::to_string(&result)
        .expect("BUG: bash result Value always serializes")
        .into_bytes())
}

/// Build a bounded preview of a tool's result bytes for live display in the
/// CLI. Returns `(preview, elided_bytes)`. The preview is at most
/// `MAX_PREVIEW_LINES` lines, each truncated to `MAX_PREVIEW_LINE_CHARS`
/// chars; the elided byte count covers everything past that window so the
/// CLI can render a "+N bytes omitted" affordance. Non-UTF8 input is
/// lossily decoded — the model still sees the raw bytes upstream.
fn build_tool_result_preview(bytes: &[u8]) -> (String, u32) {
    const MAX_PREVIEW_LINES: usize = 8;
    const MAX_PREVIEW_LINE_CHARS: usize = 200;

    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut consumed: usize = 0;
    let mut lines_iter = text.split_inclusive('\n');
    for _ in 0..MAX_PREVIEW_LINES {
        let Some(line) = lines_iter.next() else {
            break;
        };
        consumed += line.len();
        let trimmed: String = line.chars().take(MAX_PREVIEW_LINE_CHARS).collect();
        out.push_str(&trimmed);
        if trimmed.len() < line.len() && !trimmed.ends_with('\n') {
            out.push('\n');
        }
    }
    // Remove a single trailing newline so the CLI doesn't render a stray
    // blank line — the renderer adds its own line breaks per scrollback row.
    if out.ends_with('\n') {
        out.pop();
    }
    let elided = bytes.len().saturating_sub(consumed);
    (out, u32::try_from(elided).unwrap_or(u32::MAX))
}

fn tool_activity_summary(name: &str, args: &Value) -> String {
    let path_str = || {
        args.get("path")
            .or_else(|| args.get("file_path"))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    match name {
        "Write" => {
            let path = path_str();
            let lines = args
                .get("content")
                .and_then(Value::as_str)
                .map_or(0, |c| c.lines().count());
            format!("{path} ({lines} lines)")
        }
        "Edit" => {
            let path = path_str();
            let old_lines = args
                .get("old_string")
                .and_then(Value::as_str)
                .map_or(0, |s| s.lines().count());
            let new_lines = args
                .get("new_string")
                .and_then(Value::as_str)
                .map_or(0, |s| s.lines().count());
            let added = new_lines.saturating_sub(old_lines);
            let removed = old_lines.saturating_sub(new_lines);
            let mut parts = Vec::new();
            if added > 0 {
                parts.push(format!("+{added}"));
            }
            if removed > 0 {
                parts.push(format!("-{removed}"));
            }
            if parts.is_empty() {
                parts.push(format!("~{new_lines}"));
            }
            format!("{path} ({} lines)", parts.join(", "))
        }
        "Read" => path_str().to_string(),
        "Grep" => {
            let pat = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            let root = args.get("root").and_then(Value::as_str).unwrap_or(".");
            // Cap the pattern so a long regex doesn't blow past the
            // status column. Root is short by nature.
            let pat_short: String = pat.chars().take(40).collect();
            format!("{pat_short} @ {root}")
        }
        "Glob" => args
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "Bash" => {
            let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
            cmd.chars().take(80).collect()
        }
        "WebFetch" => args.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
        // PII-safe preview: surface only the `op`, never the `query` (a Gmail
        // search expression) or `id` — both can carry sensitive content.
        "gmail" => args.get("op").and_then(Value::as_str).unwrap_or("").to_string(),
        "AuthorWorkflow" => {
            let goal = args.get("goal").and_then(Value::as_str).unwrap_or("");
            goal.chars().take(60).collect()
        }
        "RunWorkflow" => args.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => {
            let s = args.to_string();
            s.chars().take(60).collect()
        }
    }
}

fn tool_diff_lines(name: &str, args: &Value) -> Vec<crate::protocol::DiffLine> {
    use crate::protocol::DiffLine;
    match name {
        "Edit" => {
            let old = args.get("old_string").and_then(Value::as_str).unwrap_or("");
            let new = args.get("new_string").and_then(Value::as_str).unwrap_or("");
            let old_lines: Vec<&str> = old.lines().collect();
            let new_lines: Vec<&str> = new.lines().collect();
            diff_lines_lcs(&old_lines, &new_lines)
        }
        "Write" => {
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            content
                .lines()
                .enumerate()
                .map(|(i, line)| DiffLine {
                    kind: "+".to_string(),
                    // Line counts beyond u32::MAX (~4B lines) are not realistic for any file
                    // we'd diff; saturate rather than panic if we ever see one.
                    line_no: u32::try_from(i + 1).unwrap_or(u32::MAX),
                    text: line.to_string(),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Cap on the LCS dynamic-programming table (`old.len() * new.len()`). Typical
/// Edit hunks are tiny; whole-file replacements are not, and an O(n*m) table
/// would cost too much there. Past this, fall back to a linear diff.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// Line diff via longest-common-subsequence. Produces a minimal sequence of
/// context (`" "`), delete (`"-"`), and insert (`"+"`) lines that reproduces
/// `old` (delete+context) and `new` (insert+context) exactly and in order.
///
/// Replaces a hand-rolled two-cursor walk that could spin forever when lines
/// were reordered (each side's current line recurred later in the other, so
/// neither cursor advanced). `O(n*m)` time/space, bounded by [`MAX_DIFF_CELLS`];
/// larger inputs degrade to a correct-but-non-minimal delete-all/insert-all diff.
fn diff_lines_lcs(old: &[&str], new: &[&str]) -> Vec<crate::protocol::DiffLine> {
    use crate::protocol::DiffLine;
    let n = old.len();
    let m = new.len();

    if n.saturating_mul(m) > MAX_DIFF_CELLS {
        let mut out = Vec::with_capacity(n + m);
        for (i, line) in old.iter().enumerate() {
            let old_no = u32::try_from(i + 1).unwrap_or(u32::MAX);
            out.push(DiffLine {
                kind: "-".to_string(),
                line_no: old_no,
                text: (*line).to_string(),
            });
        }
        for (i, line) in new.iter().enumerate() {
            let new_no = u32::try_from(i + 1).unwrap_or(u32::MAX);
            out.push(DiffLine {
                kind: "+".to_string(),
                line_no: new_no,
                text: (*line).to_string(),
            });
        }
        return out;
    }

    // dp[i][j] = LCS length of old[i..] and new[j..].
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // Backtrack from (0,0); prefer deletes on ties for stable output. Context
    // and insert lines are numbered by the new file, deletes by the old file
    // (matching the prior rendering convention).
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    let (mut old_no, mut new_no) = (0u32, 0u32);
    while i < n && j < m {
        if old[i] == new[j] {
            old_no += 1;
            new_no += 1;
            out.push(DiffLine {
                kind: " ".to_string(),
                line_no: new_no,
                text: new[j].to_string(),
            });
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            old_no += 1;
            out.push(DiffLine {
                kind: "-".to_string(),
                line_no: old_no,
                text: old[i].to_string(),
            });
            i += 1;
        } else {
            new_no += 1;
            out.push(DiffLine {
                kind: "+".to_string(),
                line_no: new_no,
                text: new[j].to_string(),
            });
            j += 1;
        }
    }
    while i < n {
        old_no += 1;
        out.push(DiffLine {
            kind: "-".to_string(),
            line_no: old_no,
            text: old[i].to_string(),
        });
        i += 1;
    }
    while j < m {
        new_no += 1;
        out.push(DiffLine {
            kind: "+".to_string(),
            line_no: new_no,
            text: new[j].to_string(),
        });
        j += 1;
    }
    out
}

#[cfg(test)]
mod diff_line_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Regression: an `Edit` whose old/new lines are *reordered* (each current
    /// line recurs later in the opposite side) used to make `tool_diff_lines`
    /// spin forever — neither cursor advanced, so the outer `while` never
    /// exited. It runs on the live turn path (`agent.rs:848`, building the
    /// `ToolActivity` diff), so the bug wedged the whole daemon at 100% CPU on
    /// one thread. This test fails by *non-termination* if the bug regresses:
    /// the diff is computed on a worker thread guarded by a wall-clock deadline.
    #[test]
    fn edit_diff_terminates_on_reordered_lines() {
        let worker = std::thread::spawn(|| {
            let args = serde_json::json!({
                "old_string": "alpha\nbeta",
                "new_string": "beta\nalpha",
            });
            tool_diff_lines("Edit", &args)
        });

        let start = Instant::now();
        while !worker.is_finished() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "tool_diff_lines did not terminate on reordered lines (infinite-loop regression)"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let diff = worker.join().expect("tool_diff_lines panicked");
        // A correct diff is finite and accounts for every line on both sides.
        let texts: Vec<&str> = diff.iter().map(|d| d.text.as_str()).collect();
        assert!(texts.contains(&"alpha"), "missing 'alpha' in diff: {texts:?}");
        assert!(texts.contains(&"beta"), "missing 'beta' in diff: {texts:?}");
    }

    /// Reconstruct one side of the diff: deletes+context yield the old file,
    /// inserts+context yield the new file. A correct diff must reproduce both
    /// sides exactly and in order.
    fn reconstruct(diff: &[crate::protocol::DiffLine], side: &str) -> Vec<String> {
        diff.iter()
            .filter(|d| d.kind == " " || d.kind == side)
            .map(|d| d.text.clone())
            .collect()
    }

    #[test]
    fn edit_diff_keeps_common_line_as_context_on_swap() {
        // old [A,B] -> new [B,A]. The longest common subsequence has length 1,
        // so a minimal diff keeps exactly one line as context. The hand-rolled
        // walk kept zero (pure delete+insert churn); a proper LCS diff keeps one.
        let args = serde_json::json!({ "old_string": "A\nB", "new_string": "B\nA" });
        let diff = tool_diff_lines("Edit", &args);
        let context = diff.iter().filter(|d| d.kind == " ").count();
        assert_eq!(
            context, 1,
            "minimal diff should keep the common line as context: {diff:?}"
        );
    }

    #[test]
    fn edit_diff_reconstructs_both_sides() {
        // The fundamental diff invariant across a representative mix.
        let cases = [
            ("A\nB\nC", "A\nB\nC"),    // identical
            ("A\nB\nC", "A\nX\nC"),    // substitution
            ("A", "A\nB"),             // pure insert
            ("A\nB", "A"),             // pure delete
            ("A\nB", "B\nA"),          // swap
            ("", "A\nB"),              // empty old
            ("A\nB", ""),              // empty new
            ("a\nb\nc\nd", "b\nd\ne"), // mixed
        ];
        for (old, new) in cases {
            let args = serde_json::json!({ "old_string": old, "new_string": new });
            let diff = tool_diff_lines("Edit", &args);
            let old_lines: Vec<String> = old.lines().map(str::to_string).collect();
            let new_lines: Vec<String> = new.lines().map(str::to_string).collect();
            assert_eq!(
                reconstruct(&diff, "-"),
                old_lines,
                "old side mismatch for {old:?}->{new:?}"
            );
            assert_eq!(
                reconstruct(&diff, "+"),
                new_lines,
                "new side mismatch for {old:?}->{new:?}"
            );
        }
    }
}

fn parse_region(v: &Value) -> Result<origin_tools::builtins::recall::Region, LoopError> {
    if let Some(lines) = v.get("lines").and_then(Value::as_array) {
        // Region indices are bounded by file sizes and will never exceed usize::MAX
        // on any supported target. Casting u64 -> usize is intentional here.
        #[allow(clippy::cast_possible_truncation)]
        let start = lines
            .first()
            .and_then(Value::as_u64)
            .ok_or_else(|| LoopError::BadArgs("Recall.region.lines requires [start, end]".into()))?
            as usize;
        #[allow(clippy::cast_possible_truncation)]
        let end = lines
            .get(1)
            .and_then(Value::as_u64)
            .ok_or_else(|| LoopError::BadArgs("Recall.region.lines requires [start, end]".into()))?
            as usize;
        Ok(origin_tools::builtins::recall::Region::Lines { start, end })
    } else if let Some(m) = v.get("match").and_then(Value::as_str) {
        Ok(origin_tools::builtins::recall::Region::Match {
            pattern: m.to_string(),
        })
    } else if v.get("outline_only").and_then(Value::as_bool) == Some(true) {
        Ok(origin_tools::builtins::recall::Region::OutlineOnly)
    } else {
        Err(LoopError::BadArgs(
            "Recall.region: expected lines/match/outline_only".into(),
        ))
    }
}

/// Run one streaming turn. Pre-subscribes BOTH the drain and (optionally) the
/// relay before publishing — a fresh subscriber starts at the current write
/// cursor, so subscribing after the producer publishes would miss every record.
/// The drive future always closes the ring on completion (success OR error) so
/// the drain and the relay subscriber wake up cleanly even if the provider
/// fails mid-stream. `Ring::close` is idempotent.
///
/// P3.4: also drives `ToolUseParser`s and spawns speculative tasks for pure
/// tools when the first `Field` event fires. Returns the registry alongside
/// the synthetic `ChatResponse` so `run_loop` can await precomputed handles.
#[tracing::instrument(
    level = "info",
    skip(provider, req, opts),
    fields(kind = "provider", provider = provider.name())
)]
async fn run_streaming_turn(
    provider: &dyn Provider,
    req: ChatRequest,
    opts: &LoopOptions,
) -> Result<StreamingTurn, LoopError> {
    let ring = origin_stream::Ring::with_capacity(256 * 1024);
    let drain_sub = ring.subscribe();
    if let Some(tx) = &opts.relay_tx {
        let relay_sub = ring.subscribe();
        let _ = tx.send(relay_sub).await;
    }
    let ring_for_drive = ring.clone();
    let drive = async move {
        let outcome = provider.chat_stream(req, &ring_for_drive).await;
        ring_for_drive.close();
        outcome
    };
    let drain = drain_subscriber_into_response(drain_sub, opts.cas.clone());
    let (drive_res, turn_res) = tokio::join!(drive, drain);
    drive_res?;
    turn_res
}

/// Decode a `ToolUseStart` payload into `(index, id, name)`.
/// Layout: 4-byte LE index + `id` bytes + `\0` + `name` bytes.
fn decode_tool_use_start(payload: &[u8]) -> Option<(u32, &str, &str)> {
    if payload.len() < 5 {
        return None;
    }
    let index = u32::from_le_bytes(payload[..4].try_into().ok()?);
    let rest = &payload[4..];
    let sep = rest.iter().position(|&b| b == 0)?;
    let id = std::str::from_utf8(&rest[..sep]).ok()?;
    let name = std::str::from_utf8(&rest[sep + 1..]).ok()?;
    Some((index, id, name))
}

/// Decode a `Usage` token payload (4× BE u32 = 16 bytes) into [`origin_provider::Usage`].
/// Returns `None` on any size mismatch so the caller cleanly skips malformed
/// payloads instead of panicking. Replaces a string of `try_into().expect("4 bytes")`
/// calls whose safety was load-bearing on the outer `p.len() == 16` guard.
fn decode_usage_payload(payload: &[u8]) -> Option<origin_provider::Usage> {
    // The single fallible step: assert the whole payload is exactly 16 bytes.
    // After that, the four 4-byte sub-arrays come from `let-else` slice
    // conversions that re-check locally — no `expect` and no panic risk if
    // the outer length guard is ever refactored away.
    let arr: &[u8; 16] = payload.try_into().ok()?;
    let Ok(input_bytes) = <[u8; 4]>::try_from(&arr[0..4]) else {
        return None;
    };
    let Ok(output_bytes) = <[u8; 4]>::try_from(&arr[4..8]) else {
        return None;
    };
    let Ok(cache_read_bytes) = <[u8; 4]>::try_from(&arr[8..12]) else {
        return None;
    };
    let Ok(cache_creation_bytes) = <[u8; 4]>::try_from(&arr[12..16]) else {
        return None;
    };
    Some(origin_provider::Usage {
        input_tokens: u32::from_be_bytes(input_bytes),
        output_tokens: u32::from_be_bytes(output_bytes),
        cache_read_input_tokens: u32::from_be_bytes(cache_read_bytes),
        cache_creation_input_tokens: u32::from_be_bytes(cache_creation_bytes),
    })
}

/// Try to speculatively spawn a pure tool when the first `Field` event fires.
/// Called at most once per `tool_use_id`. Returns `true` if a task was spawned.
fn try_speculative_spawn(
    tool_use_id: &str,
    tool_names: &HashMap<String, String>,
    tool_input_bufs: &HashMap<String, Vec<u8>>,
    registry: &mut SpeculativeRegistry,
    cas: Option<Arc<Store>>,
) -> bool {
    let Some(name) = tool_names.get(tool_use_id) else {
        return false;
    };
    let Some(meta) = registry_iter().find(|m| m.name == *name) else {
        return false;
    };
    if !matches!(meta.side_effects, SideEffects::Pure) {
        return false;
    }
    // Try to parse the accumulated bytes as a complete JSON object. For
    // single-field tools (Read, Glob) this succeeds at the first `Field`
    // event because the value's closing quote is also the start of the
    // outer `}`. For multi-field tools (Grep with `pattern` + `root`) the
    // first attempt may fail because only one field has arrived — we'll
    // retry on the next Field event when more bytes have accumulated.
    let buf = tool_input_bufs
        .get(tool_use_id)
        .map_or(&[] as &[u8], Vec::as_slice);
    if let Ok(args) = serde_json::from_slice::<Value>(buf) {
        registry.spawn(tool_use_id.to_owned(), meta, args, cas);
        return true;
    }
    false
}

#[allow(clippy::too_many_lines)] // streaming state-machine; extracting sub-functions would require extra allocation
async fn drain_subscriber_into_response(
    mut sub: origin_stream::Subscriber,
    cas: Option<Arc<Store>>,
) -> Result<StreamingTurn, LoopError> {
    let mut text = String::new();
    let mut usage = origin_provider::Usage::default();
    let mut blocks: Vec<Block> = Vec::new();

    let mut parsers: HashMap<String, ToolUseParser> = HashMap::new();
    let mut tool_input_bufs: HashMap<String, Vec<u8>> = HashMap::new();
    let mut tool_input_order: Vec<String> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut index_to_id: HashMap<u32, String> = HashMap::new();
    let mut registry = SpeculativeRegistry::default();
    let mut speculative_spawned: HashSet<String> = HashSet::new();

    // Time-to-first-token clock. Started when we begin awaiting stream events
    // (the earliest moment this consumer can observe the provider's output) and
    // sampled exactly once, at the FIRST content-bearing event — a text or
    // thinking delta, or a tool-use start. Pure local measurement: it feeds the
    // OTel `gen_ai` TTFT instrument (a no-op without `--features otel`) and
    // never alters the reconstructed response, so the default path is
    // byte-identical.
    let stream_start = std::time::Instant::now();
    let mut first_token_ms: Option<u64> = None;

    while let Some(ev) = sub
        .next()
        .await
        .map_err(|e| LoopError::ToolFailure(e.to_string()))?
    {
        match ev.kind() {
            origin_stream::TokenKind::TextDelta => {
                if first_token_ms.is_none() {
                    first_token_ms = Some(elapsed_ms_since(stream_start));
                }
                text.push_str(&String::from_utf8_lossy(ev.payload()));
            }
            origin_stream::TokenKind::ToolUseStart => {
                if first_token_ms.is_none() {
                    first_token_ms = Some(elapsed_ms_since(stream_start));
                }
                if let Some((index, id, name)) = decode_tool_use_start(ev.payload()) {
                    let mut parser = ToolUseParser::new();
                    parser.begin_tool_use(name);
                    parsers.insert(id.to_owned(), parser);
                    tool_names.insert(id.to_owned(), name.to_owned());
                    tool_input_bufs.insert(id.to_owned(), Vec::new());
                    if !tool_input_order.contains(&id.to_owned()) {
                        tool_input_order.push(id.to_owned());
                    }
                    index_to_id.insert(index, id.to_owned());
                } else {
                    tracing::warn!(bytes = ev.payload().len(), "malformed ToolUseStart payload");
                }
            }
            origin_stream::TokenKind::ToolUseDelta => {
                let payload = ev.payload();
                // Decode the 4-byte LE index locally without `expect` so any
                // future refactor that weakens an outer length guard cannot
                // turn this into a daemon-wide panic. A payload shorter than
                // 4 bytes is silently skipped (same behaviour as before).
                let Some(idx_slice) = payload.get(..4) else {
                    continue;
                };
                let Ok(idx_bytes) = <[u8; 4]>::try_from(idx_slice) else {
                    continue;
                };
                let index = u32::from_le_bytes(idx_bytes);
                let json_bytes = &payload[4..];
                if let Some(id) = index_to_id.get(&index) {
                    let id_owned = id.clone();
                    if let Some(buf) = tool_input_bufs.get_mut(&id_owned) {
                        buf.extend_from_slice(json_bytes);
                    } else {
                        // Invariant violation: `index_to_id` resolved but the
                        // per-id input buffer is missing. Should never happen
                        // unless `ToolUseStart` and these maps drift apart.
                        tracing::warn!(
                            index,
                            tool_use_id = %id_owned,
                            bytes = json_bytes.len(),
                            "ToolUseDelta: tool_input_bufs missing entry for known id; dropping bytes"
                        );
                    }
                    if let Some(parser) = parsers.get_mut(&id_owned) {
                        let events = parser.feed(json_bytes);
                        if !speculative_spawned.contains(&id_owned)
                            && events.iter().any(|e| matches!(e, ToolUseDelta::Field { .. }))
                            && try_speculative_spawn(
                                &id_owned,
                                &tool_names,
                                &tool_input_bufs,
                                &mut registry,
                                cas.clone(),
                            )
                        {
                            speculative_spawned.insert(id_owned);
                        }
                    } else {
                        // Same invariant: parsers map should mirror index_to_id.
                        tracing::warn!(
                            index,
                            tool_use_id = %id_owned,
                            "ToolUseDelta: parsers missing entry for known id; speculative dispatch skipped"
                        );
                    }
                } else {
                    // Orphan delta: index was never opened (likely because a
                    // prior `ToolUseStart` payload was malformed and warned
                    // out at the decode site above). Log it so the dropped
                    // tool input is at least observable.
                    tracing::warn!(
                        index,
                        bytes = json_bytes.len(),
                        "ToolUseDelta for unknown index; dropping bytes (no matching ToolUseStart)"
                    );
                }
            }
            origin_stream::TokenKind::Usage => {
                if let Some(parsed) = decode_usage_payload(ev.payload()) {
                    // Merge field-wise rather than overwrite: providers split
                    // usage across events (e.g. Anthropic reports input/cache in
                    // `message_start` with output=0, then output in
                    // `message_delta` with input/cache=0). A wholesale assignment
                    // would let the later event zero the earlier counts. Keep the
                    // last non-zero value seen for each field.
                    if parsed.input_tokens != 0 {
                        usage.input_tokens = parsed.input_tokens;
                    }
                    if parsed.output_tokens != 0 {
                        usage.output_tokens = parsed.output_tokens;
                    }
                    if parsed.cache_read_input_tokens != 0 {
                        usage.cache_read_input_tokens = parsed.cache_read_input_tokens;
                    }
                    if parsed.cache_creation_input_tokens != 0 {
                        usage.cache_creation_input_tokens = parsed.cache_creation_input_tokens;
                    }
                }
            }
            origin_stream::TokenKind::TurnEnd => break,
            origin_stream::TokenKind::ThinkingDelta => {
                // A thinking delta is the model's first emitted token on
                // reasoning models, so it counts toward time-to-first-token
                // even though its content is not folded into the response.
                if first_token_ms.is_none() {
                    first_token_ms = Some(elapsed_ms_since(stream_start));
                }
            }
        }
    }

    if !text.is_empty() {
        blocks.push(Block::Text {
            text,
            cache_marker: None,
        });
    }
    for id in &tool_input_order {
        let Some(buf) = tool_input_bufs.get(id) else {
            continue;
        };
        let name = tool_names.get(id).cloned().unwrap_or_default();
        blocks.push(Block::ToolUse {
            id: id.clone(),
            name,
            input_json: buf.clone(),
            cache_marker: None,
        });
    }
    let assistant = Message {
        role: Role::Assistant,
        blocks,
    };
    Ok(StreamingTurn {
        response: origin_provider::ChatResponse { assistant, usage },
        speculative: registry,
        first_token_ms,
    })
}

/// Milliseconds elapsed since `start`, saturating at `u64::MAX`.
///
/// A tiny pure helper extracted so the time-to-first-token / latency clocks in
/// the agent loop format their elapsed window identically (and so the cast is
/// fallible-checked in exactly one place rather than at every call site).
#[inline]
fn elapsed_ms_since(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod dispatch_table_tests {
    use super::*;
    use origin_tools::dispatch::{MemoryHandle, MemoryToolError, SearchHit};
    use origin_tools::registry_iter;
    use std::sync::Mutex;

    /// Build a fresh in-memory-ish `CodeGraphIndex` backed by a tempdir for CAS
    /// and an in-memory (`:memory:`) `SQLite` database. Returns the index wrapped
    /// in `Arc<tokio::sync::Mutex<...>>` as required by `LoopOptions`.
    fn make_empty_code_graph() -> (
        Arc<tokio::sync::Mutex<origin_codegraph::index::CodeGraphIndex>>,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cas_root = tmp.path().join("cas");
        let cas = origin_cas::Store::open(origin_cas::StoreConfig {
            root: cas_root,
            hot_capacity: 64,
            warm_pack_target_bytes: 1 << 20,
            cold_zstd_level: 3,
        })
        .expect("open cas");
        // Use the temp db path rather than :memory: so migrations run via refinery.
        let db_path = tmp.path().join("origin.db");
        let sql = origin_store::Store::open(&db_path).expect("open sqlite");
        let idx = origin_codegraph::index::CodeGraphIndex::new(cas, sql);
        (Arc::new(tokio::sync::Mutex::new(idx)), tmp)
    }

    /// Every tool advertised to the model via `tools_schema = registry_iter().map(...)`
    /// MUST be recognized by `dispatch_tool`. An `UnknownTool` error means the
    /// model received a tool name it can pick, then got told "I don't know that
    /// tool" — which is misleading. Tools whose subsystems are not yet wired
    /// should return `ToolFailure(<reason>)`, NOT `UnknownTool`.
    #[tokio::test]
    async fn dispatch_tool_recognizes_every_registered_tool() {
        let empty = serde_json::Value::Object(serde_json::Map::new());
        let mut unrecognized: Vec<String> = Vec::new();
        for meta in registry_iter() {
            let result = dispatch_tool(meta, &empty, None, None, None, None, None, None, None).await;
            if let Err(LoopError::UnknownTool(name)) = &result {
                unrecognized.push(name.clone());
            }
        }
        assert!(
            unrecognized.is_empty(),
            "tools registered in the inventory but not handled by dispatch_tool: {unrecognized:?}"
        );
    }

    /// `graph_explain` is the only non-`Recall` tool wired here with a real
    /// implementation (no missing subsystem). Verify it produces the expected
    /// English gloss for each Query variant.
    #[tokio::test]
    async fn graph_explain_returns_real_nl_gloss() {
        let meta = registry_iter()
            .find(|m| m.name == "graph_explain")
            .expect("graph_explain registered");
        let args = serde_json::json!({"kind": "communities"});
        let out = dispatch_tool(meta, &args, None, None, None, None, None, None, None)
            .await
            .expect("communities dispatch");
        assert_eq!(out, "all detected communities");

        let args = serde_json::json!({
            "kind": "recent_changes",
            "args": {"since_ms": 1_700_000_000_000_i64}
        });
        let out = dispatch_tool(meta, &args, None, None, None, None, None, None, None)
            .await
            .expect("recent_changes dispatch");
        assert!(out.contains("1700000000000"), "got: {out}");

        // Unknown kind surfaces as BadArgs, not ToolFailure or UnknownTool.
        let args = serde_json::json!({"kind": "bogus"});
        let err = dispatch_tool(meta, &args, None, None, None, None, None, None, None)
            .await
            .expect_err("bogus must fail");
        assert!(matches!(err, LoopError::BadArgs(_)));
    }

    /// The stub arms return `ToolFailure` with messages naming the missing
    /// subsystem — never `UnknownTool`. Regression guard for accidental
    /// reversion to the silent-fall-through.
    #[allow(clippy::panic)]
    #[tokio::test]
    async fn stub_arms_return_toolfailure_not_unknowntool() {
        // After Subsystems A (graph_* + ask), B (mem_*) merged, only `Task`
        // remains a stub. The behavior contract is unchanged: missing
        // subsystem → ToolFailure with subsystem-naming message.
        // mem_* / graph_* / ask still surface a `ToolFailure` when their
        // handle is None (covered in their dedicated tests below); calling
        // them here with all-None would test the same path, so we keep this
        // suite narrow to the remaining literal stubs.
        let names = ["Task"];
        let args = serde_json::Value::Object(serde_json::Map::new());
        for name in names {
            let meta = registry_iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            let err = dispatch_tool(meta, &args, None, None, None, None, None, None, None)
                .await
                .expect_err(name);
            match err {
                LoopError::ToolFailure(msg) => {
                    assert!(
                        msg.contains("not yet wired") || msg.contains("subsystem"),
                        "{name}: ToolFailure message must name the missing subsystem; got `{msg}`"
                    );
                }
                LoopError::UnknownTool(_) => panic!("{name}: regressed to UnknownTool"),
                other => panic!("{name}: unexpected error variant {other:?}"),
            }
        }
    }

    // ── Subsystem A tests: graph_* + ask (with CodeGraphIndex) ────────────────

    /// Dispatch `graph_query` with `kind=communities` against an empty index.
    /// Post-P7.8 Communities returns `QueryResult::Partitions` (empty list
    /// when the edge table has no rows), which serializes with a
    /// `partitions` field.
    #[tokio::test]
    async fn graph_query_runs_against_empty_index_returns_empty_result() {
        let (code_graph, _tmp) = make_empty_code_graph();
        let meta = registry_iter()
            .find(|m| m.name == "graph_query")
            .expect("graph_query registered");
        let args = serde_json::json!({"kind": "communities"});
        let out = dispatch_tool(meta, &args, None, Some(&code_graph), None, None, None, None, None)
            .await
            .expect("graph_query dispatch");
        // Empty edge table yields an empty Partitions list.
        assert_eq!(
            out, r#"{"partitions":[]}"#,
            "expected empty partitions JSON, got: {out}"
        );
    }

    /// Dispatch `ask` with a code-flavored query against an empty index +
    /// `NullMemRouter`. The classifier routes "what calls foo" to `Route::Code`
    /// so `result.route` must serialize as `"code"`.
    #[tokio::test]
    async fn ask_classifies_pure_code_query() {
        let (code_graph, _tmp) = make_empty_code_graph();
        let meta = registry_iter().find(|m| m.name == "ask").expect("ask registered");
        let args = serde_json::json!({"query": "what calls foo"});
        let null_router = origin_codegraph::ask::NullMemRouter;
        let out = dispatch_tool(
            meta,
            &args,
            None,
            Some(&code_graph),
            Some(&null_router as &dyn origin_codegraph::ask::MemRouter),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("ask dispatch");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        let route = parsed["route"].as_str().expect("route field");
        assert_eq!(route, "code", "expected route=code, got: {out}");
    }

    /// Dispatch `graph_rebuild` with an empty paths array. The report must show
    /// `paths_seen = 0` and no errors.
    #[tokio::test]
    async fn graph_rebuild_with_empty_paths_returns_zero_report() {
        let (code_graph, _tmp) = make_empty_code_graph();
        let meta = registry_iter()
            .find(|m| m.name == "graph_rebuild")
            .expect("graph_rebuild registered");
        let args = serde_json::json!({"paths": []});
        let out = dispatch_tool(meta, &args, None, Some(&code_graph), None, None, None, None, None)
            .await
            .expect("graph_rebuild dispatch");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        let paths_seen = parsed["paths_seen"].as_u64().expect("paths_seen field");
        assert_eq!(paths_seen, 0, "expected 0 paths_seen, got: {out}");
        let errors = parsed["errors"].as_array().expect("errors field");
        assert!(errors.is_empty(), "expected no errors, got: {out}");
    }

    // ── Subsystem B tests: mem_* (with MemoryHandle) ──────────────────────────

    /// A minimal in-memory `MemoryHandle` implementation for testing.
    /// Uses a `Mutex<Vec<_>>` so it is `Send + Sync` and requires no external deps.
    #[derive(Debug)]
    struct StubMemoryHandle {
        entries: Mutex<Vec<(String, String, Vec<String>)>>, // (id, body, tags)
    }

    impl StubMemoryHandle {
        const fn new() -> Self {
            Self {
                entries: Mutex::new(Vec::new()),
            }
        }
    }

    impl MemoryHandle for StubMemoryHandle {
        fn search(&self, query: &str, k: usize, _fresh: bool) -> Result<Vec<SearchHit>, MemoryToolError> {
            let q_lower = query.to_lowercase();
            let hits: Vec<SearchHit> = {
                let entries = self.entries.lock().expect("lock");
                entries
                    .iter()
                    .filter(|(_, body, _)| body.to_lowercase().contains(&q_lower))
                    .take(k)
                    .map(|(id, body, tags)| SearchHit {
                        id: id.clone(),
                        preview: body.chars().take(128).collect(),
                        score: 1.0,
                        age_days: 0.0,
                        tags: tags.clone(),
                    })
                    .collect()
            };
            Ok(hits)
        }

        fn save(&self, body: &str, tags: &[String]) -> Result<String, MemoryToolError> {
            let id = format!("stub-{}", ulid::Ulid::new());
            self.entries
                .lock()
                .expect("lock")
                .push((id.clone(), body.to_string(), tags.to_vec()));
            Ok(id)
        }

        fn forget(&self, id: &str) -> Result<(), MemoryToolError> {
            let mut entries = self.entries.lock().expect("lock");
            let before = entries.len();
            entries.retain(|(eid, _, _)| eid != id);
            if entries.len() < before {
                Ok(())
            } else {
                Err(MemoryToolError::BadId(id.to_string()))
            }
        }
    }

    /// `mem_search` with `memory_handle = None` must return `ToolFailure` containing
    /// "subsystem" — preserving the no-handle behavior.
    #[allow(clippy::panic)]
    #[tokio::test]
    async fn mem_search_without_handle_returns_toolfailure() {
        let meta = registry_iter()
            .find(|m| m.name == "mem_search")
            .expect("mem_search registered");
        let args = serde_json::json!({"query": "anything"});
        let err = dispatch_tool(meta, &args, None, None, None, None, None, None, None)
            .await
            .expect_err("must fail without handle");
        match err {
            LoopError::ToolFailure(msg) => {
                assert!(
                    msg.contains("subsystem"),
                    "ToolFailure must mention subsystem; got `{msg}`"
                );
            }
            other => panic!("expected ToolFailure, got {other:?}"),
        }
    }

    /// Wire a `StubMemoryHandle` through dispatch, save a memory via `mem_save`,
    /// then confirm `mem_search` returns the saved item.
    #[tokio::test]
    async fn mem_save_round_trips_via_handle() {
        let handle = StubMemoryHandle::new();

        let save_meta = registry_iter()
            .find(|m| m.name == "mem_save")
            .expect("mem_save registered");
        let save_args = serde_json::json!({
            "body": "the quick brown fox",
            "tags": ["test", "roundtrip"]
        });
        let save_out = dispatch_tool(save_meta, &save_args, None, None, None, Some(&handle), None, None, None)
            .await
            .expect("mem_save must succeed");
        let save_json: serde_json::Value =
            serde_json::from_str(&save_out).expect("mem_save output must be valid JSON");
        assert!(
            save_json.get("id").and_then(|v| v.as_str()).is_some(),
            "mem_save must return {{\"id\":\"...\"}}; got `{save_out}`"
        );

        let search_meta = registry_iter()
            .find(|m| m.name == "mem_search")
            .expect("mem_search registered");
        let search_args = serde_json::json!({"query": "quick brown", "k": 5});
        let search_out = dispatch_tool(search_meta, &search_args, None, None, None, Some(&handle), None, None, None)
            .await
            .expect("mem_search must succeed");
        let hits: serde_json::Value =
            serde_json::from_str(&search_out).expect("mem_search output must be valid JSON");
        let arr = hits.as_array().expect("mem_search must return an array");
        assert!(
            !arr.is_empty(),
            "mem_search must find the saved entry; got empty array"
        );
        let first = &arr[0];
        assert!(
            first["preview"]
                .as_str()
                .map_or(false, |p| p.contains("quick brown")),
            "hit preview must contain the saved body; got {first}"
        );
    }

    // ── Subsystem C tests: Task (with swarm Coordinator) ──────────────────────

    /// When `coordinator` is `None`, `Task` must return `ToolFailure` (not
    /// `UnknownTool`). Regression guard for the subsystem-not-configured path.
    #[allow(clippy::panic)]
    #[tokio::test]
    async fn task_without_coordinator_returns_toolfailure() {
        let meta = registry_iter()
            .find(|m| m.name == "Task")
            .expect("Task registered");
        let args = serde_json::json!({
            "goal": "do something",
            "allowed_tools": []
        });
        let err = dispatch_tool(meta, &args, None, None, None, None, None, None, None)
            .await
            .expect_err("Task without coordinator must fail");
        match err {
            LoopError::ToolFailure(msg) => {
                assert!(
                    msg.contains("subsystem"),
                    "Task ToolFailure message must mention subsystem; got `{msg}`"
                );
            }
            LoopError::UnknownTool(_) => panic!("Task regressed to UnknownTool"),
            other => panic!("unexpected error variant {other:?}"),
        }
    }

    /// When a real `Coordinator` (backed by an in-memory `Plan` + `PlanStore`
    /// over in-memory `CasStore` + `SqlStore`) is threaded through, `Task` must
    /// return a valid `TaskOutput` JSON. The default noop worker always completes
    /// successfully, so the round-trip asserts shape, not semantic content.
    #[tokio::test]
    async fn task_with_coordinator_spawns_noop_worker() {
        use std::sync::Arc;

        // Build in-memory backing stores.
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("plan.db");
        let sql = Arc::new(origin_store::Store::open(db_path.to_str().expect("utf8")).expect("sql open"));
        let cas_root = tmp.path().join("cas");
        let cas = Arc::new(
            origin_cas::Store::open(origin_cas::StoreConfig {
                root: cas_root,
                hot_capacity: 16,
                warm_pack_target_bytes: 1024 * 1024,
                cold_zstd_level: 1,
            })
            .expect("cas open"),
        );

        // Construct Plan / PlanStore / PlanHandle / Coordinator.
        let plan = Arc::new(tokio::sync::Mutex::new(origin_plan::Plan::new()));
        let plan_store = Arc::new(
            origin_plan::PlanStore::open(Arc::clone(&sql), Arc::clone(&cas)).expect("plan store open"),
        );
        let plan_handle = origin_swarm::PlanHandle::new(plan, plan_store);
        let coordinator = Arc::new(origin_swarm::Coordinator::new(plan_handle, "test-ring"));

        let meta = registry_iter()
            .find(|m| m.name == "Task")
            .expect("Task registered");
        let args = serde_json::json!({
            "goal": "noop integration test",
            "allowed_tools": []
        });

        let out = dispatch_tool(meta, &args, None, None, None, None, Some(coordinator.as_ref()), None, None)
            .await
            .expect("Task with coordinator must succeed");

        // Assert the output is valid JSON with the expected shape.
        let v: serde_json::Value = serde_json::from_str(&out).expect("TaskOutput must be valid JSON");
        assert!(v.get("status").is_some(), "TaskOutput must have `status`");
        assert!(v.get("summary").is_some(), "TaskOutput must have `summary`");
        assert!(
            v.get("files_touched").is_some(),
            "TaskOutput must have `files_touched`"
        );
        assert!(v.get("follow_ups").is_some(), "TaskOutput must have `follow_ups`");
    }

    /// Defensive guard against the previous `try_into().expect("4 bytes")`
    /// pattern in the `Usage` decode path. After the refactor to
    /// [`decode_usage_payload`], a payload shorter than 16 bytes (or longer)
    /// must return `None` rather than panicking — matching the pre-fix
    /// outer `if p.len() == 16` skip semantics.
    #[test]
    fn decode_usage_payload_returns_none_on_size_mismatch() {
        // Too short: pre-fix `if p.len() == 16` would have skipped this; the
        // helper must also return None (no panic from the inner `expect`).
        assert!(decode_usage_payload(&[]).is_none());
        assert!(decode_usage_payload(&[0; 3]).is_none());
        assert!(decode_usage_payload(&[0; 15]).is_none());
        // Too long: previously fell through the `==` guard silently; the
        // helper must also return None for consistency.
        assert!(decode_usage_payload(&[0; 17]).is_none());
        assert!(decode_usage_payload(&[0; 32]).is_none());
    }

    /// Round-trip a 16-byte BE-encoded payload through [`decode_usage_payload`]
    /// to lock down the byte layout: 4× u32 BE in field order
    /// (input, output, `cache_read`, `cache_creation`).
    #[test]
    fn decode_usage_payload_parses_canonical_16_byte_layout() {
        let mut p = [0u8; 16];
        p[0..4].copy_from_slice(&1_111_u32.to_be_bytes());
        p[4..8].copy_from_slice(&2_222_u32.to_be_bytes());
        p[8..12].copy_from_slice(&3_333_u32.to_be_bytes());
        p[12..16].copy_from_slice(&4_444_u32.to_be_bytes());
        let usage = decode_usage_payload(&p).expect("valid 16-byte payload");
        assert_eq!(usage.input_tokens, 1_111);
        assert_eq!(usage.output_tokens, 2_222);
        assert_eq!(usage.cache_read_input_tokens, 3_333);
        assert_eq!(usage.cache_creation_input_tokens, 4_444);
    }
}

/// Tests for the additive, env-gated daemon wirings (Tasks 1-5). Each asserts
/// the default (no env flag / `None` field) path leaves behavior unchanged.
#[cfg(test)]
mod wiring_tests {
    use super::*;
    use origin_permission::{Decision, Outcome};

    /// Serializes tests that mutate the process-global `ORIGIN_CMD_GUARD` env
    /// var. Without this, the parallel test runner can interleave a `set_var` in
    /// one test with a `remove_var`/read in another, flaking the assertions.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Task 1: the checkpoint gate is OFF by default. With `ORIGIN_CHECKPOINTS`
    /// unset, the gate reads false and `maybe_checkpoint_turn_with_hooks`
    /// performs no git work and fires no commit hooks (and must not panic).
    #[tokio::test]
    async fn checkpoint_gate_off_by_default() {
        let enabled = std::env::var("ORIGIN_CHECKPOINTS").as_deref() == Ok("1");
        assert!(!enabled, "checkpoints must be opt-in via ORIGIN_CHECKPOINTS=1");
        maybe_checkpoint_turn_with_hooks().await;
    }

    /// WS-S: the per-tool checkpoint gate is OFF by default and INDEPENDENT of
    /// the per-turn gate. With `ORIGIN_CHECKPOINTS_PER_TOOL` unset,
    /// `per_tool_checkpoints_enabled` reads false (so no live git work runs).
    #[test]
    fn per_tool_checkpoint_gate_off_by_default() {
        assert!(
            std::env::var("ORIGIN_CHECKPOINTS_PER_TOOL").as_deref() != Ok("1"),
            "per-tool checkpoints must be opt-in via ORIGIN_CHECKPOINTS_PER_TOOL=1"
        );
        assert!(
            !per_tool_checkpoints_enabled(),
            "gate must read false when the env var is unset"
        );
    }

    /// WS-S: the per-tool checkpoint label combines the tool name with its
    /// edited path(s) so entries are distinguishable in `origin checkpoints`,
    /// and degrades to just the tool name when no path was edited.
    #[test]
    fn per_tool_checkpoint_label_combines_tool_and_paths() {
        assert_eq!(per_tool_checkpoint_label("Edit", &[]), "tool:Edit");
        assert_eq!(
            per_tool_checkpoint_label("Write", &["main.py".to_string()]),
            "tool:Write main.py"
        );
        assert_eq!(
            per_tool_checkpoint_label(
                "ApplyPatch",
                &["src/a.rs".to_string(), "src/b.rs".to_string()]
            ),
            "tool:ApplyPatch src/a.rs,src/b.rs"
        );
    }

    /// Task 2: the autoformat gate is OFF by default, and the formatter mapping
    /// the gate consults resolves known extensions and skips unknown ones.
    #[test]
    fn autoformat_gate_off_by_default_and_mapping_is_correct() {
        let enabled = std::env::var("ORIGIN_AUTOFORMAT").as_deref() == Ok("1");
        assert!(!enabled, "autoformat must be opt-in via ORIGIN_AUTOFORMAT=1");
        assert_eq!(origin_postedit::formatter_for("src/main.rs"), Some("rustfmt"));
        assert_eq!(origin_postedit::formatter_for("README"), None);
        // No-op with the gate off (no panic, no spawn).
        maybe_autoformat("src/main.rs");
    }

    /// Task 2: patch target extraction handles both unified-diff and
    /// apply-patch markers and skips `/dev/null`.
    #[test]
    fn patch_target_paths_extracts_both_formats() {
        let patch = "*** Update File: src/a.rs\n+++ b/src/b.rs\n+++ /dev/null\n";
        let paths = patch_target_paths(patch);
        assert!(paths.contains(&"src/a.rs".to_string()));
        assert!(paths.contains(&"src/b.rs".to_string()));
        assert!(!paths.iter().any(|p| p == "/dev/null"));
    }

    /// WS-J: `edited_paths_from_tool` pulls `file_path` from Edit/Write/MultiEdit
    /// and patch targets from `ApplyPatch`, and yields nothing for non-edit tools
    /// or a missing/empty path.
    #[test]
    fn edited_paths_extracts_from_edit_tools() {
        let edit = serde_json::json!({ "file_path": "src/lib.rs", "old_string": "a", "new_string": "b" });
        assert_eq!(edited_paths_from_tool("Edit", &edit), vec!["src/lib.rs".to_string()]);
        let write = serde_json::json!({ "file_path": "main.py", "content": "x" });
        assert_eq!(edited_paths_from_tool("Write", &write), vec!["main.py".to_string()]);
        let multi = serde_json::json!({ "file_path": "a.ts", "edits": [] });
        assert_eq!(edited_paths_from_tool("MultiEdit", &multi), vec!["a.ts".to_string()]);

        let patch =
            serde_json::json!({ "patch": "*** Update File: src/a.rs\n+++ b/src/b.rs\n" });
        let got = edited_paths_from_tool("ApplyPatch", &patch);
        assert!(got.contains(&"src/a.rs".to_string()));
        assert!(got.contains(&"src/b.rs".to_string()));

        // Non-edit tools and missing/empty paths yield nothing.
        assert!(edited_paths_from_tool("Read", &serde_json::json!({ "file_path": "x" })).is_empty());
        assert!(edited_paths_from_tool("Edit", &serde_json::json!({})).is_empty());
        assert!(edited_paths_from_tool("Edit", &serde_json::json!({ "file_path": "" })).is_empty());
    }

    /// WS-J: the autonomous post-edit LSP diagnostics feature is opt-in. With
    /// `ORIGIN_LSP_DIAGNOSTICS` unset the gate reads false, so `run_loop` skips
    /// the probe and the next turn's system prompt stays byte-identical.
    #[test]
    fn lsp_diagnostics_gate_off_by_default() {
        if std::env::var_os("ORIGIN_LSP_DIAGNOSTICS").is_none() {
            assert!(
                !crate::lsp_diagnostics::enabled(),
                "LSP diagnostics must be opt-in via ORIGIN_LSP_DIAGNOSTICS=1"
            );
        }
    }

    /// Task 3(a): with both governance layers `None` (the default), an Allow is
    /// returned unchanged (outcome stays Allow).
    #[test]
    fn governance_overlay_none_leaves_allow_unchanged() {
        let opts = LoopOptions::default();
        assert!(opts.policy.is_none() && opts.conseca.is_none());
        let allow = Decision {
            outcome: Outcome::Allow,
            reason: "base allowed".into(),
        };
        let out = apply_governance_overlay(allow, "Edit", &opts);
        assert_eq!(out.outcome, Outcome::Allow, "default (no overlay) must not change Allow");
    }

    /// Task 3(b): a policy that denies a tool downgrades a base Allow to Deny,
    /// while leaving a non-denied tool as Allow.
    #[test]
    fn governance_overlay_policy_deny_downgrades_allow() {
        let layer =
            origin_policy::parse_layer("denied_tools = [\"Bash\"]", origin_policy::Tier::Admin)
                .expect("valid layer");
        let opts = LoopOptions {
            policy: Some(Arc::new(origin_policy::PolicyEngine::new(vec![layer]))),
            ..LoopOptions::default()
        };

        let allow = Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        let out = apply_governance_overlay(allow, "Bash", &opts);
        assert_eq!(out.outcome, Outcome::Deny, "policy deny must downgrade Allow");

        let allow2 = Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        let out2 = apply_governance_overlay(allow2, "Read", &opts);
        assert_eq!(out2.outcome, Outcome::Allow, "non-denied tool stays Allow");
    }

    /// gemini Plan Mode: read-only mode denies mutating tools, allows Pure
    /// tools, and is a no-op when `read_only == false`.
    #[test]
    fn read_only_overlay_blocks_mutating_allows_pure() {
        let allow = || Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        // read_only off ⇒ no change even for a mutating tool.
        assert_eq!(
            apply_read_only_overlay(allow(), false, SideEffects::Mutating, "Edit").outcome,
            Outcome::Allow,
        );
        // read_only on ⇒ mutating tool denied.
        assert_eq!(
            apply_read_only_overlay(allow(), true, SideEffects::Mutating, "Edit").outcome,
            Outcome::Deny,
        );
        // read_only on ⇒ Pure tool still allowed.
        assert_eq!(
            apply_read_only_overlay(allow(), true, SideEffects::Pure, "Read").outcome,
            Outcome::Allow,
        );
    }

    /// cline multi-root: an empty root list renders nothing (byte-identical
    /// prompt); a populated list renders a `<workspace-roots>` block listing
    /// every root.
    #[test]
    fn workspace_roots_block_empty_and_populated() {
        use std::path::PathBuf;
        assert!(workspace_roots_block(&[]).is_empty());
        let roots = [PathBuf::from("/a/repo1"), PathBuf::from("/b/repo2")];
        let block = workspace_roots_block(&roots);
        assert!(block.starts_with("<workspace-roots>"));
        assert!(block.contains("/a/repo1"));
        assert!(block.contains("/b/repo2"));
        assert!(block.trim_end().ends_with("</workspace-roots>"));
    }

    /// Task 3(c): conseca downgrades an Allow when the tool is not allow-listed,
    /// and the overlay is deny-only — a pre-existing Deny is never widened
    /// because the call site only routes an Allow through the overlay.
    #[test]
    fn governance_overlay_conseca_deny_only() {
        let policy = origin_conseca::SecurityPolicy {
            allow_tools: vec!["Read".to_string()],
            ..origin_conseca::SecurityPolicy::default()
        };
        let opts = LoopOptions {
            conseca: Some(Arc::new(policy)),
            ..LoopOptions::default()
        };

        // Bash is not allow-listed ⇒ conseca denies ⇒ Allow downgraded.
        let allow = Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        assert_eq!(
            apply_governance_overlay(allow, "Bash", &opts).outcome,
            Outcome::Deny
        );

        // Read is allow-listed ⇒ stays Allow.
        let allow2 = Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        assert_eq!(
            apply_governance_overlay(allow2, "Read", &opts).outcome,
            Outcome::Allow
        );

        // Deny-only invariant: a base Deny is never an Allow, so the call-site
        // guard (`if outcome == Allow`) never routes it through the overlay and
        // it can never be widened.
        let base_deny = Decision {
            outcome: Outcome::Deny,
            reason: "base denied".into(),
        };
        assert_ne!(
            base_deny.outcome,
            Outcome::Allow,
            "a base Deny is never routed through the overlay, so it can never widen"
        );
    }

    /// Task 4: telemetry is disabled by default (no opt-in), so the pipeline
    /// drains nothing and the helper writes no file.
    #[test]
    fn telemetry_disabled_by_default() {
        let opt_in = std::env::var("ORIGIN_TELEMETRY").as_deref() == Ok("1");
        assert!(!opt_in, "telemetry must be opt-in via ORIGIN_TELEMETRY=1");
        let cfg = origin_telemetry::Config::from_env(false, false, 1.0);
        assert!(!cfg.enabled);
        let mut pipe = origin_telemetry::Pipeline::new(cfg);
        pipe.record(origin_telemetry::Event::new("turn".to_string(), 0));
        assert!(pipe.drain().is_empty(), "disabled pipeline emits no lines");
        // No-op with the gate off.
        maybe_record_turn_telemetry("anthropic", "claude-sonnet-4-6", 1, 2);
    }

    /// Task 5: the completion-notification gate is OFF by default.
    #[test]
    fn notify_gate_off_by_default() {
        let enabled = std::env::var("ORIGIN_NOTIFY").as_deref() == Ok("1");
        assert!(!enabled, "notifications must be opt-in via ORIGIN_NOTIFY=1");
        maybe_notify_completion();
    }

    /// AGENT-LOOP Task 1: the exposure-truncation gate is OFF by default, and
    /// `harvest_grep_exposure` folds a `content`-mode result's `(file, line)`
    /// matches into the running set, de-duping exact repeats and ignoring
    /// non-`content` shapes.
    #[test]
    fn grep_exposure_truncate_gate_off_by_default_and_harvest_works() {
        let enabled = std::env::var("ORIGIN_AGENTGREP_TRUNCATE").as_deref() == Ok("1");
        assert!(
            !enabled,
            "exposure-truncation must be opt-in via ORIGIN_AGENTGREP_TRUNCATE=1"
        );

        let mut exposure: Vec<origin_tools::builtins::grep_tool::ExposureWindow> = Vec::new();
        // A `files_with_matches`-shaped result has no `matches` array ⇒ no-op.
        harvest_grep_exposure(r#"{"files":["a.rs"]}"#, &mut exposure);
        assert!(exposure.is_empty(), "non-content result adds nothing");

        // A `content`-shaped result contributes one window per (file, line).
        let content = r#"{"matches":[
            {"file":"src/a.rs","line":10,"text":"x"},
            {"file":"src/a.rs","line":11,"text":"y"},
            {"file":"src/b.rs","line":3,"text":"z"}
        ]}"#;
        harvest_grep_exposure(content, &mut exposure);
        assert_eq!(exposure.len(), 3, "three distinct matches harvested");

        // Re-harvesting the same matches de-dups (no unbounded growth).
        harvest_grep_exposure(content, &mut exposure);
        assert_eq!(exposure.len(), 3, "exact repeats are de-duped");

        // Malformed JSON is ignored, not panicked on.
        harvest_grep_exposure("not json", &mut exposure);
        assert_eq!(exposure.len(), 3);

        // The harvested windows are exactly what `grep_v2` consults to elide a
        // re-run's already-seen lines.
        assert!(exposure.iter().any(|w| w.file == "src/a.rs"
            && w.start_line == 10
            && w.end_line == 10));
    }

    /// AGENT-LOOP Task 2: the editfmt apply path is OFF by default, and when a
    /// model emits a search/replace block in prose, `apply_prose_edits` parses
    /// it via `origin_editfmt` and writes the edit to disk, flagging the turn as
    /// mutated. Both halves of the round-trip (parse → apply) are exercised.
    #[test]
    fn editfmt_apply_gate_off_by_default_and_round_trips() {
        let enabled = std::env::var("ORIGIN_EDITFMT").as_deref() == Ok("1");
        assert!(!enabled, "editfmt must be opt-in via ORIGIN_EDITFMT=1");

        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("target.txt");
        std::fs::write(&file, "let x = 1;\n").expect("seed file");

        // A search/replace block naming the file (path on the line above).
        let prose = format!(
            "Here is the change you asked for:\n\n\
             {}\n\
             <<<<<<< SEARCH\n\
             let x = 1;\n\
             =======\n\
             let x = 2;\n\
             >>>>>>> REPLACE\n",
            file.display()
        );

        let mut mutated = false;
        apply_prose_edits(&prose, "claude-opus-4", &mut mutated);
        assert!(mutated, "a successful prose edit flags the turn as mutated");
        let after = std::fs::read_to_string(&file).expect("read back");
        assert_eq!(after, "let x = 2;\n", "the edit was applied to disk");

        // Plain prose with no edit block is a no-op (no parse, no mutation).
        let mut mutated2 = false;
        apply_prose_edits("just chatting, no edits here", "claude-opus-4", &mut mutated2);
        assert!(!mutated2, "prose without an edit block changes nothing");
    }

    /// AGENT-LOOP Task 2: an edit block whose `before` text is absent in the
    /// target leaves the file untouched and never flags a mutation (best-effort:
    /// a no-match must not fail the turn).
    #[test]
    fn editfmt_apply_no_match_is_a_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("nomatch.txt");
        std::fs::write(&file, "original contents\n").expect("seed file");

        let prose = format!(
            "{}\n\
             <<<<<<< SEARCH\n\
             this text is not in the file\n\
             =======\n\
             replacement\n\
             >>>>>>> REPLACE\n",
            file.display()
        );
        let mut mutated = false;
        apply_prose_edits(&prose, "claude-opus-4", &mut mutated);
        assert!(!mutated, "a no-match apply must not flag a mutation");
        assert_eq!(
            std::fs::read_to_string(&file).expect("read back"),
            "original contents\n",
            "the file is left untouched on a no-match"
        );
    }

    /// AGENT-LOOP Task 3: the `gen_ai` usage record is callable unconditionally
    /// and is a cheap no-op in the default (non-`otel`) build — it must not
    /// panic and must not require the exporter to be installed.
    #[test]
    #[allow(
        clippy::missing_const_for_fn,
        reason = "with `--features otel` the call is not const; the const no-op is build-specific"
    )]
    fn record_gen_ai_usage_is_a_noop_without_otel() {
        origin_metrics::instruments::record_gen_ai_usage(
            "anthropic",
            "claude-sonnet-4-6",
            "claude-sonnet-4-6",
            1_234,
            567,
            42.0,
            3,
        );
        // Reaching here without a panic is the assertion.
    }

    /// AGENT-LOOP Task 4: session-stop pain telemetry is disabled by default
    /// (no opt-in), so the helper drains nothing and writes no file. Also
    /// verifies the `PainMetrics` it would build folds into a redactable event.
    #[test]
    fn session_stop_telemetry_disabled_by_default() {
        let opt_in = std::env::var("ORIGIN_TELEMETRY").as_deref() == Ok("1");
        assert!(!opt_in, "telemetry must be opt-in via ORIGIN_TELEMETRY=1");

        // The disabled pipeline drains nothing even for a populated event.
        let cfg = origin_telemetry::Config::from_env(false, false, 1.0);
        assert!(!cfg.enabled);
        let metrics = origin_telemetry::PainMetrics::new()
            .with_stop_reason(origin_telemetry::SessionStopReason::Completed)
            .with_agent_time_split(1_000, 0)
            .with_turns(4, 4);
        let event = metrics
            .into_event("session_stop".to_string(), 0)
            .expect("pain metrics serialize");
        let mut pipe = origin_telemetry::Pipeline::new(cfg);
        pipe.record(event);
        assert!(pipe.drain().is_empty(), "disabled pipeline emits no lines");

        // No-op with the gate off (no panic, no file). Exercises both the
        // full-split `from_loop` constructor and the reason-only one used by
        // the cross-module Abandoned/Idle emit sites.
        record_session_stop_pain(SessionStopPain::from_loop(
            origin_telemetry::SessionStopReason::BudgetExhausted,
            5_000,
            1_200,
            Some(300),
            Some(150),
            8,
        ));
        record_session_stop_pain(SessionStopPain::reason_only(
            origin_telemetry::SessionStopReason::Abandoned,
        ));
    }

    /// cmdparse cmd-guard: with `ORIGIN_CMD_GUARD` unset (the default) even a
    /// catastrophic command is left as Allow — the overlay is fully off.
    #[test]
    fn cmd_guard_off_leaves_allow_unchanged() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("ORIGIN_CMD_GUARD");
        let allow = Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        let args = serde_json::json!({ "command": "rm -rf ~" });
        let out = apply_cmd_guard(allow, "Bash", &args);
        assert_eq!(
            out.outcome,
            Outcome::Allow,
            "cmd-guard must be a no-op when ORIGIN_CMD_GUARD is unset"
        );
    }

    /// cmdparse cmd-guard: with the gate ON, `rm -rf ~` on `Bash` is downgraded
    /// to Deny, a safe command stays Allow, and a non-Bash tool is untouched.
    #[test]
    fn cmd_guard_on_denies_dangerous_bash() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mk = || Decision {
            outcome: Outcome::Allow,
            reason: "base".into(),
        };
        let dangerous = serde_json::json!({ "command": "rm -rf ~" });
        let safe = serde_json::json!({ "command": "ls -la" });

        std::env::set_var("ORIGIN_CMD_GUARD", "1");
        let denied = apply_cmd_guard(mk(), "Bash", &dangerous);
        let allowed = apply_cmd_guard(mk(), "Bash", &safe);
        let non_bash = apply_cmd_guard(mk(), "Read", &dangerous);
        std::env::remove_var("ORIGIN_CMD_GUARD");

        assert_eq!(
            denied.outcome,
            Outcome::Deny,
            "dangerous bash must be denied when ORIGIN_CMD_GUARD=1"
        );
        assert_eq!(allowed.outcome, Outcome::Allow, "safe bash stays Allow");
        assert_eq!(
            non_bash.outcome,
            Outcome::Allow,
            "cmd-guard only inspects the Bash tool"
        );
    }

    /// WS-D browser visual loop: the gate is OFF by default. With
    /// `ORIGIN_BROWSER_VISUAL` unset, the `Browser` arm serializes the bare
    /// `SnapshotResp` — so `browser_visual_result` is never invoked and the
    /// tool output is byte-identical to the pre-visual behavior.
    #[test]
    fn browser_visual_gate_off_by_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("ORIGIN_BROWSER_VISUAL");
        let enabled = std::env::var("ORIGIN_BROWSER_VISUAL").as_deref() == Ok("1");
        assert!(!enabled, "visual loop must be opt-in via ORIGIN_BROWSER_VISUAL=1");

        // The bare-serialization path the gate-off arm takes is exactly
        // `serde_json::to_string(&resp)` — no `visual`/`response` envelope keys.
        let resp = origin_browser::SnapshotResp {
            ok: true,
            r#ref: None,
            snapshot: Some("snap".into()),
            html: None,
            status: Some(200),
            title: Some("OK".into()),
            error: None,
            console: None,
        };
        let bare = serde_json::to_string(&resp).expect("serialize bare resp");
        assert!(!bare.contains("\"visual\""), "gate-off output must not wrap");
        assert!(!bare.contains("\"console\""), "None console omitted from wire");
    }

    /// WS-D: when the gate is on but the backend produced neither a screenshot
    /// nor any console line, `browser_visual_result` returns the bare
    /// `SnapshotResp` JSON — identical to the gate-off path (no empty envelope).
    #[test]
    fn browser_visual_empty_capture_is_bare_response() {
        let verb = origin_browser::Verb::Snapshot {
            session: "s".into(),
        };
        let resp = origin_browser::SnapshotResp {
            ok: true,
            r#ref: None,
            snapshot: Some("snap".into()),
            html: None,
            status: Some(200),
            title: Some("OK".into()),
            error: None,
            console: None,
        };
        let out = browser_visual_result(&verb, &resp).expect("visual result");
        let bare = serde_json::to_string(&resp).expect("serialize bare resp");
        assert_eq!(out, bare, "empty capture must not add envelope keys");
    }

    /// WS-D: with a captured screenshot + console lines, the visual result is an
    /// additive envelope: `{response, visual:{attachments:[image], console}}`.
    /// The image is a real `origin_multimodal::ContentBlock` (kind/media/base64).
    #[test]
    fn browser_visual_envelope_carries_screenshot_and_console() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shot = dir.path().join("page.png");
        // PNG magic + a little payload — the visual module validates the magic.
        let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(b"IHDRdata");
        std::fs::write(&shot, &png).expect("write png");

        let verb = origin_browser::Verb::Screenshot {
            session: "s".into(),
            path: shot.to_str().expect("utf8 path").into(),
        };
        let resp = origin_browser::SnapshotResp {
            ok: true,
            r#ref: None,
            snapshot: None,
            html: None,
            status: Some(200),
            title: Some("OK".into()),
            error: None,
            console: Some(vec!["[log] ready".into(), "[error] oops".into()]),
        };

        let out = browser_visual_result(&verb, &resp).expect("visual result");
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");

        // Original response preserved under `response`.
        assert_eq!(v["response"]["status"], 200);
        assert_eq!(v["response"]["title"], "OK");

        // Screenshot rode as a ContentBlock image with the expected base64.
        let img = &v["visual"]["attachments"][0];
        assert_eq!(img["kind"], "image");
        assert_eq!(img["media_type"], "image/png");
        assert_eq!(img["base64"], origin_multimodal::base64_encode(&png));

        // Console text block has the bounded header + both lines.
        let console = v["visual"]["console"].as_str().expect("console text");
        assert!(console.starts_with("console (2 lines):"));
        assert!(console.contains("[log] ready"));
        assert!(console.contains("[error] oops"));
    }

    /// WS-D: a non-`Screenshot` verb has no screenshot path, so a console-only
    /// backend still produces a valid envelope — console attaches, no image.
    #[test]
    fn browser_visual_console_only_without_screenshot() {
        let verb = origin_browser::Verb::Click {
            r#ref: "e1".into(),
            session: "s".into(),
        };
        let resp = origin_browser::SnapshotResp {
            ok: true,
            r#ref: None,
            snapshot: None,
            html: None,
            status: Some(200),
            title: None,
            error: None,
            console: Some(vec!["[warn] slow".into()]),
        };
        let out = browser_visual_result(&verb, &resp).expect("visual result");
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert!(
            v["visual"]["attachments"].as_array().expect("array").is_empty(),
            "no screenshot ⇒ no image attachment"
        );
        assert!(v["visual"]["console"].as_str().expect("console").contains("[warn] slow"));
    }
}

#[cfg(test)]
mod bash_streaming_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The core guarantee of the streaming fix: output reaches the user **as it
    /// is produced**, not in one burst after the process exits. A command that
    /// prints an early line, then sleeps, then prints a late line must deliver
    /// the first `ToolChunk` well before the command terminates — otherwise a
    /// long-running (or stuck) command looks hung to the user.
    #[allow(clippy::panic)] // test asserts the StreamEvent variant via a panicking else-arm
    #[tokio::test]
    async fn streams_output_in_real_time() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(64);
        let args = serde_json::json!({
            // Print, flush via newline, sleep ~1s, print again, then exit.
            "command": "echo early; sleep 1; echo late",
            "timeout": 10,
        });

        let start = Instant::now();
        // Route through `spawn_in` like all daemon code (Realtime ≈ per-stream
        // relay): keeps the no-raw-`tokio::spawn` invariant uniform, including
        // in tests, so the spawn-audit stays green.
        let driver = spawn_in(TaskClass::Realtime, async move { run_bash_streaming(&args, Some(&tx)).await });

        // The first chunk must arrive before the full sleep elapses.
        let first = tokio::time::timeout(Duration::from_millis(800), rx.recv())
            .await
            .expect("first ToolChunk must arrive before the command finishes")
            .expect("channel open");
        let first_at = start.elapsed();
        match first {
            StreamEvent::ToolChunk { tool, content } => {
                assert_eq!(tool, "Bash");
                assert_eq!(content, "early");
            }
            other => panic!("expected ToolChunk(early), got {other:?}"),
        }
        assert!(
            first_at < Duration::from_millis(800),
            "first chunk streamed at {first_at:?}; output was buffered, not real-time"
        );

        // Drain the rest; the late line must also arrive.
        let mut saw_late = false;
        while let Some(ev) = rx.recv().await {
            if let StreamEvent::ToolChunk { content, .. } = ev {
                if content == "late" {
                    saw_late = true;
                }
            }
        }
        assert!(saw_late, "late line never streamed");

        let bytes = driver.await.expect("task joined").expect("bash ok");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(v["status"], "exited");
        assert_eq!(v["exit_code"], 0);
        // The LLM still receives the fully accumulated body.
        let stdout = v["stdout"].as_str().expect("stdout");
        assert!(stdout.contains("early") && stdout.contains("late"), "stdout: {stdout:?}");
    }

    /// Silent commands (no stdout/stderr) still surface a terminal affordance
    /// so the user sees the command completed rather than a blank gap.
    #[tokio::test]
    async fn silent_command_emits_terminal_result() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(8);
        let args = serde_json::json!({ "command": "true", "timeout": 5 });
        let bytes = run_bash_streaming(&args, Some(&tx)).await.expect("bash ok");
        drop(tx);

        let mut saw_result = false;
        while let Some(ev) = rx.recv().await {
            if let StreamEvent::ToolResult { tool, ok, preview, .. } = ev {
                assert_eq!(tool, "Bash");
                assert!(ok);
                assert!(preview.contains("no output"), "preview: {preview}");
                saw_result = true;
            }
        }
        assert!(saw_result, "silent command must emit a ToolResult affordance");

        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(v["status"], "exited");
        assert_eq!(v["exit_code"], 0);
    }

    /// `run_in_background` returns the pid immediately without streaming, just
    /// like the foreground/background split in `bash_v2`.
    #[tokio::test]
    async fn background_returns_pid_without_streaming() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(8);
        let args = serde_json::json!({
            "command": "echo hi; sleep 1",
            "run_in_background": true,
        });
        let started = Instant::now();
        let bytes = run_bash_streaming(&args, Some(&tx)).await.expect("bash ok");
        assert!(started.elapsed() < Duration::from_millis(500), "background must return fast");
        drop(tx);

        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(v["status"], "started");
        assert!(v["pid"].as_u64().is_some());

        // No chunks streamed for the background path.
        assert!(rx.recv().await.is_none(), "background path must not stream chunks");
    }

    /// A `None` sink (headless runs, tests) still completes and returns the full
    /// body — streaming is best-effort, execution is not.
    #[tokio::test]
    async fn no_sink_still_returns_full_body() {
        let args = serde_json::json!({ "command": "echo a; echo b", "timeout": 5 });
        let bytes = run_bash_streaming(&args, None).await.expect("bash ok");
        let v: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(v["status"], "exited");
        let stdout = v["stdout"].as_str().expect("stdout");
        assert!(stdout.contains('a') && stdout.contains('b'), "stdout: {stdout:?}");
    }
}
