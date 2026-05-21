use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt as _;
use origin_cli::cli_def::{Cli, Cmd, KeyringSub, PairSub, ProvidersSub, SessionsSub, TraceSub};
use origin_cli::input::{parse_mem_command, reduce, InputAction};
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

// CLI subcommand dispatch is intentionally inlined here; splitting it into
// per-subcommand entry helpers is a follow-up polish item.
#[allow(clippy::too_many_lines)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Dispatch a subcommand if one was given, otherwise fall through to the
    // TUI entry path (preserves the existing env-driven invocation).
    let cli = Cli::parse();
    if cli.tutorial {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        origin_cli::tutorial::run(stdin.lock(), stdout.lock())?;
        return Ok(());
    }
    match cli.cmd {
        Some(Cmd::Trace {
            sub: TraceSub::Query(q),
        }) => {
            return origin_cli::trace_cmd::invoke(q).map_err(|e| anyhow::anyhow!("{e}"));
        }
        Some(Cmd::Pair { sub }) => {
            return match sub {
                PairSub::Start { ttl_secs } => pair_start(ttl_secs).await,
                PairSub::Redeem { url, code, device_id } => pair_redeem(&url, &code, device_id).await,
            };
        }
        Some(Cmd::Run {
            text,
            json,
            remote,
            bearer,
            model,
        }) => {
            return origin_cli::headless::run(text, json, remote, bearer, model).await;
        }
        Some(Cmd::Usage) => return origin_cli::admin::usage().await,
        Some(Cmd::Sessions { sub }) => return origin_cli::admin::sessions(sub_to_action(sub)).await,
        Some(Cmd::Keyring { sub }) => {
            // Login drives an interactive OAuth flow and must be handled
            // before converting to KeyringAction (which doesn't have a Login
            // variant — Login bypasses the daemon IPC path entirely).
            if let KeyringSub::Login { provider, account } = sub {
                return origin_cli::keyring_login::run(&provider, &account).await;
            }
            return origin_cli::admin::keyring(sub_to_action_kr(sub)).await;
        }
        Some(Cmd::Providers { sub }) => {
            return match sub {
                ProvidersSub::Ls => {
                    origin_cli::providers::ls();
                    Ok(())
                }
                ProvidersSub::Describe { id } => {
                    origin_cli::providers::describe(&id);
                    Ok(())
                }
            };
        }
        Some(Cmd::Init) => {
            return origin_cli::init::run().await;
        }
        Some(Cmd::Import(a)) => {
            let r = origin_cli::import::run_import(&a).map_err(anyhow::Error::from)?;
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
            return Ok(());
        }
        None => {}
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
    // and finally to hard-coded "anthropic" / "claude-opus-4-7" so callers
    // who declined / skipped onboarding still get a working session).
    let (default_provider, default_model) = origin_cli::config::load()
        .ok()
        .flatten()
        .map(|c| (c.primary.provider, c.primary.model))
        .unwrap_or_else(|| ("anthropic".to_string(), "claude-opus-4-7".to_string()));

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let model = env::var("ORIGIN_MODEL").unwrap_or(default_model);

    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;

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
    let app: SharedApp = Arc::new(Mutex::new(App::new(provider_static, model.clone())));
    app.lock().add_line(
        "",
        "Connected; type a prompt and press Enter. Ctrl-C / Esc to quit.",
    );

    // First-run discovery: if `origin init`'s welcome flow queued a pending
    // prompt, fire it as the user's first turn and remove the file so it
    // never auto-fires twice. Errors are non-fatal — the user can always
    // type a prompt manually.
    let pending_prompt = match origin_cli::first_run_prompt::path() {
        Ok(p) => origin_cli::first_run_prompt::drain(&p).ok().flatten(),
        Err(_) => None,
    };

    let scheduler = Scheduler::new(Duration::from_millis(6));
    let handle = scheduler.handle();
    handle.mark_dirty();

    let render_task = {
        let c2 = composer.clone();
        let a2 = app.clone();
        let w2 = widget.clone();
        spawn_in(TaskClass::Realtime, async move {
            scheduler
                .run(move || {
                    // Draw into composer, then collect frame bytes while lock is held,
                    // then release locks before writing to stdout.
                    let bytes = {
                        let mut c = c2.lock();
                        let mut w = w2.lock();
                        a2.lock().draw(&mut c, &mut w);
                        c.frame()
                    };
                    if !bytes.is_empty() {
                        use std::io::Write as _;
                        let _ = std::io::stdout().write_all(&bytes);
                        let _ = std::io::stdout().flush();
                    }
                })
                .await;
        })
    };

    // Auto-fire the pending discovery prompt now that the TUI is wired up.
    if let Some(text) = pending_prompt {
        app.lock()
            .add_line("system> ", "Running queued first-run discovery prompt\u{2026}");
        handle.mark_dirty();
        handle_submit(&app, &handle, &path, &model, &text).await;
    }

    let result = run_event_loop(app, composer, widget, handle, &path, &model).await;

    render_task.abort();
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    result
}

async fn run_event_loop(
    app: SharedApp,
    _composer: SharedComposer,
    _widget: SharedWidget,
    handle: Handle,
    path: &str,
    model: &str,
) -> Result<()> {
    // Plan side panel: subscribe to the daemon's PlanBus over IPC. Each
    // received envelope feeds `PlanPanelWiring::ingest`. The subscribe
    // runs on a dedicated long-lived connection so it survives the
    // one-shot prompt/admin connections each request opens.
    let plan_panel = Arc::new(Mutex::new(PlanPanelWiring::new()));
    spawn_plan_subscription(path.to_string(), Arc::clone(&plan_panel), handle.clone());
    let mut input_stream = crossterm::event::EventStream::new();
    while let Some(maybe_ev) = input_stream.next().await {
        if let crossterm::event::Event::Key(ev) = maybe_ev? {
            let action = {
                let mut a = app.lock();
                reduce(&mut a.input, ev)
            };
            match action {
                InputAction::Quit => break,
                InputAction::Submit(text) => {
                    handle_submit(&app, &handle, path, model, &text).await;
                }
                _ => {
                    handle.mark_dirty();
                }
            }
        }
    }
    Ok(())
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

async fn handle_submit(app: &SharedApp, handle: &Handle, path: &str, model: &str, text: &str) {
    if let Some(rest) = slash_account_args(text) {
        {
            let mut a = app.lock();
            a.add_line("you> ", text);
        }
        handle.mark_dirty();
        match parse_account_command(rest) {
            Ok((provider, account_id)) => match switch_account(path, &provider, &account_id).await {
                Ok((p, a)) => {
                    app.lock()
                        .add_line("system> ", &format!("provider active: {p}/{a}"));
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
    {
        let mut a = app.lock();
        a.add_line("you> ", text);
        a.start_assistant_turn();
    }
    handle.mark_dirty();

    let mut deltas: Vec<String> = Vec::new();
    let mut usage_events: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut proposals: Vec<(u32, String, Vec<String>)> = Vec::new();
    let reply = call_daemon(
        path,
        model,
        text,
        |d| deltas.push(d.to_string()),
        |i, o, cr, cw| usage_events.push((i, o, cr, cw)),
        |id, body, tags| proposals.push((id, body, tags)),
    )
    .await;

    let mut a = app.lock();
    for d in &deltas {
        a.append_to_current_assistant(d);
    }
    match reply {
        Ok((r, elapsed)) => {
            let mut sum = (0u32, 0u32, 0u32, 0u32);
            for (i, o, cr, cw) in &usage_events {
                sum.0 = sum.0.saturating_add(*i);
                sum.1 = sum.1.saturating_add(*o);
                sum.2 = sum.2.saturating_add(*cr);
                sum.3 = sum.3.saturating_add(*cw);
            }
            a.record_usage(sum.0, sum.1, sum.2, sum.3, elapsed);
            a.finalize_assistant_turn(r.turns);
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
        }
        Err(e) => {
            a.current_assistant = None;
            a.add_line("error> ", &format!("{e}"));
        }
    }
    drop(a);
    handle.mark_dirty();
}

async fn call_daemon(
    path: &str,
    model: &str,
    user_text: &str,
    mut on_delta: impl FnMut(&str) + Send,
    mut on_usage: impl FnMut(u32, u32, u32, u32) + Send,
    mut on_proposal: impl FnMut(u32, String, Vec<String>) + Send,
) -> Result<(PromptReply, Duration)> {
    let start = std::time::Instant::now();
    let mut client = Connector::connect(path).await?;
    let msg = ClientMessage::prompt(PromptRequest {
        system: String::new(),
        model: model.to_string(),
        user_text: user_text.to_string(),
    });
    let body = serde_json::to_vec(&msg)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;

    loop {
        let body = client.read_frame_body().await?;
        // Try to decode as a StreamEvent first; if that fails, treat as the
        // terminal `PromptReply` Response frame.
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&body) {
            match ev {
                StreamEvent::TextDelta { text } => on_delta(&text),
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

fn sub_to_action(sub: SessionsSub) -> origin_cli::admin::SessionsAction {
    match sub {
        SessionsSub::Ls => origin_cli::admin::SessionsAction::Ls,
        SessionsSub::Resume { session_id } => origin_cli::admin::SessionsAction::Resume(session_id),
        SessionsSub::Rm { session_id } => origin_cli::admin::SessionsAction::Rm(session_id),
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
    let ca = parsed.fingerprint_to_ca_placeholder();
    let mut c = origin_ipc::quic::QuicConnector::connect(parsed.addr, "origin-daemon", &ca)
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
