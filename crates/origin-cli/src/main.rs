use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt as _;
use origin_cli::input::{parse_mem_command, reduce, InputAction};
use origin_cli::plan_panel_wiring::Wiring as PlanPanelWiring;
use origin_cli::trace_cmd::TraceQuery;
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

#[derive(Parser)]
#[command(name = "origin", version, about = "origin agentic coding harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Query the trace ring (P11.11). Without any flags, prints the most
    /// recent 100 spans across every kind.
    Trace {
        #[command(subcommand)]
        sub: TraceSub,
    },
    /// Start or redeem a pairing session for remote QUIC clients (P13.2).
    Pair {
        #[command(subcommand)]
        sub: PairSub,
    },
    /// One-shot prompt: connect to the daemon, send `text`, drain to completion, exit.
    Run {
        /// The user prompt.
        text: String,
        /// Emit JSON-Lines stream of every IPC event.
        #[arg(long)]
        json: bool,
        /// Remote daemon URL (`origin://host:port#fingerprint`).
        #[arg(long)]
        remote: Option<String>,
        /// Optional bearer token for remote auth.
        #[arg(long)]
        bearer: Option<String>,
        /// Model override.
        #[arg(long)]
        model: Option<String>,
    },
    /// Daemon usage snapshot (tokens in/out per provider/model).
    Usage,
    /// Manage persisted sessions.
    Sessions {
        #[command(subcommand)]
        sub: SessionsSub,
    },
    /// Manage stored provider credentials.
    Keyring {
        #[command(subcommand)]
        sub: KeyringSub,
    },
}

#[derive(Subcommand)]
enum TraceSub {
    /// Print spans matching the given filters.
    Query(TraceQuery),
}

#[derive(Subcommand)]
enum SessionsSub {
    /// List recent sessions (most-recent first).
    Ls,
    /// Resume a session by id (currently a no-op acknowledgement).
    Resume { session_id: String },
    /// Delete a session and all its messages.
    Rm { session_id: String },
}

#[derive(Subcommand)]
enum KeyringSub {
    /// Add or overwrite a provider secret.
    Add {
        provider: String,
        account: String,
        /// The secret value; read from stdin if `-`.
        secret: String,
    },
    /// List accounts for a provider.
    List { provider: String },
    /// Remove a provider account secret.
    Remove { provider: String, account: String },
}

#[derive(Subcommand)]
enum PairSub {
    /// Daemon-side: show a 6-digit pairing code.
    Start {
        #[arg(long, default_value_t = 60)]
        ttl_secs: u32,
    },
    /// Client-side: redeem a code against a remote daemon.
    Redeem {
        /// Remote URL: `origin://host:port#fingerprint`.
        url: String,
        /// The 6-digit code shown on the daemon host.
        code: String,
        /// Stable device identifier (defaults to hostname).
        #[arg(long)]
        device_id: Option<String>,
    },
}

#[derive(Deserialize)]
struct PromptReply {
    #[allow(dead_code)] // reconstructed live from stream deltas; only `turns` is used.
    assistant_text: String,
    turns: u32,
}

type SharedApp = Arc<Mutex<App>>;
type SharedComposer = Arc<Mutex<Composer>>;
type SharedWidget = Arc<Mutex<StreamWidget>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Dispatch a subcommand if one was given, otherwise fall through to the
    // TUI entry path (preserves the existing env-driven invocation).
    let cli = Cli::parse();
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
        Some(Cmd::Keyring { sub }) => return origin_cli::admin::keyring(sub_to_action_kr(sub)).await,
        None => {}
    }

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let model = env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into());

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
    let app: SharedApp = Arc::new(Mutex::new(App::new("anthropic", model.clone())));
    app.lock().add_line(
        "",
        "Connected; type a prompt and press Enter. Ctrl-C / Esc to quit.",
    );

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
    // Plan side panel wiring (P9.9). The widget is in-process today; the
    // daemon-driven `PlanHandle` broadcast subscription lands in P10. See
    // `plan_panel_wiring.rs` for the TODO marking that integration seam.
    let _plan_panel = PlanPanelWiring::new();
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
