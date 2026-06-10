// SPDX-License-Identifier: Apache-2.0
use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt as _;
use origin_cli::cli_def::{Cli, Cmd, KeyringSub, PairSub, ProvidersSub, SessionsSub, TraceSub};
// Plugin subcommand is dispatched through `origin_cli::plugin::run`, which takes
// the `PluginSub` directly.
use origin_cli::goal_render::render_goal_event;
use origin_cli::input::{
    parse_clear_command, parse_mem_command, parse_model_command, parse_skill_command, parse_workflow_command,
    permission_answer, reduce_editor, InputAction,
};
use origin_cli::plan_panel_wiring::Wiring as PlanPanelWiring;
use origin_cli::tui::App;
use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::{Connection, Connector};
use origin_runtime::{spawn_in, TaskClass};
use origin_tui::composer::Composer;
use origin_tui::scheduler::{Handle, Scheduler};
use origin_tui::stream_widget::{Rect, StreamWidget};
use parking_lot::Mutex;
use serde::Deserialize;

#[derive(Deserialize)]
struct PromptReply {
    #[allow(dead_code)] // reconstructed live from stream deltas; only `turns` is used.
    assistant_text: String,
    turns: u32,
}

type SharedApp = Arc<Mutex<App>>;
type SharedComposer = Arc<Mutex<Composer>>;
type SharedWidget = Arc<Mutex<StreamWidget>>;

/// Cap on rendered diff rows for a single Write/Edit tool result so a large
/// patch doesn't bury the conversation. These `DiffLine`s are view-only (never
/// sent to the model), so the cap is purely cosmetic.
const MAX_DIFF_ROWS: usize = 40;

/// Process-wide extended-thinking budget (in tokens) seeded from the startup
/// `--thinking-tokens` flag. `None` (the default) ⇒ no thinking budget on any
/// `PromptRequest`, keeping the provider wire byte-identical.
///
/// The session reasoning-effort token lives on `App` (in `tui.rs`), but the
/// thinking budget is a scalar set once at startup and never mutated
/// mid-session, so a `OnceLock`-backed process global is the simplest home that
/// the TUI prompt path (`call_daemon`) can read without threading a new field
/// through `App`. `set_thinking_tokens_seed` is called exactly once during
/// startup before any prompt is driven.
static THINKING_TOKENS_SEED: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();

/// Record the startup `--thinking-tokens` seed. Idempotent: only the first call
/// wins (later calls are no-ops), matching the once-at-startup contract.
fn set_thinking_tokens_seed(value: Option<u32>) {
    let _ = THINKING_TOKENS_SEED.set(value);
}

/// Read the startup `--thinking-tokens` seed; `None` until set, and `None`
/// thereafter unless a positive budget was provided.
fn thinking_tokens_seed() -> Option<u32> {
    THINKING_TOKENS_SEED.get().copied().flatten()
}

/// Process-wide active account for the interactive session, set by the
/// `/account` composer command and stamped onto every [`PromptRequest`].
///
/// Unlike the thinking-budget seed this is MUTATED mid-session, so it is a
/// `Mutex` rather than a `OnceLock`. It lives here (not on `App` or threaded
/// through `call_daemon`) because the CLI opens a fresh daemon connection per
/// prompt: the account cannot live on a connection, and the daemon's
/// per-connection slot is therefore never set on the prompt path. Stamping the
/// account on each request is what makes `/account` isolation actually reach the
/// daemon's cross-provider rebuild. `None` (the default) ⇒ no override, wire
/// byte-identical.
static SESSION_ACCOUNT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Record the session's active account (called by the `/account` handler).
fn set_session_account(account: Option<String>) {
    if let Ok(mut g) = SESSION_ACCOUNT.lock() {
        *g = account;
    }
}

/// Read the session's active account for stamping onto a `PromptRequest`.
fn session_account() -> Option<String> {
    SESSION_ACCOUNT.lock().ok().and_then(|g| g.clone())
}

/// Stack size for the thread that drives the async entrypoint.
///
/// The TUI's top-level future is a single large state machine — many
/// un-boxed nested async fns inlined into one (updater→reqwest, daemon
/// auto-spawn, and the event loop's prompt-turn → `call_daemon` chain).
/// `block_on` materializes that whole future on the stack *before* polling
/// it, and in a debug build it exceeds Windows' default 1 MiB main-thread
/// stack — overflowing before `main` does any work (`STATUS_STACK_OVERFLOW`,
/// `0xC000_00FD`), even for `--version`. Linux's 8 MiB default main stack hides
/// this, so it only bit Windows. We drive the runtime on a dedicated thread
/// with a generous stack (2× Linux's default) so every platform behaves
/// identically — the same reason `origin-daemon` hand-rolls its entrypoint
/// instead of using `#[tokio::main]`.
const RUNTIME_STACK_SIZE: usize = 16 * 1024 * 1024;

fn main() -> Result<()> {
    let worker = std::thread::Builder::new()
        .name("origin-rt".to_string())
        .stack_size(RUNTIME_STACK_SIZE)
        .spawn(|| {
            // `flavor = "current_thread"` + `enable_all()` reproduces the
            // exact runtime the `#[tokio::main(flavor = "current_thread")]`
            // attribute built before — net + time drivers on, single thread.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow::anyhow!("build tokio runtime: {e}"))?;
            rt.block_on(run())
        })
        .map_err(|e| anyhow::anyhow!("spawn runtime thread: {e}"))?;
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("runtime thread panicked"))?
}

/// Synchronous auto-update step run before any subcommand dispatch. The flow is:
///   1. Swap in any binary staged from a prior run (rename `.new` over exe).
///   2. For npm-installed binaries only (dev/source builds are skipped), check
///      the npm registry for a newer published version. If newer, download the
///      matching GitHub-release asset, verify it against `SHA256SUMS` (sha256,
///      no `cosign`), and stage as `<exe>.new` BEFORE proceeding.
///   3. If we just staged a new binary, swap it in and re-exec with the
///      same argv so the user's command runs on the new code path.
///
/// Failures along the way fall through to running the current binary. A
/// successful re-exec calls `std::process::exit` and never returns.
async fn run_self_update() -> Result<()> {
    match origin_cli::updater::apply_staged_if_present() {
        Ok(true) => eprintln!("Applied staged update from previous run."),
        Ok(false) => {}
        Err(e) => tracing::warn!("updater: apply_staged_if_present failed: {e}"),
    }

    match origin_cli::updater::check_and_stage_blocking().await {
        Ok(true) => {
            // We just staged a new binary. Swap it in and re-exec.
            if matches!(origin_cli::updater::apply_staged_if_present(), Ok(true)) {
                let exe = std::env::current_exe()?;
                let args: Vec<String> = std::env::args().skip(1).collect();
                eprintln!("Update staged; relaunching…");
                let status = std::process::Command::new(&exe)
                    .args(&args)
                    .status()
                    .map_err(|e| anyhow::anyhow!("relaunch failed: {e}"))?;
                std::process::exit(status.code().unwrap_or(0));
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!("updater: check_and_stage_blocking failed: {e}"),
    }
    Ok(())
}

/// Dispatch a top-level subcommand. Returns `Some(result)` for every
/// subcommand (each terminates the program with its own result), mirroring
/// the `return` arms this replaced. The TUI entry path is reached when
/// `Cli::cmd` is `None`, so this is only called with a concrete `Cmd`.
#[allow(clippy::too_many_lines)] // Single linear dispatch over every subcommand; splitting hurts readability.
async fn dispatch_subcommand(cmd: Cmd) -> Option<Result<()>> {
    Some(match cmd {
        Cmd::Trace {
            sub: TraceSub::Query(q),
        } => origin_cli::trace_cmd::invoke(q).map_err(|e| anyhow::anyhow!("{e}")),
        Cmd::Pair { sub } => match sub {
            PairSub::Start { ttl_secs } => pair_start(ttl_secs).await,
            PairSub::Redeem { url, code, device_id } => pair_redeem(&url, &code, device_id).await,
        },
        Cmd::Run {
            text,
            json,
            remote,
            bearer,
            model,
            effort,
            thinking_tokens,
            alias,
            attach,
            output_format,
            json_schema,
            root,
        } => {
            // Run-level `--thinking-tokens` wins; otherwise inherit the startup
            // seed. `0` is a hard error (matches the global flag's contract).
            let thinking_tokens = match origin_cli::config::validate_thinking_tokens(
                thinking_tokens.or_else(thinking_tokens_seed),
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(anyhow::anyhow!("{e}"))),
            };
            origin_cli::headless::run(origin_cli::headless::RunArgs {
                text,
                json,
                remote,
                bearer,
                model,
                effort,
                thinking_tokens,
                aliases: alias,
                attach,
                output_format,
                json_schema,
                roots: root,
            })
            .await
        }
        Cmd::OidcExchange {
            token_url,
            subject_token,
            audience,
            workspace_id,
            federation_rule_id,
            json,
        } => {
            origin_cli::oidc::run(origin_cli::oidc::OidcArgs {
                token_url,
                subject_token,
                audience,
                workspace_id,
                federation_rule_id,
                json,
            })
            .await
        }
        Cmd::Usage => origin_cli::admin::usage().await,
        Cmd::Insights => origin_cli::insights::run().await,
        Cmd::Sessions { sub } => origin_cli::admin::sessions(sub_to_action(sub)).await,
        Cmd::Keyring { sub } => {
            // Login drives an interactive OAuth flow and must be handled
            // before converting to KeyringAction (which doesn't have a Login
            // variant — Login bypasses the daemon IPC path entirely).
            if let KeyringSub::Login { provider, account } = sub {
                origin_cli::keyring_login::run(&provider, &account).await
            } else {
                origin_cli::admin::keyring(sub_to_action_kr(sub)).await
            }
        }
        Cmd::Providers { sub } => match sub {
            ProvidersSub::Ls => {
                origin_cli::providers::ls();
                Ok(())
            }
            ProvidersSub::Describe { id } => {
                origin_cli::providers::describe(&id);
                Ok(())
            }
            ProvidersSub::Refresh { provider } => {
                origin_cli::providers::refresh(provider.as_deref());
                Ok(())
            }
            ProvidersSub::Recommend { models, write } => origin_cli::recommend::run(&models, write),
        },
        Cmd::Init => origin_cli::init::run().await,
        Cmd::Import(a) => import_subcommand(&a),
        Cmd::ResumeForeign { source, path } => origin_cli::resume_foreign::run(source, path).await,
        Cmd::Doctor { json, privacy } => origin_cli::doctor::run(json, privacy).await,
        Cmd::Mermaid { path } => origin_cli::mermaid::run(&path),
        Cmd::Knowledge { sub } => origin_cli::knowledge::run(sub),
        Cmd::Schedule { sub } => origin_cli::schedule::run(sub),
        Cmd::Export {
            session_id,
            json,
            out,
        } => origin_cli::admin::export_session(session_id, json, out).await,
        Cmd::Checkpoint { label } => origin_cli::vcs::checkpoint(label),
        Cmd::Checkpoints => origin_cli::vcs::checkpoints(),
        Cmd::Rewind { id, files_only, path } => origin_cli::vcs::rewind(&id, files_only, path),
        Cmd::CheckpointDiff { id } => origin_cli::vcs::checkpoint_diff(&id),
        Cmd::Memory { sub } => origin_cli::memory_inbox::run_memory(sub),
        Cmd::Scout { repo_url, cache } => origin_cli::scout::run(&repo_url, cache),
        Cmd::Watch { root, ext } => origin_cli::watch::run(root, ext),
        Cmd::CopyContext { instruction, files } => origin_cli::clipboard::copy_context(instruction, &files),
        Cmd::ApplyClipboard => origin_cli::clipboard::apply_clipboard(),
        Cmd::Dictate {
            interleave,
            lang,
            device,
        } => origin_cli::voice::run(interleave, lang, device),
        Cmd::Search { query, engine } => origin_cli::search::run(&query, engine).await,
        Cmd::Plugin { sub } => origin_cli::plugin::run(sub),
        Cmd::Lsp { sub } => origin_cli::lsp::run(&sub),
        Cmd::Ambient { sub } => origin_cli::ambient::run(&sub),
        Cmd::Bench {
            samples,
            json,
            from,
            leaderboard,
        } => {
            if leaderboard {
                origin_cli::bench::run_leaderboard(&from, json, samples)
            } else {
                // Single-file reliability path (unchanged): the first --from
                // selects the recorded results; None runs the task set live.
                origin_cli::bench::run(samples, json, from.into_iter().next())
            }
        }
        Cmd::Review { strictness, llm } => origin_cli::review::run(&strictness, llm).await,
        Cmd::Gmail {
            op,
            query,
            id,
            max,
            include_body,
            client_id,
            client_secret,
            port,
        } => {
            origin_cli::gaps_cmds::gmail(op, query, id, max, include_body, client_id, client_secret, port)
                .await
        }
        Cmd::Workflow { sub } => origin_cli::gaps_cmds::workflow(sub).await,
        Cmd::Selfdev { sub } => origin_cli::gaps_cmds::selfdev(sub).await,
        Cmd::Team { sub } => origin_cli::gaps_cmds::team(sub).await,
    })
}

/// Handle `origin import`: run the import and print a JSON or human summary.
fn import_subcommand(a: &origin_cli::import::ImportArgs) -> Result<()> {
    let r = origin_cli::import::run_import(a).map_err(anyhow::Error::from)?;
    if a.json {
        println!(
            "{}",
            serde_json::json!({
                "sessions_inserted": r.sessions_inserted,
                "skills_inserted": r.skills_inserted,
            })
        );
    } else {
        println!(
            "Imported {} sessions, {} skills.",
            r.sessions_inserted, r.skills_inserted
        );
    }
    Ok(())
}

/// DECAWM (terminal autowrap) off / on, as raw VT sequences.
///
/// The TUI renderer absolute-positions every cell with a cursor-position (CUP)
/// escape and never relies on the terminal wrapping at the right margin. With
/// autowrap **on**, emitting the screen's bottom-right cell makes an
/// immediate-wrap terminal (legacy Windows conhost) scroll the alternate screen
/// up by one line — which shifts the physical screen out from under the
/// damage-diff shadow grid (`Composer::scratch_*`) and leaves stale, "disjointed"
/// fragments and un-cleared highlights. We disable autowrap for the session and
/// restore it on teardown. Deferred-wrap terminals (modern Windows Terminal,
/// xterm) are unaffected either way, so this is safe everywhere.
///
/// See `crates/origin-tui/tests/autowrap_desync.rs` for the reproduction.
const DISABLE_AUTOWRAP: &[u8] = b"\x1b[?7l";
const ENABLE_AUTOWRAP: &[u8] = b"\x1b[?7h";

/// Emit the DECAWM-off sequence. Split out (over a generic writer) so it can be
/// unit-tested without driving a real terminal.
fn disable_autowrap(w: &mut impl std::io::Write) -> std::io::Result<()> {
    w.write_all(DISABLE_AUTOWRAP)
}

/// Emit the DECAWM-on sequence — restores the mode `disable_autowrap` cleared.
fn enable_autowrap(w: &mut impl std::io::Write) -> std::io::Result<()> {
    w.write_all(ENABLE_AUTOWRAP)
}

/// Write `bytes` to the terminal as one atomic unit: hold the stdout lock across
/// the write *and* the flush so frame bytes and out-of-band escapes (OSC 52
/// clipboard, mode toggles) emitted from different tasks can never interleave.
fn emit_to_terminal(bytes: &[u8]) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Copy `text` to the clipboard two ways: an OSC 52 escape (works over SSH and
/// remote terminals) and the local OS clipboard (reliable on the host). Both are
/// best-effort; the OS write is offloaded to a blocking task so it never stalls
/// the input loop.
fn copy_text_to_clipboard(text: String) {
    emit_to_terminal(origin_cli::clipboard::osc52_sequence(&text).as_bytes());
    tokio::task::spawn_blocking(move || {
        let _ = origin_cli::clipboard::copy_to_os_clipboard(&text);
    });
}

/// Best-effort restore of the terminal to its pre-TUI state. Idempotent and
/// error-swallowing so it is safe to call from a panic hook, a `Drop` guard, and
/// the normal exit path. Reverses the setup in [`run`] (raw mode + alternate
/// screen + mouse capture + hidden cursor + autowrap).
fn restore_terminal() {
    let _ = disable_raw_mode();
    // Re-enable autowrap (disabled on entry) before leaving the alt screen so the
    // normal buffer behaves as the shell expects.
    let _ = enable_autowrap(&mut std::io::stdout());
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}

/// RAII guard that restores the terminal on drop — covering early `?` returns
/// and unwinding panics, which the linear teardown at the end of [`run`] would
/// otherwise skip (leaving the shell in raw mode / alt screen / mouse capture,
/// forcing the user to blindly type `reset`).
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

#[allow(clippy::too_many_lines)] // linear startup wiring; splitting hurts readability
async fn run() -> Result<()> {
    run_self_update().await?;

    // Dispatch a subcommand if one was given, otherwise fall through to the
    // TUI entry path (preserves the existing env-driven invocation).
    let cli = Cli::parse();
    // Record the optional UI-locale override (`--lang <code>`) before any chrome
    // renders, so localized strings (welcome/bye/...) pick it up via the
    // process-global override that `locale::resolve(None)` consults first.
    // Default-off: when `--lang` is unset, nothing is stored and chrome resolves
    // exactly as before (env locale, else English).
    if let Some(code) = cli.lang.as_deref() {
        origin_cli::locale::set_locale_override(code);
    }
    // Resolve the optional reasoning-effort flag (item H). Default-off: when
    // `--effort` is unset this is `None` and nothing about the wire changes.
    // A valid level becomes the session's starting effort token (seeded onto
    // the App below and carried on every PromptRequest); `/effort`/`/fast`
    // mutate it mid-session. An unknown value is a non-fatal warning.
    let effort_seed: Option<String> = cli.effort.as_deref().and_then(|raw| {
        origin_cli::effort::ReasoningEffort::parse_level(raw).map_or_else(
            || {
                eprintln!("warning: unknown --effort level `{raw}` (ignored)");
                None
            },
            |level| Some(level.as_str().to_string()),
        )
    });
    // Resolve the optional extended-thinking budget (aider `--thinking-tokens`).
    // Default-off: unset ⇒ `None` ⇒ wire unchanged. `0` is a hard error (a zero
    // budget is meaningless and Anthropic rejects it). The validated value is
    // recorded process-wide and rides on every PromptRequest the TUI sends.
    let thinking_tokens_seed = origin_cli::config::validate_thinking_tokens(cli.thinking_tokens)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    set_thinking_tokens_seed(thinking_tokens_seed);
    if cli.tutorial {
        // Localized welcome chrome (item A; origin-i18n locale from
        // $LC_ALL/$LANG, English fallback).
        println!("{}", origin_cli::locale::line("welcome"));
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        origin_cli::tutorial::run(stdin.lock(), stdout.lock())?;
        return Ok(());
    }
    if let Some(cmd) = cli.cmd {
        if let Some(res) = dispatch_subcommand(cmd).await {
            return res;
        }
    }

    // First-run onboarding: if ~/.origin/config.toml does not exist, run the
    // interactive init flow before entering the TUI's raw-mode alt screen.
    // The flow only runs when no subcommand was given (the `None` branch
    // above), so explicit subcommands stay non-interactive. Setting
    // `ORIGIN_SKIP_INIT=1` bypasses for CI / scripted environments.
    if !origin_cli::config::exists() && env::var_os("ORIGIN_SKIP_INIT").is_none() {
        origin_cli::init::run().await?;
    }

    // Resolve TUI defaults from the saved config (falling back to env vars
    // and finally to hard-coded "anthropic" / "claude-fable-5" / "default"
    // so callers who declined / skipped onboarding still get a working
    // session). The provider/account pair is also forwarded to the daemon
    // when we auto-spawn it — the daemon itself only reads ORIGIN_PROVIDER /
    // ORIGIN_ACCOUNT, not config.toml, so we have to hand it the answer.
    let loaded_cfg = origin_cli::config::load().ok().flatten();
    let (default_provider, default_account, default_model) = loaded_cfg.as_ref().map_or_else(
        || {
            (
                "anthropic".to_string(),
                "default".to_string(),
                "claude-fable-5".to_string(),
            )
        },
        |c| {
            (
                c.primary.provider.clone(),
                c.primary.account.clone(),
                c.primary.model.clone(),
            )
        },
    );

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    // Resolve the model id against the config `[aliases]` table (aider `--alias`).
    // The substitution is the single CLI-side resolution point: an undefined
    // alias — or any literal model id — passes through unchanged, so the
    // pre-alias behaviour is byte-identical. Empty/absent table ⇒ no-op.
    let raw_model = env::var("ORIGIN_MODEL").unwrap_or(default_model);
    let mut model = loaded_cfg.as_ref().map_or_else(
        || raw_model.clone(),
        |c| origin_cli::config::resolve_alias(&c.aliases, &raw_model),
    );
    // `--resume <id>` reuses a prior session id; the daemon rehydrates that
    // session's transcript on the first prompt (see handle_request →
    // load_messages), so the model picks up where it left off. Default: fresh.
    let resuming = cli.resume.clone();
    let session_id = resuming
        .clone()
        .unwrap_or_else(|| format!("{:032x}", rand::random::<u128>()));

    // Quickstart docs promise auto-spawn: stand up `origin-daemon` as a
    // detached child if nothing is listening on the IPC path yet, and wait
    // for it to bind the pipe before we drop into the TUI's alt-screen.
    // (Doing this before `enable_raw_mode` keeps spawn errors readable.)
    ensure_daemon_running(&path, &default_provider, &default_account).await?;

    // Restore the terminal before the default panic handler prints, so a
    // backtrace lands on the normal screen instead of a corrupted raw-mode one.
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_panic(info);
    }));
    enable_raw_mode()?;
    // Guard restores the terminal on ANY scope exit (early `?`, panic, normal).
    let terminal_guard = TerminalGuard;
    // `Hide` the hardware cursor: the renderer paints its own caret, and without
    // this the real cursor teleports to the last damage run (the status line)
    // and jitters there on every 80 ms heartbeat tick.
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        Hide
    )?;
    // Disable autowrap so the renderer's bottom-right cell writes never scroll the
    // alternate screen (which would desync the damage-diff shadow grid and leave
    // stale fragments). Restored by `restore_terminal`. See `DISABLE_AUTOWRAP`.
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        disable_autowrap(&mut out)?;
        out.flush()?;
    }

    let (composer, widget, app) = setup_tui(default_provider, &model);
    // Seed the session reasoning-effort from the startup `--effort` flag.
    if effort_seed.is_some() {
        app.lock().effort = effort_seed;
    }
    // Seed extra workspace roots from the startup `--root` flags (cline multi-root).
    if !cli.root.is_empty() {
        app.lock().workspace_roots.clone_from(&cli.root);
    }
    // Note a resumed session so the empty scrollback doesn't look like a fresh
    // start — the daemon will rehydrate the transcript on the first prompt.
    if let Some(id) = &resuming {
        let short: String = id.chars().take(8).collect();
        app.lock().add_line(
            "system> ",
            &origin_cli::locale::linef("session.resumed", &[("short", &short)]),
        );
    }

    // First-run discovery: if `origin init`'s welcome flow queued a pending
    // prompt, fire it as the user's first turn and remove the file so it
    // never auto-fires twice. Errors are non-fatal — the user can always
    // type a prompt manually.
    let pending_prompt = origin_cli::first_run_prompt::path()
        .ok()
        .and_then(|p| origin_cli::first_run_prompt::drain(&p).ok().flatten());

    let plan_panel: Arc<Mutex<PlanPanelWiring>> = Arc::new(Mutex::new(PlanPanelWiring::new()));

    let scheduler = Scheduler::new(Duration::from_millis(6));
    let handle = scheduler.handle();
    handle.mark_dirty();

    // `composer`/`widget` are not used after the render task takes them, so
    // move them in directly; `app`/`plan_panel`/`handle` are still needed
    // below, so those are cloned.
    // Keep a composer handle for the event loop's resize arm; the render task
    // takes its own clone (it's an Arc<Mutex<…>>, so both share one composer).
    let composer_for_events = Arc::clone(&composer);
    let render_task = spawn_render_task(scheduler, composer, app.clone(), widget, plan_panel.clone());

    spawn_stall_watchdog(app.clone(), handle.clone());

    // Auto-fire the pending discovery prompt now that the TUI is wired up.
    fire_pending_prompt(pending_prompt, &app, &handle, &path, &mut model, &session_id).await;

    // Keep a handle to read the cumulative usage for the farewell summary after
    // the event loop consumes its own `app` handle.
    let app_for_summary = app.clone();
    let result = run_event_loop(
        app,
        handle,
        &path,
        &mut model,
        &session_id,
        plan_panel,
        composer_for_events,
    )
    .await;

    render_task.abort();
    // Restore now (before the farewell) so "bye" prints on the normal screen;
    // the guard's `Drop` afterward is then an idempotent no-op safety net.
    drop(terminal_guard);
    // Localized farewell on a clean TUI exit (item A; origin-i18n locale from
    // `--lang`/$LC_ALL/$LANG, English fallback). Only on the Ok path so error
    // output is unchanged. A priced session also prints a localized
    // "Session total: $X" line (the `cost.session` catalog key); unpriced /
    // zero-spend sessions print nothing extra.
    if result.is_ok() {
        // Compute before the `if let` so the usage lock guard is released
        // immediately (not held across the println! body).
        let total_line = origin_cli::status::session_total_line(&app_for_summary.lock().usage);
        if let Some(total) = total_line {
            println!("{total}");
        }
        println!("{}", origin_cli::locale::line("bye"));
    }
    result
}

/// Build the shared TUI state: the composer (full-screen grid), the stream
/// widget (main scrollback region), and the `App`. Reads the current terminal
/// size and pushes the startup banner. The caller is responsible for having
/// already entered raw mode / the alternate screen.
fn setup_tui(default_provider: String, model: &str) -> (SharedComposer, SharedWidget, SharedApp) {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let main_cols = cols.saturating_sub(20);
    let main_rows = rows.saturating_sub(3);

    let composer: SharedComposer = Arc::new(Mutex::new(Composer::new(cols, rows)));
    let widget: SharedWidget = Arc::new(Mutex::new(StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: main_cols,
        rows: main_rows,
    })));
    // `App::new` requires a `&'static str` for the provider label that the
    // status bar renders. Leaking the onboarded provider string is bounded
    // to a single allocation per process invocation, so it's the simplest
    // path to satisfying the lifetime without touching the wider App API.
    let provider_static: &'static str = Box::leak(default_provider.into_boxed_str());
    let sources = origin_cli::autocomplete::load_sources();
    let app: SharedApp = Arc::new(Mutex::new(App::new(provider_static, model.to_string(), sources)));
    {
        let mut a = app.lock();
        // Seed the opt-in vim layer from `ORIGIN_VIM=1` so the session can begin
        // in vim Normal mode; default-off ⇒ byte-identical direct insert.
        a.set_vim_active(origin_cli::input::vim_active_default());
        // Load the customizable composer keymap once (builtin defaults overlaid
        // with `~/.origin/keybindings.toml`). Absent file ⇒ builtin ⇒ unchanged.
        a.set_keymap(origin_cli::keybindings::KeyMap::load());
    }
    app.lock().push_banner(cols, rows);
    (composer, widget, app)
}

/// Spawn the coalescing render task. It owns `scheduler` and drives one draw
/// per dirty tick: composing the main grid + optional side panel into a frame
/// and flushing it to stdout. Returns the task handle so the caller can
/// `abort()` it during teardown.
fn spawn_render_task(
    scheduler: Scheduler,
    composer: SharedComposer,
    app: SharedApp,
    widget: SharedWidget,
    plan_panel: Arc<Mutex<PlanPanelWiring>>,
) -> tokio::task::JoinHandle<()> {
    spawn_in(TaskClass::Realtime, async move {
        scheduler
            .run(move || {
                let bytes = {
                    // Snapshot the plan first; the side panel is shown exactly
                    // when there is a plan to show (the panel was wired but
                    // `side_visible` stayed false forever, so it never appeared).
                    let lines = plan_panel.lock().render();
                    let pal = app.lock().palette();
                    let mut c = composer.lock();
                    let mut w = widget.lock();
                    // Reflows only on the hidden↔visible transition.
                    c.set_side_visible(!lines.is_empty());
                    app.lock().draw(&mut c, &mut w);
                    if c.side_visible() {
                        origin_cli::tui::draw_side(c.side_grid(), &lines, pal);
                    }
                    // Capture the rendered main pane for click-drag selection
                    // extraction — only while a selection is active, so there is
                    // no per-frame cost otherwise.
                    {
                        let mut a = app.lock();
                        if a.selection.is_some() {
                            a.screen_text = c.snapshot_main_text();
                        }
                    }
                    c.frame()
                };
                if !bytes.is_empty() {
                    emit_to_terminal(&bytes);
                }
            })
            .await;
    })
}

/// Spawn the render heartbeat + stall watchdog. While a turn is active this
/// ticks the spinner/elapsed clock independently of daemon events, so a hung
/// daemon never looks like a dead screen. It also watches a cheap activity
/// fingerprint: when it stops changing for `STALL_SOFT_AFTER`, the daemon is
/// quiet and we show a gentle "still working…" reassurance. The task runs for
/// the life of the process; the handle is intentionally dropped.
fn spawn_stall_watchdog(app: SharedApp, handle: Handle) {
    spawn_in(TaskClass::Realtime, async move {
        let mut last_sig: u64 = 0;
        let mut quiet_since: Option<std::time::Instant> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let mut a = app.lock();
            if !a.spinner.active {
                quiet_since = None;
                a.stall = None;
                continue;
            }
            let sig = a.activity_signature();
            if sig == last_sig {
                let since = *quiet_since.get_or_insert_with(std::time::Instant::now);
                a.stall = origin_cli::tui::stall_tier(since.elapsed(), origin_cli::tui::STALL_SOFT_AFTER);
            } else {
                last_sig = sig;
                quiet_since = Some(std::time::Instant::now());
                a.stall = None;
            }
            drop(a);
            handle.mark_dirty();
        }
    });
}

/// Auto-fire the queued first-run discovery prompt, if any, now that the TUI
/// is wired up. A `None` prompt is a no-op.
async fn fire_pending_prompt(
    pending_prompt: Option<String>,
    app: &SharedApp,
    handle: &Handle,
    path: &str,
    model: &mut String,
    session_id: &str,
) {
    let Some(text) = pending_prompt else {
        return;
    };
    {
        let mut a = app.lock();
        a.add_line("system> ", "Running queued first-run discovery prompt\u{2026}");
        // Activate the spinner so the render heartbeat animates and the
        // stall watchdog arms for this turn too — without this the
        // first-run prompt ran with a frozen, un-animated status line.
        a.spinner.start();
    }
    handle.mark_dirty();
    // No user interrupt channel for the auto-fire path — the user has
    // not had a chance to press Ctrl+C yet (TUI is not yet driving the
    // input loop). `None` keeps `call_daemon`'s select arm a no-op.
    handle_submit(app, handle, path, model, &text, session_id, None).await;
    app.lock().spinner.stop();
    handle.mark_dirty();
}

async fn run_event_loop(
    app: SharedApp,
    handle: Handle,
    path: &str,
    model: &mut String,
    session_id: &str,
    plan_panel: Arc<Mutex<PlanPanelWiring>>,
    composer: SharedComposer,
) -> Result<()> {
    spawn_plan_subscription(path.to_string(), Arc::clone(&plan_panel), handle.clone());
    // Bug #5: shared slot holding the current `call_daemon`'s interrupt
    // sender. `Some(tx)` while a Prompt is in flight; `None` between
    // prompts. Ctrl+C in the input loop drops a `()` into `tx` and the
    // `call_daemon` `tokio::select!` writes `ClientMessage::Interrupt` to
    // the daemon over the SAME connection serving the current prompt
    // (required — the daemon's drive-goal-loop peek is per-connection).
    let interrupt_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let mut input_stream = crossterm::event::EventStream::new();
    while let Some(maybe_ev) = input_stream.next().await {
        let event = maybe_ev?;
        // Terminal resize: reflow the composer's grids to the new size. Without
        // this the grid stays frozen at launch size and scribbles past the new
        // bounds. The draw routine re-wraps scrollback to the new width and
        // clamps scroll on the next frame.
        if let crossterm::event::Event::Resize(cols, rows) = event {
            let mut c = composer.lock();
            let side_visible = c.side_visible();
            c.resize(cols, rows, side_visible);
            drop(c);
            handle.mark_dirty();
            continue;
        }
        // Mouse: wheel ticks drive the scrollback offset (~3 visual rows each);
        // left-button press/drag/release drives in-app text selection, which is
        // auto-copied (OS clipboard + OSC 52) on release. These selection events
        // only arrive while mouse capture is on (the default); with `/mouse off`
        // the terminal handles native selection instead.
        if let crossterm::event::Event::Mouse(me) = &event {
            use crossterm::event::MouseButton;
            match me.kind {
                MouseEventKind::ScrollUp => {
                    app.lock().scroll_up(3);
                    handle.mark_dirty();
                }
                MouseEventKind::ScrollDown => {
                    app.lock().scroll_down(3);
                    handle.mark_dirty();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    app.lock().begin_selection(me.row, me.column);
                    handle.mark_dirty();
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    app.lock().update_selection(me.row, me.column);
                    handle.mark_dirty();
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    // Extract from the last captured frame and copy; keep the
                    // highlight visible as the "copied" feedback.
                    let text = app.lock().selection_text();
                    if let Some(text) = text.filter(|t| !t.is_empty()) {
                        copy_text_to_clipboard(text);
                    }
                    handle.mark_dirty();
                }
                _ => {}
            }
            continue;
        }
        // Bracketed paste: the terminal delivers the whole clipboard as one
        // event, so a multi-line paste no longer streams key-by-key (where the
        // first embedded newline would fire a truncated Submit). Append it
        // verbatim (normalizing CR/CRLF to LF) in one shot, then one recompute.
        if let crossterm::event::Event::Paste(pasted) = &event {
            let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
            let mut a = app.lock();
            a.input.insert_str(&normalized);
            a.recompute_suggestions();
            drop(a);
            handle.mark_dirty();
            continue;
        }
        if let crossterm::event::Event::Key(ev) = event {
            // Any keystroke dismisses an active selection/highlight (the user has
            // moved on from selecting).
            if app.lock().clear_selection() {
                handle.mark_dirty();
            }
            match handle_key_event(ev, &app, &handle, &interrupt_tx, path, model, session_id).await {
                KeyOutcome::Continue => {}
                KeyOutcome::Break => break,
            }
        }
    }
    Ok(())
}

/// Outcome of handling a single key event: either keep polling the input
/// stream or break out of the event loop (process exit path).
enum KeyOutcome {
    Continue,
    Break,
}

/// Handle one decoded key event. Returns [`KeyOutcome::Break`] only for the
/// quit path; every other branch returns [`KeyOutcome::Continue`], matching
/// the `continue`/fall-through behaviour of the original inline `match`.
/// The input card's text width, derived from the current terminal size — used
/// by the editor reducer for visual Home/End and Up/Down across wrapped lines.
/// Mirrors the card geometry in `App::draw` (full-width card: `card_w = cols-2`,
/// with a 2-column inner indent for the accent rule + padding, so the text
/// width is `card_w - 2 == cols - 4`).
fn input_text_width() -> usize {
    let cols = crossterm::terminal::size().map_or(80, |(c, _)| c);
    let card_w = cols.saturating_sub(2);
    usize::from(card_w.saturating_sub(2))
}

async fn handle_key_event(
    ev: crossterm::event::KeyEvent,
    app: &SharedApp,
    handle: &Handle,
    interrupt_tx: &Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>>,
    path: &str,
    model: &mut String,
    session_id: &str,
) -> KeyOutcome {
    // crossterm on Windows reports both Press and Release for every
    // keystroke; without this filter, every character would land in
    // the buffer twice. Allow Repeat so autorepeat still works.
    if !matches!(
        ev.kind,
        crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
    ) {
        return KeyOutcome::Continue;
    }
    // claude-code L147: route the raw event through the session keymap so a
    // user `~/.origin/keybindings.toml` rebind reaches the existing handlers.
    // `canonicalize` rewrites a user-bound chord to the builtin event the
    // legacy reducer/scrollback path already understands; with the builtin
    // (default) map every event maps to itself, so the key path is
    // byte-identical when no override file is present.
    let ev = app.lock().keymap().canonicalize(ev);
    // Scrollback navigation — intercept before the buffer reducer. Returns
    // `Some` when the key was a navigation key we fully handled.
    if let Some(outcome) = handle_scrollback_key(ev, app, handle) {
        return outcome;
    }

    // Pending permission ask (opt-in `/permissions`): y/n/Esc answers it. The
    // decision is sent on a FRESH connection so the daemon's registry can
    // resolve it (the turn's connection is busy streaming). Other keys fall
    // through to normal handling; a no-op when nothing is pending.
    if app.lock().pending_permission.is_some() {
        if let Some(allow) = permission_answer(ev.code) {
            let pending = app.lock().pending_permission.take();
            if let Some(p) = pending {
                let _ = send_decision(path, &ClientMessage::PermissionDecision { id: p.id, allow }).await;
                // The deny verb routes through the `permission.denied` catalog key
                // (En "denied" — byte-identical); the allow verb has no key and
                // stays in code. The `<tool> <args>` suffix is unchanged.
                let verb = if allow {
                    "allowed".to_string()
                } else {
                    origin_cli::locale::line("permission.denied").to_string()
                };
                app.lock()
                    .add_line("system> ", &format!("{verb}: {} {}", p.tool, p.args));
            }
            handle.mark_dirty();
            return KeyOutcome::Continue;
        }
    }

    // claude-code L147 keymap `Clear` action: the builtin chord is `Ctrl+U`,
    // which the legacy reducer leaves as a no-op. Wiring it makes a *rebind* of
    // `clear` actually wipe the composer buffer. Gated on `is_overridden` so the
    // pure builtin map's Ctrl+U stays the historic no-op (byte-identical
    // default); only a loaded `keybindings.toml` activates this additive action.
    {
        let is_clear = app.lock().keymap().is_overridden()
            && origin_cli::keybindings::KeyMap::builtin_event(origin_cli::keybindings::Action::Clear)
                .is_some_and(|c| c.code == ev.code && c.modifiers == ev.modifiers);
        if is_clear {
            let mut a = app.lock();
            if !a.input.is_empty() {
                a.input.set_buffer(String::new());
                a.recompute_suggestions();
                drop(a);
                handle.mark_dirty();
            }
            return KeyOutcome::Continue;
        }
    }

    if matches!(ev.code, crossterm::event::KeyCode::Tab) {
        let mut a = app.lock();
        if !a.suggestions.candidates.is_empty() {
            let suggestions = a.suggestions.clone();
            // accept_selected works on a String; round-trip through the editor
            // (set_buffer places the cursor at the end of the accepted text).
            let mut buf = a.input.buffer().to_string();
            origin_cli::suggestions::accept_selected(&suggestions, &mut buf);
            a.input.set_buffer(buf);
            a.recompute_suggestions();
        }
        drop(a);
        handle.mark_dirty();
        return KeyOutcome::Continue;
    }

    // aider L107 opt-in vim layer: when active, route the key through the pure
    // `vim_key` reducer → `App::apply_vim_action` instead of straight to
    // `reduce_editor`. A consumed key (mode switch / motion) marks the frame
    // dirty and stops here. A `Pass` falls through to the normal reducer in
    // Insert mode (so typing is byte-identical) and is dropped in Normal mode —
    // except modifier chords (Ctrl/Alt), which always fall through so the global
    // Ctrl+C/Ctrl+D exits keep working. Default sessions (`vim_active == false`)
    // skip this block entirely ⇒ byte-identical key handling.
    if app.lock().vim_active() {
        use crossterm::event::KeyModifiers;
        let mode = app.lock().vim_mode();
        let vim_action = origin_cli::input::vim_key(mode, ev);
        let consumed = app.lock().apply_vim_action(vim_action);
        if consumed {
            app.lock().recompute_suggestions();
            handle.mark_dirty();
            return KeyOutcome::Continue;
        }
        // Not a vim binding (`Pass`). In Normal mode, drop non-chord keys (vim
        // Normal never inserts text); chords fall through to the exit reducer.
        let is_chord = ev.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if app.lock().vim_mode() == origin_cli::input::VimMode::Normal && !is_chord {
            return KeyOutcome::Continue;
        }
    }

    let action = {
        let mut a = app.lock();
        // Bug #5: an operation is "in flight" when either the
        // status-line spinner is active (a Prompt is mid-stream)
        // or a goal indicator is visible. Either case means
        // Ctrl+C should send Interrupt instead of quitting.
        let op_in_flight = a.spinner.active || a.goal_status.is_some();
        reduce_editor(&mut a.input, ev, op_in_flight, input_text_width())
    };
    handle_input_action(action, app, handle, interrupt_tx, path, model, session_id).await
}

/// Intercept scrollback/suggestion navigation keys before the buffer reducer.
/// Returns `Some(KeyOutcome::Continue)` when the key was fully handled here;
/// `None` when it should fall through to the input reducer (an unhandled key,
/// or an unshifted Up/Down with no open suggestion popup).
fn handle_scrollback_key(
    ev: crossterm::event::KeyEvent,
    app: &SharedApp,
    handle: &Handle,
) -> Option<KeyOutcome> {
    match ev.code {
        crossterm::event::KeyCode::PageUp => {
            app.lock().scroll_up(10);
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        crossterm::event::KeyCode::PageDown => {
            app.lock().scroll_down(10);
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        crossterm::event::KeyCode::Up if ev.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) => {
            app.lock().scroll_up(3);
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        crossterm::event::KeyCode::Down if ev.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) => {
            app.lock().scroll_down(3);
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        crossterm::event::KeyCode::End => {
            app.lock().scroll_to_bottom();
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        // Unshifted Up/Down navigate the suggestion popup when it
        // is open. With no popup these keys are no-ops (history
        // navigation isn't implemented yet); the SHIFT variants
        // above still drive scrollback.
        crossterm::event::KeyCode::Up => {
            let mut a = app.lock();
            if a.suggestions.candidates.is_empty() {
                return None;
            }
            origin_cli::suggestions::select_prev(&mut a.suggestions);
            drop(a);
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        crossterm::event::KeyCode::Down => {
            let mut a = app.lock();
            if a.suggestions.candidates.is_empty() {
                return None;
            }
            origin_cli::suggestions::select_next(&mut a.suggestions);
            drop(a);
            handle.mark_dirty();
            Some(KeyOutcome::Continue)
        }
        _ => None,
    }
}

/// Apply a reduced [`InputAction`] to the TUI. Returns [`KeyOutcome::Break`]
/// for `Quit`; all other actions return [`KeyOutcome::Continue`].
async fn handle_input_action(
    action: InputAction,
    app: &SharedApp,
    handle: &Handle,
    interrupt_tx: &Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>>,
    path: &str,
    model: &mut String,
    session_id: &str,
) -> KeyOutcome {
    match action {
        InputAction::Quit => return KeyOutcome::Break,
        InputAction::Interrupt => {
            // Best-effort: drop a token into the current call_daemon's
            // interrupt channel. If no Prompt is in flight the slot is
            // `None` and the keystroke is a no-op (the reducer should not
            // even have produced this variant in that case, but we guard
            // anyway). Clone the sender out of the guard in a tight scope
            // so the lock is dropped before `send()` rather than held
            // across the await-free send (significant_drop_in_scrutinee).
            let tx = interrupt_tx.lock().await.clone();
            if let Some(tx) = tx {
                let _ = tx.send(());
            }
            app.lock()
                .add_line("system> ", origin_cli::locale::line("interrupt"));
            handle.mark_dirty();
        }
        InputAction::Submit(text) => {
            if is_slash_command(&text) {
                // Slash commands are fast (local, or a single one-shot IPC
                // round-trip) and may mutate `model`; run them inline.
                handle_submit(app, handle, path, model, &text, session_id, None).await;
                app.lock().recompute_suggestions();
                handle.mark_dirty();
            } else {
                // Hold the interrupt-slot lock across the busy check AND the
                // queue push: the turn-end drain in `spawn_prompt_turn` pops
                // the queue under this same lock before releasing the slot,
                // so a message queued here can never slip between "turn
                // ended" and "queue checked" and silently go stale.
                let slot = interrupt_tx.lock().await;
                if slot.is_some() {
                    // A prompt turn is already streaming on this connection;
                    // queue the message for auto-submission when it ends.
                    // Up on the top-most input line edits queued messages.
                    let pending = {
                        let mut a = app.lock();
                        a.input.queue_message(&text);
                        a.recompute_suggestions();
                        a.input.queued_len()
                    };
                    drop(slot);
                    app.lock().add_line(
                        "system> ",
                        &origin_cli::locale::linef(
                            "cmd.queue.added",
                            &[("pending", pending.to_string().as_str())],
                        ),
                    );
                    handle.mark_dirty();
                } else {
                    drop(slot);
                    spawn_prompt_turn(text, app, handle, interrupt_tx, path, model, session_id).await;
                }
            }
        }
        InputAction::QueueEdited => {
            let pending = app.lock().input.queued_len();
            app.lock().add_line(
                "system> ",
                &origin_cli::locale::linef("cmd.queue.edited", &[("pending", pending.to_string().as_str())]),
            );
            handle.mark_dirty();
        }
        InputAction::Insert(_) | InputAction::Backspace | InputAction::Newline => {
            app.lock().recompute_suggestions();
            handle.mark_dirty();
        }
        InputAction::Noop => {
            handle.mark_dirty();
        }
    }
    KeyOutcome::Continue
}

/// Start a streaming prompt turn on its own task so the event loop keeps
/// polling input and can deliver a Ctrl+C interrupt while the turn is live.
/// Installs the interrupt sender into `interrupt_tx` and clears it (plus the
/// spinner) when the turn completes.
/// Commit a user prompt to the view synchronously: echo the `you>` line, open
/// the assistant buffer, and start the turn timer. Called by both prompt paths
/// BEFORE the (possibly spawned) turn runs, so the very next 6 ms frame shows
/// the committed prompt + spinner + live timer instead of an empty card — the
/// "did it take my input?" dead window after pressing Enter.
fn begin_prompt_turn(app: &SharedApp, text: &str) {
    let mut a = app.lock();
    a.add_line("you> ", text);
    a.start_assistant_turn();
    a.start_turn_timer();
}

async fn spawn_prompt_turn(
    text: String,
    app: &SharedApp,
    handle: &Handle,
    interrupt_tx: &Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>>,
    path: &str,
    model: &str,
    session_id: &str,
) {
    // A prompt turn can stream for a long time (agentic goal loops back off
    // up to 60s per iteration). Spawn it so the event loop keeps polling
    // input and can deliver a Ctrl+C Interrupt into `interrupt_tx` while the
    // turn is live — awaiting inline (the old behaviour) blocked the loop and
    // made Ctrl+C dead until the turn ended.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    *interrupt_tx.lock().await = Some(tx);
    {
        let mut a = app.lock();
        a.recompute_suggestions();
        a.spinner.start();
    }
    // Commit the prompt synchronously so the first frame after Enter shows it,
    // independent of the spawn/connect hop below.
    begin_prompt_turn(app, &text);
    handle.mark_dirty();
    let app_for_turn = Arc::clone(app);
    let handle_for_turn = handle.clone();
    let interrupt_for_turn = Arc::clone(interrupt_tx);
    let path_for_turn = path.to_string();
    let model_for_turn = model.to_string();
    let session_for_turn = session_id.to_string();
    spawn_in(TaskClass::Realtime, async move {
        let mut text = text;
        let mut rx = Some(rx);
        loop {
            // IMPORTANT — queue-drain boundary semantics. `handle_prompt_turn`
            // resolves only when the daemon writes the terminal Response
            // (`PromptReply`) frame for this prompt, and the daemon writes
            // that only after `drive_goal_loop` → `run_loop` has fully
            // finished: every agentic model↔tool iteration of the task, and,
            // when a `/goal` is active, every goal-driver iteration through
            // verification. A queued message therefore NEVER lands mid-task —
            // it starts a brand-new prompt exactly where a hand-typed
            // follow-up would, after the daemon considers the whole task
            // complete. (Tool calls within a task are daemon-internal; the
            // CLI has no frame on which it could inject input mid-task even
            // if it wanted to.)
            handle_prompt_turn(
                &app_for_turn,
                &handle_for_turn,
                &path_for_turn,
                &model_for_turn,
                &text,
                &session_for_turn,
                rx.take(),
            )
            .await;
            // Drain the next queued message, if any, and chain straight into
            // another turn. The pop happens under the interrupt-slot lock —
            // the same lock the submit path holds while deciding queue-vs-
            // spawn — so a message submitted during this window can never
            // miss both the running turn and the drain. A fresh interrupt
            // channel is installed per chained turn so Ctrl+C keeps working.
            let mut slot = interrupt_for_turn.lock().await;
            // Bind the pop result BEFORE branching: a `match` scrutinee's
            // temporaries live to the end of the whole match, so matching on
            // `app.lock().input.pop_queued()` directly kept the App guard
            // alive across both arms — and the re-locks below self-deadlocked
            // the single-threaded runtime (parking_lot is not reentrant),
            // freezing the entire TUI at the end of every turn.
            let queued = app_for_turn.lock().input.pop_queued();
            if let Some(next) = queued {
                let (tx, new_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
                *slot = Some(tx);
                drop(slot);
                rx = Some(new_rx);
                text = next;
                begin_prompt_turn(&app_for_turn, &text);
                handle_for_turn.mark_dirty();
            } else {
                *slot = None;
                drop(slot);
                app_for_turn.lock().spinner.stop();
                handle_for_turn.mark_dirty();
                break;
            }
        }
    });
}

/// Open a dedicated long-lived IPC connection, send
/// [`ClientMessage::SubscribePlan`], and feed every received
/// [`StreamEvent::PlanOp`] into `wiring.ingest`. The task exits when the
/// daemon closes the connection.
fn spawn_plan_subscription(path: String, wiring: Arc<Mutex<PlanPanelWiring>>, render: Handle) {
    spawn_in(TaskClass::Realtime, async move {
        let Ok(mut client) = Connector::connect(&path).await else {
            return;
        };
        let Ok(body) = serde_json::to_vec(&ClientMessage::SubscribePlan) else {
            return;
        };
        if client
            .write_raw(&encode(1, FrameKind::Request, &body))
            .await
            .is_err()
        {
            return;
        }
        loop {
            let Ok(frame) = client.read_frame_body().await else {
                break;
            };
            let ev: StreamEvent = match serde_json::from_slice(&frame) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let StreamEvent::PlanOp { envelope } = ev {
                wiring.lock().ingest(envelope);
                render.mark_dirty();
            }
        }
    });
}

/// True if `text` is a slash/command that `handle_submit` dispatches inline
/// (and which may mutate `model`), rather than an assistant prompt. Mirrors the
/// detection order in `handle_submit`; an unrecognized `/foo` is NOT a command
/// here (it is sent to the daemon as a prompt, matching `handle_submit`).
/// Whether `text` is the help command (`/help` or `/?`).
fn is_help_command(text: &str) -> bool {
    let t = text.trim();
    t == "/help" || t == "/?"
}

fn is_slash_command(text: &str) -> bool {
    slash_model_args(text).is_some()
        || slash_account_args(text).is_some()
        || parse_mem_command(text).is_some()
        || parse_clear_command(text).is_some()
        || parse_skill_command(text).is_some()
        || parse_workflow_command(text).is_some()
        // Reserved slash verbs with inline handlers in `handle_submit` that
        // `parse_skill_command` rejects (so they'd otherwise reach the model):
        // `/help` and `/knowledge`. `/vim` and `/permissions` are non-reserved
        // and pass today only via the skill catch-all — named here too so the
        // gate stays authoritative if `RESERVED_SLASH_VERBS` changes. Each
        // predicate mirrors its handler's word-boundary in `handle_submit`.
        || is_help_command(text)
        || slash_verb_boundary(text, "/timeline")
        || slash_verb_boundary(text, "/knowledge")
        || slash_verb_boundary(text, "/permissions")
        || text
            .trim()
            .strip_prefix("/vim")
            .is_some_and(|rest| rest.trim().is_empty())
}

/// True if `text` (trimmed) is exactly `cmd` or `cmd` followed by whitespace —
/// the word-boundary shape the `/knowledge` / `/permissions` handlers use, so
/// `/knowledgefoo` (a skill name) is not matched as the command.
fn slash_verb_boundary(text: &str, cmd: &str) -> bool {
    text.trim()
        .strip_prefix(cmd)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

// `too_many_lines`: single linear dispatch over many slash commands; splitting
// hurts readability. The other three are localized, pre-existing idioms inside
// this long dispatch (a scoped `app.lock()` guard feeding a rendered message; a
// block-local `use`/`const` next to its sole use) that read more clearly inline
// than hoisted to the function top.
#[allow(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::items_after_statements
)]
async fn handle_submit(
    app: &SharedApp,
    handle: &Handle,
    path: &str,
    model: &mut String,
    text: &str,
    session_id: &str,
    // Bug #5: one-shot channel used by the input loop to forward a Ctrl+C
    // hit while this Prompt is in flight. Only the Prompt path (the
    // streaming branch) uses it — slash commands that round-trip in a
    // single frame do not need to be interruptible.
    interrupt_rx: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
) {
    // `/model <name>` swaps the active model for subsequent prompts.
    // Client-side only: the daemon doesn't store an "active model" —
    // every PromptRequest carries its model string, so updating the
    // local `model` and the status-line snapshot is enough.
    if let Some(rest) = slash_model_args(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        if let Some(name) = parse_model_command(text) {
            model.clear();
            model.push_str(&name);
            let mut a = app.lock();
            a.set_model(name.clone());
            a.add_line(
                "system> ",
                &origin_cli::locale::linef("cmd.model.set", &[("name", name.as_str())]),
            );
            drop(a);
        } else {
            let _ = rest; // unused when usage hint fires; matches `/account`'s shape
            app.lock()
                .add_line("error> ", origin_cli::locale::line("cmd.model.usage"));
        }
        handle.mark_dirty();
        return;
    }
    // `/effort <level>` and `/fast` set the session reasoning-effort token that
    // every subsequent PromptRequest carries. Client-side only — like /model.
    if let Some(parsed) = origin_cli::effort::parse_effort_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match parsed {
            Some(level) => {
                let token = level.as_str();
                app.lock().effort = Some(token.to_string());
                app.lock().add_line(
                    "system> ",
                    &origin_cli::locale::linef("cmd.effort.set", &[("token", token)]),
                );
            }
            None => app
                .lock()
                .add_line("error> ", origin_cli::locale::line("cmd.effort.usage")),
        }
        handle.mark_dirty();
        return;
    }
    // `/output-style <default|explanatory|learning|concise>` sets the session
    // output style; its system suffix is sent on every subsequent PromptRequest.
    if let Some(arg) = text
        .trim()
        .strip_prefix("/output-style")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match origin_outputstyle::Style::from_str_opt(arg.trim()) {
            Some(style) => {
                app.lock().output_style = Some(style);
                app.lock().add_line(
                    "system> ",
                    &origin_cli::locale::linef("cmd.outputstyle.set", &[("label", style.label())]),
                );
            }
            None => app
                .lock()
                .add_line("error> ", origin_cli::locale::line("cmd.outputstyle.usage")),
        }
        handle.mark_dirty();
        return;
    }
    // `/steer <text>` queues a steering hint (gemini model steering). The hint
    // is merged ahead of the user's text on the next real prompt, without
    // starting a turn itself.
    if let Some(hint) = text
        .trim()
        .strip_prefix("/steer")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        app.lock().add_line("you> ", text);
        if hint.is_empty() {
            app.lock()
                .add_line("error> ", origin_cli::locale::line("cmd.steer.usage"));
        } else {
            let pending = {
                let mut a = app.lock();
                a.steering.push(hint);
                a.steering.len()
            };
            let pending = pending.to_string();
            app.lock().add_line(
                "system> ",
                &origin_cli::locale::linef("cmd.steer.queued", &[("pending", pending.as_str())]),
            );
        }
        handle.mark_dirty();
        return;
    }
    // `/timeline [<id>|revert <id>]` — in-TUI checkpoint timeline + diff viewer
    // (gap 3). No arg lists checkpoints; `<id>` renders that checkpoint's patch
    // into the scrollable scrollback; `revert <id>` restores the working tree
    // from it (files-only, HEAD unmoved). Checkpoints are client-local shadow git.
    if let Some(arg) = text
        .trim()
        .strip_prefix("/timeline")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        app.lock().add_line("you> ", text);
        if let Some(cp_id) = arg
            .strip_prefix("revert")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim)
        {
            match origin_cli::vcs::rewind_to(cp_id, true) {
                Ok(()) => app.lock().add_line(
                    "system> ",
                    &format!("reverted working tree from checkpoint {cp_id}"),
                ),
                Err(e) => app.lock().add_line("error> ", &format!("{e}")),
            }
        } else if arg.is_empty() {
            match origin_cli::vcs::list_checkpoints() {
                Ok(cps) => {
                    for line in origin_cli::vcs::format_checkpoint_lines(&cps) {
                        app.lock().add_line("system> ", &line);
                    }
                }
                Err(e) => app.lock().add_line("error> ", &format!("{e}")),
            }
        } else {
            match origin_cli::vcs::checkpoint_patch(arg) {
                Ok(diff) => {
                    for line in diff.lines() {
                        app.lock().add_line("", line);
                    }
                }
                Err(e) => app.lock().add_line("error> ", &format!("{e}")),
            }
        }
        handle.mark_dirty();
        return;
    }
    // `/plan [on|off]` toggles read-only plan mode (gemini Plan Mode). With no
    // argument it flips the current state; subsequent prompts run read-only
    // (the daemon denies every mutating tool) until toggled off.
    if let Some(arg) = text
        .trim()
        .strip_prefix("/plan")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        app.lock().add_line("you> ", text);
        let now_on = {
            let mut a = app.lock();
            a.plan_mode = match arg {
                "on" => true,
                "off" => false,
                _ => !a.plan_mode,
            };
            a.plan_mode
        };
        let msg = if now_on {
            "plan mode ON — mutating tools (Edit/Write/Bash/…) are disabled until /plan off"
        } else {
            "plan mode OFF — edits and commands re-enabled"
        };
        app.lock().add_line("system> ", msg);
        handle.mark_dirty();
        return;
    }
    // `/vim` toggles the opt-in vim input layer (aider L107). With it OFF (the
    // default) the composer is direct-insert (byte-identical); ON starts in vim
    // Normal mode (hjkl/0/$/w/b move, i/a/A/I enter insert, Esc → Normal).
    if text
        .trim()
        .strip_prefix("/vim")
        .is_some_and(|rest| rest.trim().is_empty())
    {
        app.lock().add_line("you> ", text);
        let now_on = app.lock().toggle_vim();
        let msg = if now_on {
            "vim mode ON \u{2014} Normal mode (i to insert, Esc to return); /vim to disable"
        } else {
            "vim mode OFF \u{2014} direct insert (default)"
        };
        app.lock().add_line("system> ", msg);
        handle.mark_dirty();
        return;
    }
    // `/knowledge <add|search|ls|rm> …` runs the local knowledge index in-session
    // (openclaude `/knowledge` parity), pushing results into the scrollback. The
    // standalone `origin knowledge` subcommand remains; this gives the TUI parity.
    if text
        .trim()
        .strip_prefix("/knowledge")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        app.lock().add_line("you> ", text);
        match origin_cli::input::parse_knowledge_command(text) {
            Some(sub) => match origin_cli::knowledge::run_to_string(sub) {
                Ok(out) if out.is_empty() => app.lock().add_line("knowledge> ", "(no output)"),
                Ok(out) => {
                    let mut a = app.lock();
                    for line in out.lines() {
                        a.add_line("knowledge> ", line);
                    }
                }
                Err(e) => app.lock().add_line("error> ", &format!("knowledge: {e}")),
            },
            None => app.lock().add_line(
                "error> ",
                "usage: /knowledge <add <id> <text…> | search <query…> | ls | rm <id>>",
            ),
        }
        handle.mark_dirty();
        return;
    }
    // `/permissions [on|off]` toggles opt-in interactive tool prompting. When
    // ON, the daemon asks before running RequiresPermission tools (Bash/Write/
    // Edit/…) and the user answers each with y/n. Default OFF keeps the historic
    // auto-allow behaviour (byte-identical wire + tool execution).
    if let Some(arg) = text
        .trim()
        .strip_prefix("/permissions")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        app.lock().add_line("you> ", text);
        let now_on = app.lock().set_permission_ask(arg);
        let msg = if now_on {
            "permission prompts ON \u{2014} approve Bash/Write/Edit before they run (y/n); /permissions off to disable"
        } else {
            "permission prompts OFF \u{2014} tools auto-run (default)"
        };
        app.lock().add_line("system> ", msg);
        handle.mark_dirty();
        return;
    }
    // `/mouse [on|off]` toggles terminal mouse capture. ON (default) gives wheel
    // scrolling but intercepts the terminal's native drag-select/copy; OFF
    // releases capture so the user can select & copy (scroll via PageUp/Shift+↑↓).
    if let Some(arg) = text
        .trim()
        .strip_prefix("/mouse")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        app.lock().add_line("you> ", text);
        let now_on = app.lock().set_mouse_capture(arg);
        // Hold the stdout lock across the mode toggle so it can't interleave with
        // a concurrent render-frame write.
        if now_on {
            let _ = execute!(std::io::stdout().lock(), EnableMouseCapture);
        } else {
            let _ = execute!(std::io::stdout().lock(), DisableMouseCapture);
        }
        let msg = if now_on {
            "mouse capture ON \u{2014} wheel scrolls; drag to select & auto-copy (/mouse off for terminal-native select)"
        } else {
            "mouse capture OFF \u{2014} terminal-native select & copy; scroll with PageUp/Shift+\u{2191}\u{2193} (/mouse on to re-enable)"
        };
        app.lock().add_line("system> ", msg);
        handle.mark_dirty();
        return;
    }
    // `/theme [name]` switches the color palette (default/dark/light/high-contrast).
    // The chrome re-themes immediately; scrollback already on screen keeps its
    // baked colors (only new lines pick up the new theme).
    if let Some(arg) = text
        .trim()
        .strip_prefix("/theme")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        let msg = {
            let mut a = app.lock();
            a.add_line("you> ", text);
            if a.set_theme_by_name(arg) {
                format!("theme: {}", a.theme.name())
            } else {
                "usage: /theme default | dark | light | high-contrast".to_string()
            }
        };
        app.lock().add_line("system> ", &msg);
        handle.mark_dirty();
        return;
    }
    // `/help` prints a command + keybinding cheatsheet to the scrollback. The
    // power keystrokes (history recall, scroll, Home/End) are otherwise
    // undiscoverable, and most slash commands are only found by typing `/`.
    if is_help_command(text) {
        let mut a = app.lock();
        a.add_line("you> ", text);
        a.add_line("system> ", "commands:");
        a.add_line(
            "tab> ",
            "/model /effort /fast /output-style /steer /plan /attach /account /mem",
        );
        a.add_line("tab> ", "/theme /permissions /mouse /copy /clear /help");
        a.add_line("system> ", "keys:");
        a.add_line(
            "tab> ",
            "Enter submit \u{00B7} Shift+Enter newline \u{00B7} Tab complete",
        );
        a.add_line("tab> ", "\u{2190}/\u{2192} move \u{00B7} Home/End line \u{00B7} \u{2191}/\u{2193} history \u{00B7} Backspace/Delete");
        a.add_line(
            "tab> ",
            "PageUp/PageDown \u{00B7} Shift+\u{2191}/\u{2193} scroll \u{00B7} End jump to bottom",
        );
        a.add_line(
            "tab> ",
            "Ctrl+C interrupt/quit \u{00B7} Ctrl+D exit \u{00B7} Esc dismiss popup",
        );
        drop(a);
        handle.mark_dirty();
        return;
    }
    // `/copy` copies the last assistant reply to the system clipboard via an
    // OSC 52 escape (works over SSH — the terminal owns the clipboard). This is
    // the only in-TUI copy path; mouse capture otherwise intercepts selection.
    if text.trim() == "/copy" {
        let mut a = app.lock();
        a.add_line("you> ", text);
        let reply = a
            .last_assistant
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(str::to_string);
        if let Some(reply) = reply {
            a.add_line("ok> ", origin_cli::locale::line("cmd.copy.ok"));
            drop(a);
            // Dual copy (OS clipboard + OSC 52) through the serialized writer,
            // same path as click-drag selection.
            copy_text_to_clipboard(reply);
        } else {
            a.add_line("system> ", origin_cli::locale::line("cmd.copy.empty"));
            drop(a);
        }
        handle.mark_dirty();
        return;
    }
    // `/attach <path>` stages an image/PDF for the next prompt (interactive
    // parity with headless `origin run --attach`). The file is classified and
    // encoded CLI-side so the daemon never reads client paths.
    if let Some(path_arg) = text
        .trim()
        .strip_prefix("/attach")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim)
    {
        app.lock().add_line("you> ", text);
        if path_arg.is_empty() {
            app.lock()
                .add_line("error> ", "usage: /attach <path-to-image-or-pdf>");
        } else {
            match attach_file(path_arg) {
                Ok(block) => {
                    let pending = {
                        let mut a = app.lock();
                        a.pending_attachments.push(block);
                        a.pending_attachments.len()
                    };
                    app.lock().add_line(
                        "system> ",
                        &format!("attached `{path_arg}` ({pending} staged for next prompt)"),
                    );
                }
                Err(e) => app.lock().add_line("error> ", &format!("attach failed: {e}")),
            }
        }
        handle.mark_dirty();
        return;
    }
    if let Some(rest) = slash_account_args(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match parse_account_command(rest) {
            Ok((provider, account_id)) => match switch_account(path, &provider, &account_id).await {
                Ok((p, a)) => {
                    // Record the session's active account so it rides on every
                    // subsequent PromptRequest (the daemon prefers it over its
                    // per-connection slot for the cross-provider rebuild). Without
                    // this the switch only lives on the throwaway switch
                    // connection and never reaches the prompt path.
                    set_session_account(Some(a.clone()));
                    let line = origin_cli::locale::linef(
                        "cmd.account.active",
                        &[("provider", p.as_str()), ("account", a.as_str())],
                    );
                    app.lock().add_line("system> ", &line);
                }
                Err(e) => {
                    app.lock().add_line("error> ", &format!("{e}"));
                }
            },
            Err(e) => {
                app.lock().add_line("error> ", e);
            }
        }
        handle.mark_dirty();
        return;
    }
    // /mem accept|reject|edit <N> is handled here so we never start an
    // assistant turn for it (P6.7).
    if let Some(decision) = parse_mem_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match send_decision(path, &decision).await {
            Ok(()) => app.lock().add_line("ok> ", "decision sent"),
            Err(e) => app.lock().add_line("error> ", &format!("{e}")),
        }
        handle.mark_dirty();
        return;
    }
    // `/<name>` / `/<plugin>:<name>` activate; `/-<name>` deactivates.
    // `/clear` is a mechanical context reset (not a skill): it tells the
    // daemon to drop any active goal and wipes the in-session TUI view so the
    // terminal looks as it did at launch. Must be checked BEFORE
    // `parse_skill_command` since `clear` is a reserved slash verb there.
    if let Some(msg) = parse_clear_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match send_skill_command(path, &msg).await {
            Ok(line) => {
                // Reset the view first, then surface the daemon's outcome on
                // the fresh, banner-only screen.
                let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                let mut a = app.lock();
                a.reset_to_login(cols, rows);
                a.add_line("ok> ", &line);
            }
            Err(e) => app.lock().add_line("error> ", &format!("{e}")),
        }
        handle.mark_dirty();
        return;
    }
    // Route through a one-shot IPC connection just like /mem and /account.
    if let Some(msg) = parse_skill_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match send_skill_command(path, &msg).await {
            Ok(line) => app.lock().add_line("ok> ", &line),
            Err(e) => app.lock().add_line("error> ", &format!("{e}")),
        }
        handle.mark_dirty();
        return;
    }
    // `{workflow:<name>}` walks a chain of skills server-side. Same IPC
    // helper as /skill since both replies flow through send_skill_command.
    if let Some(msg) = parse_workflow_command(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match send_skill_command(path, &msg).await {
            Ok(line) => app.lock().add_line("ok> ", &line),
            Err(e) => app.lock().add_line("error> ", &format!("{e}")),
        }
        handle.mark_dirty();
        return;
    }

    begin_prompt_turn(app, text);
    handle.mark_dirty();
    handle_prompt_turn(app, handle, path, model.as_str(), text, session_id, interrupt_rx).await;
}

/// Run one assistant prompt turn: echo the user line, stream the daemon's
/// response into the app, and surface any memory proposals. Reads `model` but
/// never mutates it, so the input loop can spawn this onto its own task (so a
/// long-running turn does not block Ctrl+C / interrupt handling).
#[allow(clippy::too_many_lines)]
async fn handle_prompt_turn(
    app: &SharedApp,
    handle: &Handle,
    path: &str,
    model: &str,
    text: &str,
    session_id: &str,
    interrupt_rx: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
) {
    // The user line is echoed + the turn timer started synchronously by the
    // caller (`begin_prompt_turn`) before this (possibly spawned) task runs, so
    // the first frame after Enter is never empty.
    let mut proposals: Vec<(u32, String, Vec<String>)> = Vec::new();
    let app_for_delta = Arc::clone(app);
    let handle_for_delta = handle.clone();
    let app_for_tool = Arc::clone(app);
    let handle_for_tool = handle.clone();
    let app_for_chunk = Arc::clone(app);
    let handle_for_chunk = handle.clone();
    let app_for_result = Arc::clone(app);
    let handle_for_result = handle.clone();
    let app_for_usage = Arc::clone(app);
    let handle_for_usage = handle.clone();
    let app_for_backoff = Arc::clone(app);
    let handle_for_backoff = handle.clone();
    let app_for_goal = Arc::clone(app);
    let handle_for_goal = handle.clone();
    let app_for_perm = Arc::clone(app);
    let handle_for_perm = handle.clone();
    // Snapshot the session effort level so this turn carries `/effort`/`/fast`.
    let effort = app.lock().effort.clone();
    // Carry the startup `--thinking-tokens` budget on every turn (aider
    // `--thinking-tokens`). `None` ⇒ wire unchanged.
    let thinking_tokens = thinking_tokens_seed();
    // The active output style's system suffix rides in the `system` field.
    let system_suffix = app
        .lock()
        .output_style
        .map(|s| s.system_suffix().to_string())
        .unwrap_or_default();
    // Force `@subagent` delegation when the turn opens with a mention (gap 9c),
    // then drain any `/steer` hints and append them as a cache-safe suffix
    // (gemini model steering). No mention + empty queue ⇒ byte-identical to `text`.
    let directed = origin_cli::mentions::force_subagent(text);
    let user_text = origin_cli::steering::next_turn_prompt(&mut app.lock().steering, &directed);
    let read_only = app.lock().plan_mode;
    // Drain any `/attach`-staged attachments for this turn (empty ⇒ text-only).
    let attachments = std::mem::take(&mut app.lock().pending_attachments);
    // Session-wide multi-root list (from the startup `--root` flags).
    let roots = app.lock().workspace_roots.clone();
    // Opt-in interactive permission prompting (`/permissions on`).
    let permission_ask = app.lock().permission_ask;
    // Opt-in per-turn cost line (`ORIGIN_TURN_COST=1`): snapshot the cumulative
    // USD before the turn so we can render its delta afterward. Default-off: when
    // the env var is unset we never read this value and emit no extra line, so the
    // output is byte-identical.
    let cost_before = origin_cli::status::cost_usd(&app.lock().usage);
    let reply = call_daemon(
        path,
        model,
        &user_text,
        session_id,
        effort,
        thinking_tokens,
        system_suffix,
        read_only,
        attachments,
        roots,
        permission_ask,
        interrupt_rx,
        move |ev: &StreamEvent| {
            // Bug #4: route Goal* events through the dedicated renderer so
            // they no longer fall into call_daemon's `_ => {}` catch-all.
            let mut a = app_for_goal.lock();
            render_goal_event(&mut *a, ev);
            drop(a);
            handle_for_goal.mark_dirty();
        },
        move |id, tool: &str, args: &str| {
            // Surface the permission ask; the event loop's y/n handler reads
            // `pending_permission` and sends the decision on a fresh connection.
            app_for_perm.lock().pending_permission = Some(origin_cli::tui::PendingPermission {
                id,
                tool: tool.to_string(),
                args: args.to_string(),
            });
            handle_for_perm.mark_dirty();
        },
        move |d| {
            app_for_delta.lock().append_to_current_assistant(d);
            handle_for_delta.mark_dirty();
        },
        move |tool, summary, diff_lines: Vec<origin_daemon::protocol::DiffLine>| {
            use origin_cli::theme;
            // Tool-activity header. The `[tool]` bracket marker routes through the
            // `tool.running` catalog key (En "[{tool}]" — byte-identical); the
            // optional summary stays appended in code.
            let head = origin_cli::locale::linef("tool.running", &[("tool", tool)]);
            let line = if summary.is_empty() {
                head
            } else {
                format!("{head} {summary}")
            };
            let mut a = app_for_tool.lock();
            a.finalize_assistant_turn(0);
            // ▸ running marker; flipped to ✔/✘ by on_tool_result / turn end.
            a.start_tool_line(&line);
            // Cap rendered diff rows so a large Write doesn't bury the
            // conversation (these DiffLines are never sent to the model, so this
            // is purely a view change). Indent 2 cols to nest under the header.
            let total_diff = diff_lines.len();
            for dl in diff_lines.iter().take(MAX_DIFF_ROWS) {
                let (fg, bg) = match dl.kind.as_str() {
                    "+" => (theme::DIFF_ADD_FG, theme::DIFF_ADD_BG),
                    "-" => (theme::DIFF_DEL_FG, theme::DIFF_DEL_BG),
                    _ => (theme::MUTED, 0),
                };
                let prefix = match dl.kind.as_str() {
                    "+" => "+",
                    "-" => "-",
                    _ => " ",
                };
                let text = format!("  {:>4} {prefix} {}", dl.line_no, dl.text);
                a.add_colored_line(text, fg, bg);
            }
            if let Some(summary) = origin_cli::tui::diff_elision_summary(total_diff, MAX_DIFF_ROWS) {
                a.add_colored_line(summary, theme::MUTED, 0);
            }
            // Blank separator so an edit's diff doesn't sit flush against the
            // assistant's following reply. Only when diff rows were actually
            // rendered — for every other tool this blank would wedge a gap
            // between the header and its output, detaching them visually.
            if total_diff > 0 {
                a.add_blank_line();
            }
            a.start_assistant_turn();
            // Drop the App guard before signalling the renderer so the lock
            // is not held across mark_dirty (significant_drop_tightening),
            // matching the other stream callbacks in this call.
            drop(a);
            handle_for_tool.mark_dirty();
        },
        move |_tool: &str, content: &str| {
            // Live Bash output: render each incoming line under the tool
            // header as an indented row so users see progress instead of a
            // silent gap during long-running commands. Use the bright body
            // color (not DIM) so generated output is clearly legible rather
            // than washed out.
            use origin_cli::theme;
            let mut a = app_for_chunk.lock();
            for line in content.lines() {
                a.add_colored_line(format!("    {line}"), theme::BODY, 0);
            }
            drop(a);
            handle_for_chunk.mark_dirty();
        },
        move |tool: &str, ok: bool, preview: &str, elided_bytes: u32| {
            // Render the tool's output preview directly under its activity
            // line so the user sees *what the tool did*, instead of just
            // the start indicator followed by a silent gap.
            use origin_cli::theme;
            let mut a = app_for_result.lock();
            // Flip the tool's ▸ running marker to ✔/✘.
            a.finish_tool_line(ok);
            let header_fg = if ok { theme::MUTED } else { theme::RED };
            if !ok {
                // Tool-failure line. The "{tool} failed" sentence routes through
                // the `tool.done` catalog key (En "{tool} failed" — byte-identical);
                // the ✘ glyph and indent stay in code.
                let failed = origin_cli::locale::linef("tool.done", &[("tool", tool)]);
                a.add_colored_line(format!("    \u{2718} {failed}"), header_fg, 0);
            }
            for line in preview.lines() {
                a.add_colored_line(format!("    {line}"), theme::BODY, 0);
            }
            if elided_bytes > 0 {
                a.add_colored_line(
                    format!("    \u{2026} +{elided_bytes} bytes omitted"),
                    theme::MUTED,
                    0,
                );
            }
            // Separate this tool's output from whatever follows (the next tool
            // block or the assistant's reply) so the transcript doesn't read as
            // one dense wall of text.
            a.add_blank_line();
            drop(a);
            handle_for_result.mark_dirty();
        },
        move |i, o, cr, cw| {
            // Apply usage deltas immediately so the status line's token
            // counts and cost tick live during streaming. Elapsed time
            // is driven by `turn_started` in the App.
            app_for_usage.lock().record_usage_tokens(i, o, cr, cw);
            handle_for_usage.mark_dirty();
        },
        |id, body, tags| proposals.push((id, body, tags)),
        move |secs, attempt, max_attempts| {
            // Surface rate-limit backoff sleeps so they don't look like a
            // hang. The daemon sleeps up to MAX_RATE_LIMIT_SLEEP_SECS (60s)
            // per attempt; without this line the CLI shows zero output for
            // the entire sleep window.
            use origin_cli::theme;
            let mut a = app_for_backoff.lock();
            a.add_colored_line(
                format!("    rate limited - retrying in {secs}s (attempt {attempt}/{max_attempts})"),
                theme::MUTED,
                0,
            );
            drop(a);
            handle_for_backoff.mark_dirty();
        },
    )
    .await;

    // claude-code MessageDisplay (CLI render side): fire the `MessageDisplay`
    // shell hook on the final assistant text *before* taking the `App` lock, so
    // no parking_lot guard is held across the await. Default-off: with no
    // `hooks.json` / no `MessageDisplay` hook this is `None` ⇒ identity, and the
    // output-style transform alone decides the render (byte-identical).
    let display_action = if reply.is_ok() {
        fire_message_display_hook(app).await
    } else {
        None
    };

    let mut a = app.lock();
    // End the live timer regardless of success/failure so elapsed stops
    // ticking and folds into the cumulative total.
    a.stop_turn_timer();
    match reply {
        Ok((r, _elapsed)) => {
            a.finalize_assistant_turn_with_action(r.turns, display_action.as_ref());
            // Render each memory proposal as a status line (P6.7).
            for (id, body, tags) in &proposals {
                let truncated: String = body.chars().take(60).collect();
                let tag_str = tags.join(", ");
                a.add_line(
                    "mem> ",
                    &format!(
                        "[#{id}] \"{truncated}\" (tags: {tag_str}) — /mem accept {id}, /mem reject {id}, /mem edit {id} <body>"
                    ),
                );
            }
            // Opt-in per-turn cost line (`ORIGIN_TURN_COST=1`). Default-off ⇒ no
            // line ⇒ byte-identical. Routes through the `cost.turn` catalog key
            // ("This turn cost {usd}"), localizing under `--lang`/`$LANG`.
            if origin_cli::status::turn_cost_enabled() {
                let cost_after = origin_cli::status::cost_usd(&a.usage);
                if let Some(line) = origin_cli::status::format_turn_cost(cost_after - cost_before) {
                    a.add_line("system> ", &line);
                }
            }
        }
        Err(e) => {
            a.current_assistant = None;
            // The turn-error line routes through the `error.generic` catalog key.
            // En is the bare `{message}` passthrough, so the rendered body equals
            // `format!("{e}")` byte-for-byte; other locales wrap it with a localized
            // prefix (e.g. "Algo salió mal: <error>").
            let msg = format!("{e}");
            a.add_line(
                "error> ",
                &origin_cli::locale::linef("error.generic", &[("message", &msg)]),
            );
        }
    }
    drop(a);
    handle.mark_dirty();
}

/// Fire the `MessageDisplay` shell hook on the buffered assistant text.
///
/// Snapshots the text under a short-lived lock (the `parking_lot` guard is dropped
/// before the await, never held across it), then dispatches to the configured
/// hook and returns its [`DisplayAction`](origin_outputstyle::DisplayAction).
/// Default-off: with no buffered text, no `hooks.json`, or no `MessageDisplay`
/// hook this is `None` ⇒ the output-style transform alone decides the render
/// (byte-identical to the no-hook path).
async fn fire_message_display_hook(app: &SharedApp) -> Option<origin_outputstyle::DisplayAction> {
    let text = app.lock().current_assistant_text().map(str::to_owned)?;
    origin_cli::display_hook::message_display_action(&text).await
}

/// Read and classify+encode one file into a multimodal content block for the
/// interactive `/attach` command (image → base64 image block; PDF → text).
fn attach_file(path: &str) -> anyhow::Result<origin_multimodal::ContentBlock> {
    let bytes = std::fs::read(path)?;
    origin_multimodal::to_content_block(&bytes, Some(path)).map_err(|e| anyhow::anyhow!("{e}"))
}

// `redundant_pub_crate`: the `tokio::select!` below expands to `pub(crate)`
// helper items; in this bin crate (a private-module root) that trips the lint —
// a known macro false positive, not author-written `pub(crate)` visibility.
#[allow(
    clippy::too_many_arguments,
    clippy::redundant_pub_crate,
    clippy::too_many_lines
)]
async fn call_daemon(
    path: &str,
    model: &str,
    user_text: &str,
    session_id: &str,
    // Session reasoning-effort token (`/effort`/`/fast`); `None` ⇒ wire unchanged.
    effort: Option<String>,
    // Extended-thinking budget in tokens (`--thinking-tokens`); `None` ⇒ wire
    // unchanged. Only the Anthropic provider honours it.
    thinking_tokens: Option<u32>,
    // Active output-style system suffix (`/output-style`); empty ⇒ no addendum.
    system_suffix: String,
    // Read-only plan mode (`/plan`); when true the daemon denies mutating tools.
    read_only: bool,
    // `/attach`-staged multimodal attachments for this turn (empty ⇒ text-only).
    attachments: Vec<origin_multimodal::ContentBlock>,
    // Session-wide extra workspace roots (`--root`); empty ⇒ single-root.
    roots: Vec<String>,
    // Opt-in interactive permission prompting (`/permissions on`); false ⇒ the
    // daemon stays on auto-allow, so the request and tool execution are
    // byte-identical.
    permission_ask: bool,
    // Bug #5: one-shot channel surfacing user Ctrl+C while a Prompt is in
    // flight. When a tick lands we write `ClientMessage::Interrupt` to
    // the same connection serving the prompt — the daemon's
    // drive-goal-loop peek is per-connection so a fresh socket would not
    // do.
    interrupt_rx: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
    // Bug #4: routes Goal* StreamEvent variants so the renderer can
    // update the status indicator / push terminal notices. Called for
    // EVERY Goal variant; non-Goal variants are dispatched in the match
    // below as before.
    mut on_goal: impl FnMut(&StreamEvent) + Send,
    // Opt-in permission ask (id, tool, args_preview): surfaces the approval
    // prompt; the event loop sends the decision back on a fresh connection.
    mut on_permission_ask: impl FnMut(u64, &str, &str) + Send,
    mut on_delta: impl FnMut(&str) + Send,
    mut on_tool: impl FnMut(&str, &str, Vec<origin_daemon::protocol::DiffLine>) + Send,
    mut on_tool_chunk: impl FnMut(&str, &str) + Send,
    mut on_tool_result: impl FnMut(&str, bool, &str, u32) + Send,
    mut on_usage: impl FnMut(u32, u32, u32, u32) + Send,
    mut on_proposal: impl FnMut(u32, String, Vec<String>) + Send,
    mut on_backoff: impl FnMut(u32, u32, u32) + Send,
) -> Result<(PromptReply, Duration)> {
    let start = std::time::Instant::now();
    let mut client = Connector::connect(path).await?;
    let msg = ClientMessage::prompt(PromptRequest {
        system: system_suffix,
        model: model.to_string(),
        user_text: user_text.to_string(),
        session_id: Some(session_id.to_string()),
        effort,
        thinking_tokens,
        attachments,
        read_only,
        roots,
        permission_ask,
        account: session_account(),
    });
    let body = serde_json::to_vec(&msg)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;

    let mut interrupt_rx = interrupt_rx;
    loop {
        // Bug #5: select between the next inbound frame and an interrupt
        // tick. On interrupt, send `ClientMessage::Interrupt` to the
        // daemon on this SAME connection (per-connection-scoped — the
        // daemon's drive_goal_loop peek can only see writes on the
        // connection serving the goal).
        let (kind, body) = {
            // Construct the read future. When no interrupt channel is
            // wired (auto-fire path), fall back to a straight await.
            let read_fut = client.read_frame();
            if let Some(rx) = interrupt_rx.as_mut() {
                tokio::select! {
                    res = read_fut => res?,
                    maybe = rx.recv() => {
                        if maybe.is_some() {
                            // Encode and send Interrupt; then continue
                            // the loop to read the daemon's response
                            // (GoalCleared {UserSlash}, etc.).
                            if let Ok(ib) = serde_json::to_vec(&ClientMessage::Interrupt) {
                                let iframe = encode(1, FrameKind::Request, &ib);
                                let _ = client.write_raw(&iframe).await;
                            }
                        }
                        // Drop the channel so subsequent select! only
                        // awaits the read side — one interrupt per
                        // prompt is enough.
                        interrupt_rx = None;
                        continue;
                    }
                }
            } else {
                read_fut.await?
            }
        };
        // The daemon's error path (agent loop failures, provider errors)
        // writes plain UTF-8 into an `ErrorFrame`. Without this branch we'd
        // try to JSON-decode the text and surface serde's "expected value at
        // line 1 column 1" instead of the actual failure reason.
        if matches!(kind, FrameKind::ErrorFrame) {
            return Err(anyhow::anyhow!("{}", String::from_utf8_lossy(&body)));
        }
        // Try to decode as a StreamEvent first; if that fails, treat as the
        // terminal `PromptReply` Response frame.
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&body) {
            // Bug #4: route every Goal* variant through the dedicated
            // renderer up front. The remaining match handles the
            // non-Goal variants exactly as before.
            if matches!(
                ev,
                StreamEvent::GoalActive { .. }
                    | StreamEvent::GoalIteration { .. }
                    | StreamEvent::GoalVerifying
                    | StreamEvent::GoalCleared { .. }
                    | StreamEvent::GoalInactive
            ) {
                on_goal(&ev);
                continue;
            }
            match ev {
                StreamEvent::TextDelta { text } => on_delta(&text),
                StreamEvent::ToolActivity {
                    tool,
                    summary,
                    diff_lines,
                } => on_tool(&tool, &summary, diff_lines),
                StreamEvent::ToolChunk { tool, content } => on_tool_chunk(&tool, &content),
                StreamEvent::ToolResult {
                    tool,
                    ok,
                    preview,
                    elided_bytes,
                } => on_tool_result(&tool, ok, &preview, elided_bytes),
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                } => on_usage(
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens,
                    cache_creation_input_tokens,
                ),
                StreamEvent::MemoryProposed {
                    proposal_id,
                    body: pbody,
                    suggested_tags,
                } => on_proposal(proposal_id, pbody, suggested_tags),
                StreamEvent::ProviderBackoff {
                    retry_in_secs,
                    attempt,
                    max_attempts,
                } => on_backoff(retry_in_secs, attempt, max_attempts),
                StreamEvent::PermissionAsk {
                    id,
                    ref tool,
                    ref args_preview,
                } => on_permission_ask(id, tool, args_preview),
                _ => {}
            }
            continue;
        }
        let reply: PromptReply = serde_json::from_slice(&body)?;
        return Ok((reply, start.elapsed()));
    }
}

/// Returns `Some(rest)` when `line` is a `/account` command (with or
/// without arguments), where `rest` is the trimmed argument tail.
fn slash_account_args(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("/account")?;
    // Require a word boundary so `/accountfoo` is not matched.
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Returns `Some(rest)` when `line` is a `/model` command (with or
/// without arguments), where `rest` is the trimmed argument tail.
/// Mirrors the shape of [`slash_account_args`] so the `handle_submit`
/// branches read identically; the argument-validation parsing happens
/// downstream in `parse_model_command`.
fn slash_model_args(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("/model")?;
    // Require a word boundary so `/modelfoo` falls through to the skill
    // parser instead of being eaten by the model handler.
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Parses `/account <provider> [<account>]` into a `(provider, account_id)`
/// tuple. Defaults `account_id` to `"default"` when omitted. Returns
/// `Err` with a user-facing message on bad input.
fn parse_account_command(rest: &str) -> Result<(String, String), &'static str> {
    let mut parts = rest.split_whitespace();
    let provider = parts.next().ok_or("usage: /account <provider> [<account>]")?;
    let account = parts.next().unwrap_or("default");
    if parts.next().is_some() {
        return Err("usage: /account <provider> [<account>]");
    }
    Ok((provider.to_string(), account.to_string()))
}

/// Sends `ClientMessage::SwitchAccount` over a one-shot connection and
/// awaits the matching `StreamEvent::ProviderActive` confirmation.
async fn switch_account(path: &str, provider: &str, account_id: &str) -> Result<(String, String)> {
    let mut client: Connection = Connector::connect(path).await?;
    let msg = ClientMessage::SwitchAccount {
        provider: provider.to_string(),
        account_id: account_id.to_string(),
    };
    let body = serde_json::to_vec(&msg)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;

    let body = client.read_frame_body().await?;
    match serde_json::from_slice::<StreamEvent>(&body) {
        Ok(StreamEvent::ProviderActive { provider, account_id }) => Ok((provider, account_id)),
        Ok(other) => Err(anyhow::anyhow!("unexpected event: {other:?}")),
        Err(_) => {
            // Likely an ErrorFrame surfaced through `read_frame_body`; the
            // bytes are typically a UTF-8 message from the daemon.
            let text = String::from_utf8_lossy(&body).into_owned();
            Err(anyhow::anyhow!("switch failed: {text}"))
        }
    }
}

/// Send a `ClientMessage::MemoryDecision` to the daemon and wait for the
/// acknowledgement frame. Opens a one-shot connection — the decision is
/// fire-and-forget for the user, but we still drain the ack so the daemon's
/// write buffer is unblocked.
async fn send_decision(path: &str, decision: &ClientMessage) -> Result<()> {
    let mut client: Connection = Connector::connect(path).await?;
    let body = serde_json::to_vec(decision)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;
    // Best-effort drain a single frame so the daemon's reply isn't orphaned.
    // Errors here are non-fatal — the decision has already been sent.
    let _ = client.read_frame_body().await;
    Ok(())
}

/// Send a skill activate/deactivate message and drain the daemon's reply,
/// returning a one-line summary to render in the TUI. Mirrors the
/// `/mem` `send_decision` helper in shape.
async fn send_skill_command(path: &str, msg: &ClientMessage) -> Result<String> {
    let mut client: Connection = Connector::connect(path).await?;
    let body = serde_json::to_vec(msg)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;
    // `/goal <cond>` and `/clear` can emit MULTIPLE event frames: the
    // first GoalCleared (when replacing a prior goal or wiping the
    // session), and then the new state event (GoalActive, GoalInactive,
    // or the SkillActive for `/clear`). Read until we see a terminal
    // event we can render — we never block forever because the daemon
    // always emits at least one of these per request.
    let mut last_intermediate: Option<String> = None;
    loop {
        let resp = client.read_frame_body().await?;
        let ev: StreamEvent = serde_json::from_slice(&resp).map_err(|e| anyhow::anyhow!("bad reply: {e}"))?;
        // Bug #4 + #20: render Goal* outcomes inline so /goal-related
        // slash commands surface the same colored notices the streaming
        // path uses.
        match &ev {
            StreamEvent::GoalActive {
                condition,
                max_iter,
                token_budget,
            } => {
                return Ok(format!(
                    "goal active: {condition}  (max_iter={max_iter}, budget={token_budget})"
                ));
            }
            StreamEvent::GoalInactive => {
                return Ok("no active goal".to_string());
            }
            StreamEvent::GoalCleared {
                reason,
                iter,
                tokens_spent,
            } => {
                // When this is the FIRST event of a `/goal <new>` reply,
                // the daemon will follow up with a GoalActive — keep
                // looping so the caller sees the new activation summary.
                // When it's the only event (a bare-`/-goal` or a /clear
                // with no follow-up), surface it as the final outcome.
                let (msg, _fg) = origin_cli::goal_render::cleared_line(reason);
                last_intermediate = Some(format!("{msg} (iter {iter}, {tokens_spent} tok)"));
                continue;
            }
            _ => {}
        }
        // Terminal arms: delegate to the outcome mapper, which returns the
        // final summary/error string for this reply.
        return skill_command_outcome(ev, &mut last_intermediate);
    }
}

/// Map a terminal skill/workflow [`StreamEvent`] to the one-line summary (or
/// error) that [`send_skill_command`] returns. `last_intermediate` carries a
/// prior `GoalCleared` line that `/clear` (`AdminOk`) folds into its message.
fn skill_command_outcome(ev: StreamEvent, last_intermediate: &mut Option<String>) -> Result<String> {
    match ev {
        StreamEvent::SkillActive { name, allowed_tools } => {
            if allowed_tools.is_empty() {
                Ok(format!("skill `{name}` active (no narrowing)"))
            } else {
                Ok(format!(
                    "skill `{name}` active; allowed tools: {}",
                    allowed_tools.join(", ")
                ))
            }
        }
        StreamEvent::SkillError { message } => Err(anyhow::anyhow!("{message}")),
        StreamEvent::AdminOk => {
            // `/clear` arrives here after the GoalCleared (if any) was
            // already absorbed into `last_intermediate`. Combine them
            // into one line so the user sees both outcomes.
            last_intermediate.take().map_or_else(
                || Ok("skill deactivated".to_string()),
                |prior| Ok(format!("skill deactivated; {prior}")),
            )
        }
        StreamEvent::WorkflowActive { name, steps, skipped } => {
            let main = if steps.is_empty() {
                format!("workflow `{name}` activated (no steps resolved)")
            } else {
                format!("workflow `{name}` activated; skills: {}", steps.join(" → "))
            };
            if skipped.is_empty() {
                Ok(main)
            } else {
                Ok(format!("{main}  (skipped: {})", skipped.join(", ")))
            }
        }
        StreamEvent::WorkflowStepActive {
            name,
            step_index,
            total_steps,
            skill,
            skipped,
        } => {
            let pos = step_index + 1;
            let main = format!("workflow `{name}` step {pos}/{total_steps}: `{skill}` active");
            if skipped.is_empty() {
                Ok(main)
            } else {
                Ok(format!("{main}  (skipped: {})", skipped.join(", ")))
            }
        }
        StreamEvent::WorkflowComplete { name, skipped } => {
            if skipped.is_empty() {
                Ok(format!("workflow `{name}` complete"))
            } else {
                Ok(format!(
                    "workflow `{name}` complete  (skipped: {})",
                    skipped.join(", ")
                ))
            }
        }
        StreamEvent::WorkflowStepHeld {
            name,
            step_index,
            total_steps,
            skill,
            message,
        } => {
            let pos = step_index + 1;
            Ok(format!(
                "workflow `{name}` step {pos}/{total_steps} held on `{skill}` — {message}; retry your prompt to resume"
            ))
        }
        other => Err(anyhow::anyhow!("unexpected reply: {other:?}")),
    }
}

fn sub_to_action(sub: SessionsSub) -> origin_cli::admin::SessionsAction {
    match sub {
        SessionsSub::Ls => origin_cli::admin::SessionsAction::Ls,
        SessionsSub::Resume { session_id } => origin_cli::admin::SessionsAction::Resume(session_id),
        SessionsSub::Rm { session_id } => origin_cli::admin::SessionsAction::Rm(session_id),
        SessionsSub::Rewind { session_id, keep } => {
            origin_cli::admin::SessionsAction::Rewind { session_id, keep }
        }
    }
}

fn sub_to_action_kr(sub: KeyringSub) -> origin_cli::admin::KeyringAction {
    match sub {
        KeyringSub::Add {
            provider,
            account,
            secret,
        } => origin_cli::admin::KeyringAction::Add {
            provider,
            account,
            secret,
        },
        KeyringSub::List { provider } => origin_cli::admin::KeyringAction::List { provider },
        KeyringSub::Remove { provider, account } => {
            origin_cli::admin::KeyringAction::Remove { provider, account }
        }
        // Login is handled before this function is reached; see the Keyring
        // dispatch arm in main(). This arm is unreachable at runtime.
        KeyringSub::Login { .. } => unreachable!("Login is handled before sub_to_action_kr"),
    }
}

/// `origin pair start [--ttl-secs N]`. Sends a `PairStart` to the
/// local daemon and prints the 6-digit code it returns.
async fn pair_start(ttl_secs: u32) -> Result<()> {
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut c = Connector::connect(&path).await?;
    let msg = ClientMessage::PairStart { ttl_secs };
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body)).await?;
    let resp = c.read_frame_body().await?;
    let ev: StreamEvent = serde_json::from_slice(&resp)?;
    match ev {
        StreamEvent::PairCode {
            code,
            expires_in_secs,
        } => {
            println!("pairing code: {code} (valid {expires_in_secs}s)");
            Ok(())
        }
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

/// `origin pair redeem <origin-url> <code> [--device-id DEV]`. Dials
/// the remote daemon over QUIC, redeems the code, and prints the
/// minted bearer.
async fn pair_redeem(url: &str, code: &str, device_id: Option<String>) -> Result<()> {
    let device = device_id.unwrap_or_else(|| {
        hostname::get()
            .ok()
            .and_then(|n| n.into_string().ok())
            .unwrap_or_else(|| "unknown".into())
    });
    let parsed = origin_cli::admin_url::parse_origin_url(url)?;
    let server_fp = parsed.server_fingerprint()?;
    let client_bundle = origin_cli::admin_url::resolve_client_bundle()?;
    let mut c =
        origin_ipc::quic::QuicConnector::connect(parsed.addr, "origin-daemon", server_fp, &client_bundle)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let msg = ClientMessage::PairRedeem {
        code: code.into(),
        device_id: device.clone(),
    };
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (_kind, resp) = c.read_frame().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let ev: StreamEvent = serde_json::from_slice(&resp)?;
    match ev {
        StreamEvent::PairIssued {
            bearer,
            device_id,
            ttl_secs,
        } => {
            println!("paired device={device_id} ttl={ttl_secs}s");
            println!("token: {bearer}");
            Ok(())
        }
        other => Err(anyhow::anyhow!("pair failed: {other:?}")),
    }
}

/// Default IPC path: per-project (derived from the cwd) so each workspace
/// gets its own daemon. See [`origin_ipc::instance`]. `ORIGIN_SOCK` overrides
/// at every call site (`env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path())`),
/// which restores the old single-shared-daemon behaviour when desired.
fn default_path() -> String {
    origin_ipc::instance::InstanceId::for_cwd().ipc_path()
}

/// Fast probe: try to open the IPC path. Returns `true` when something is
/// already listening (the daemon is up), `false` on any connect error.
async fn daemon_reachable(path: &str) -> bool {
    origin_ipc::transport::Connector::connect(path).await.is_ok()
}

/// Resolve the daemon binary: sibling of current exe, or fall back to PATH.
fn resolve_daemon_binary() -> Result<(std::ffi::OsString, Option<std::path::PathBuf>)> {
    let daemon_name = if cfg!(windows) {
        "origin-daemon.exe"
    } else {
        "origin-daemon"
    };
    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
    let sibling = exe.parent().map(|p| p.join(daemon_name));
    let cmd_path: std::ffi::OsString = match &sibling {
        Some(p) if p.exists() => p.clone().into_os_string(),
        _ => daemon_name.into(),
    };
    Ok((cmd_path, sibling))
}

/// Resolve the supervisor binary: sibling of the current exe.
///
/// Returns the command path plus whether it is **available** (the sibling
/// exists on disk). When it is absent we fall back to a direct daemon spawn, so
/// bring-up never fails for lack of the supervisor. Unlike the daemon, we do not
/// fall back to a bare-name PATH lookup for availability: a supervised launch is
/// only chosen when we can point to a concrete binary.
fn resolve_supervisor_binary() -> (std::ffi::OsString, bool) {
    let name = if cfg!(windows) {
        "origin-supervisor.exe"
    } else {
        "origin-supervisor"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(name)));
    match sibling {
        Some(p) if p.exists() => (p.into_os_string(), true),
        _ => (name.into(), false),
    }
}

/// Path to this workspace's spawn stamp, written each time we spawn a daemon
/// for it (`~/.origin/daemons/<instance>.stamp`). Per-instance — restarting
/// project A's daemon never invalidates project B's stamp.
fn daemon_stamp_path() -> Option<std::path::PathBuf> {
    origin_ipc::instance::InstanceId::for_cwd().stamp_path(dirs::home_dir())
}

/// Path to this workspace's daemon pid file
/// (`~/.origin/daemons/<instance>.pid`). One pid per line: the CLI appends the
/// supervisor/daemon pid it spawns; the daemon appends its own pid on bind.
fn daemon_pid_path() -> Option<std::path::PathBuf> {
    origin_ipc::instance::InstanceId::for_cwd().pid_path(dirs::home_dir())
}

/// Returns `true` when the daemon binary on disk is newer than the last spawn
/// recorded in this workspace's stamp file.
fn daemon_binary_is_newer(binary: &std::ffi::OsStr) -> bool {
    let Some(stamp) = daemon_stamp_path() else {
        return false;
    };
    let bin_mtime = std::fs::metadata(binary).and_then(|m| m.modified()).ok();
    let stamp_mtime = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok();
    match (bin_mtime, stamp_mtime) {
        (Some(bin), Some(stamp)) => bin > stamp,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Touch the stamp file so subsequent launches know when this daemon was spawned.
fn touch_daemon_stamp() {
    if let Some(p) = daemon_stamp_path() {
        let _ = p.parent().map(std::fs::create_dir_all);
        let _ = std::fs::File::create(&p);
    }
}

/// Append `pid` to this workspace's pid file.
fn record_daemon_pid(pid: u32) {
    use std::io::Write as _;
    if let Some(p) = daemon_pid_path() {
        let _ = p.parent().map(std::fs::create_dir_all);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "{pid}");
        }
    }
}

/// Force-kill a single process by pid. Best-effort; exit status ignored
/// (the pid may already be gone).
fn kill_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Kill exactly THIS workspace's daemon/supervisor processes — the pids listed
/// in the per-instance pid file — then remove the file.
///
/// This replaces the old image-name sweep (`taskkill /F /IM origin-daemon.exe`
/// / `pkill -f origin-daemon`), which killed every project's daemon on the
/// machine and made concurrent `origin` sessions impossible: starting origin
/// in project B (after a rebuild) took down project A's live session. Killing
/// by recorded pid scopes the restart to the one workspace being relaunched.
/// The supervisor pid is listed first (the CLI records it at spawn), so it
/// dies before its daemon child and cannot respawn what we kill next.
fn kill_instance_daemon() {
    let Some(p) = daemon_pid_path() else {
        return;
    };
    if let Ok(content) = std::fs::read_to_string(&p) {
        for pid in content.lines().filter_map(|l| l.trim().parse::<u32>().ok()) {
            kill_pid(pid);
        }
    }
    let _ = std::fs::remove_file(&p);
}

/// How long [`ensure_daemon_running`] waits for a freshly spawned daemon to
/// bind the IPC path before giving up.
const STARTUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Make sure `origin-daemon` is listening on `path`. If a fresh probe fails,
/// resolve a daemon binary (sibling of the current exe, or `origin-daemon` on
/// PATH), spawn it detached and poll until the pipe accepts a connection —
/// bounded by `STARTUP_DEADLINE`. `provider`/`account` flow through as
/// `ORIGIN_PROVIDER` / `ORIGIN_ACCOUNT` because the daemon doesn't read
/// `~/.origin/config.toml` — without these, it defaults to `anthropic` and
/// fails the initial credential lookup for any other configured provider.
///
/// If a daemon is already running but the binary on disk is newer, the stale
/// daemon is killed and a fresh one is spawned.
async fn ensure_daemon_running(path: &str, provider: &str, account: &str) -> Result<()> {
    let (cmd_path, sibling) = resolve_daemon_binary()?;
    // Supervised launch is the default: route the daemon through
    // origin-supervisor so the self-dev relaunch sentinel (exit 86) has a
    // runtime consumer that hot-swaps the freshly built binary. Opt out with
    // ORIGIN_NO_SUPERVISOR=1, and fall back to a direct spawn when the
    // supervisor binary isn't shipped alongside us.
    let (supervisor_path, supervisor_available) = resolve_supervisor_binary();
    let launcher = origin_cli::daemon_launch::select_launcher(
        origin_cli::daemon_launch::no_supervisor_env(),
        supervisor_available,
    );
    let supervised = launcher == origin_cli::daemon_launch::Launcher::Supervisor;

    if daemon_reachable(path).await {
        if daemon_binary_is_newer(&cmd_path) {
            tracing::info!("daemon binary is newer than running daemon — restarting");
            // Kill only THIS workspace's supervisor+daemon (recorded pids).
            // The pid file lists the supervisor first, so it dies before it
            // can respawn the daemon child killed right after. Other
            // projects' daemons are untouched.
            kill_instance_daemon();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        } else {
            return Ok(());
        }
    }

    // Daemon stderr captures fatal startup errors that wouldn't otherwise
    // reach the user (the daemon's tracing layer writes to parquet only).
    // Append to `~/.origin/daemon.log` so the user has evidence on disk if
    // the spawn succeeds but the daemon fails to bind. Best-effort: fall
    // back to `null` if we can't open the log.
    let log_stderr: std::process::Stdio = dirs::home_dir()
        .map(|h| h.join(".origin").join("daemon.log"))
        .and_then(|p| {
            p.parent().map(std::fs::create_dir_all).transpose().ok()?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .ok()
        })
        .map_or_else(std::process::Stdio::null, std::process::Stdio::from);

    // Forward the config's provider/account to the daemon child. The daemon
    // resolves the initial provider purely from env vars today; without this
    // the auto-spawned daemon would always try `anthropic/default` even when
    // the user picked `anthropic-oauth` (or any other id) in onboarding.
    // Supervised: spawn origin-supervisor pointed at the daemon binary (it owns
    // and restarts the daemon, inheriting ORIGIN_PROVIDER/ACCOUNT below for the
    // daemon child). Direct: spawn origin-daemon itself (legacy path).
    let mut command = if supervised {
        let mut c = std::process::Command::new(&supervisor_path);
        c.args(origin_cli::daemon_launch::supervisor_args(&cmd_path));
        c
    } else {
        std::process::Command::new(&cmd_path)
    };
    command
        .env("ORIGIN_PROVIDER", provider)
        .env("ORIGIN_ACCOUNT", account)
        // Pin the daemon's IPC path to the one WE resolved. With per-instance
        // paths the daemon's own default derives from its cwd — already set to
        // this workspace below — but an explicit env keeps the pair in lock-step
        // even when the user set ORIGIN_SOCK or the cwd canonicalizes oddly.
        .env("ORIGIN_SOCK", path)
        // Run the daemon IN the workspace: tools (Bash/Read/Write), the code
        // graph, and self-dev all resolve paths against the daemon's cwd, so a
        // project-A daemon must not execute in project B's directory (or in
        // whatever directory the first-ever `origin` happened to start from).
        .current_dir(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log_stderr);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NEW_PROCESS_GROUP (0x200) | CREATE_NO_WINDOW (0x0800_0000):
        // - CREATE_NEW_PROCESS_GROUP detaches the child from the parent's
        //   Ctrl-C signal group so Ctrl-C in the TUI doesn't take the daemon
        //   (or supervisor) down with it.
        // - CREATE_NO_WINDOW runs the console-subsystem child without giving
        //   it a console of its own, so no terminal window pops up. We use this
        //   instead of DETACHED_PROCESS (0x08): DETACHED_PROCESS detaches the
        //   child from the parent console, but because the daemon/supervisor are
        //   console-subsystem binaries Windows then allocates a *new* console
        //   window for them — that was the stray terminal. The child still
        //   outlives this process (we never wait on it).
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let child = command.spawn().map_err(|e| {
        let searched = sibling
            .as_ref()
            .map_or_else(|| "<no exe dir>".to_string(), |p| p.display().to_string());
        let what = if supervised {
            "origin-supervisor"
        } else {
            "origin-daemon"
        };
        anyhow::anyhow!(
            "could not spawn {what}: {e}\n\
             daemon searched: {searched}, then PATH for `origin-daemon`\n\
             build it with `cargo build --release -p {what}` and place the binary \
             next to origin, set ORIGIN_NO_SUPERVISOR=1 to launch the daemon directly, \
             or set ORIGIN_SOCK to an existing daemon's pipe path"
        )
    })?;
    // Start a fresh pid ledger for this spawn generation: record the direct
    // child (supervisor in supervised mode, else the daemon itself) FIRST so
    // the targeted restart kills the supervisor before the daemon it would
    // otherwise respawn. The daemon appends its own pid when it binds.
    if let Some(p) = daemon_pid_path() {
        let _ = std::fs::remove_file(&p);
    }
    record_daemon_pid(child.id());
    drop(child);
    touch_daemon_stamp();

    let deadline = std::time::Instant::now() + STARTUP_DEADLINE;
    while std::time::Instant::now() < deadline {
        if daemon_reachable(path).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!(
        "origin-daemon did not bind {path} within {}s — see ~/.origin/daemon.log \
         for the daemon's stderr (it likely panicked during startup)",
        STARTUP_DEADLINE.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: `is_slash_command` is the gate that decides whether a Submit
    // routes to `handle_submit` (inline) or to the model. Reserved slash verbs
    // (`RESERVED_SLASH_VERBS`) are rejected by `parse_skill_command`, so a
    // reserved verb with an inline handler but no explicit gate predicate —
    // `/knowledge` and `/help` — silently fell through to the model. `/vim` and
    // `/permissions` are non-reserved and were recognized only via the skill
    // catch-all; the gate now names them explicitly so it stays authoritative.
    #[test]
    fn is_slash_command_recognizes_composer_commands() {
        assert!(is_slash_command("/knowledge ls"));
        assert!(is_slash_command("/knowledge"));
        assert!(is_slash_command("/knowledge add foo bar baz"));
        assert!(is_slash_command("/help"));
        assert!(is_slash_command("/?"));
        assert!(is_slash_command("/vim"));
        assert!(is_slash_command("/permissions"));
        assert!(is_slash_command("/permissions on"));
        // gap 3: the in-TUI checkpoint timeline is gated inline, not sent to the model.
        assert!(is_slash_command("/timeline"));
        assert!(is_slash_command("/timeline abc1234"));
        assert!(is_slash_command("/timeline revert abc1234"));
        // (`/timelinefoo` is matched by the skill catch-all, not the `/timeline`
        // handler — the handler's own word-boundary guard keeps them distinct.)
        // Genuine non-commands must still reach the model.
        assert!(!is_slash_command("knowledge foo"));
        assert!(!is_slash_command("please run /vim"));
        assert!(!is_slash_command("/"));
        assert!(!is_slash_command("just a normal prompt"));
    }

    // Regression (stale/"disjointed" TUI text): the absolute-CUP renderer must
    // run with autowrap disabled, or writing the screen's bottom-right cell
    // scrolls the alt-screen and desyncs the damage-diff shadow grid. The setup
    // emits DECAWM-off on entry and `restore_terminal` puts it back on exit.
    #[test]
    fn disable_autowrap_emits_decawm_off() {
        let mut buf: Vec<u8> = Vec::new();
        disable_autowrap(&mut buf).expect("writing to an in-memory buffer is infallible");
        assert_eq!(buf.as_slice(), DISABLE_AUTOWRAP);
        assert_eq!(buf.as_slice(), b"\x1b[?7l");
    }

    #[test]
    fn enable_autowrap_emits_decawm_on() {
        let mut buf: Vec<u8> = Vec::new();
        enable_autowrap(&mut buf).expect("writing to an in-memory buffer is infallible");
        assert_eq!(buf.as_slice(), ENABLE_AUTOWRAP);
        assert_eq!(buf.as_slice(), b"\x1b[?7h");
    }
}
