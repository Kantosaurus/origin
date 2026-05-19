use std::env;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use origin_cli::input::{parse_mem_command, reduce, InputAction};
use origin_cli::tui::{draw, App};
use origin_daemon::protocol::StreamEvent;
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::{Connection, Connector};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct PromptRequest<'a> {
    system: &'a str,
    model: &'a str,
    user_text: &'a str,
}

#[derive(Deserialize)]
struct PromptReply {
    #[allow(dead_code)] // text is reconstructed live from stream deltas; we only need `turns` to finalize.
    assistant_text: String,
    turns: u32,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let model = env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into());

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new("anthropic", model.clone());
    app.add_line(
        "",
        "Connected; type a prompt and press Enter. Ctrl-C / Esc to quit.",
    );

    let result: Result<()> = async {
        loop {
            terminal.draw(|f| draw(f, &app))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(ev) = event::read()? {
                    match reduce(&mut app.input, ev) {
                        InputAction::Quit => break,
                        InputAction::Submit(text) => {
                            // /mem accept|reject|edit <N> is handled here so
                            // we never start an assistant turn for it.
                            if let Some(decision) = parse_mem_command(&text) {
                                app.add_line("you> ", &text);
                                match send_decision(&path, &decision).await {
                                    Ok(()) => app.add_line("ok> ", "decision sent"),
                                    Err(e) => app.add_line("error> ", &format!("{e}")),
                                }
                                continue;
                            }
                            app.add_line("you> ", &text);
                            app.start_assistant_turn();
                            // Collect deltas + usage synchronously inside call_daemon
                            // so we don't fight the borrow checker over `&mut app`
                            // across the `terminal.draw(|f| draw(f, &app))` shared
                            // borrow. We re-render between deltas inside the closure.
                            let mut deltas: Vec<String> = Vec::new();
                            let mut usage_events: Vec<(u32, u32, u32, u32)> = Vec::new();
                            let mut proposals: Vec<(u32, String, Vec<String>)> = Vec::new();
                            let reply = call_daemon(
                                &path,
                                &model,
                                &text,
                                |d| deltas.push(d.to_string()),
                                |i, o, cr, cw| usage_events.push((i, o, cr, cw)),
                                |id, body, tags| proposals.push((id, body, tags)),
                            )
                            .await;
                            for d in &deltas {
                                app.append_to_current_assistant(d);
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
                                    app.record_usage(sum.0, sum.1, sum.2, sum.3, elapsed);
                                    app.finalize_assistant_turn(r.turns);
                                    // Render each memory proposal as a status line.
                                    for (id, body, tags) in &proposals {
                                        let truncated: String = body.chars().take(60).collect();
                                        let tag_str = tags.join(", ");
                                        app.add_line(
                                            "mem> ",
                                            &format!(
                                                "[#{id}] \"{truncated}\" (tags: {tag_str}) — /mem accept {id}, /mem reject {id}, /mem edit {id} <body>"
                                            ),
                                        );
                                    }
                                }
                                Err(e) => {
                                    // Drop the in-flight buffer on error; surface the
                                    // error in scrollback under the standard prefix.
                                    app.current_assistant = None;
                                    app.add_line("error> ", &format!("{e}"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
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
    let body = serde_json::to_vec(&PromptRequest {
        system: "",
        model,
        user_text,
    })?;
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

/// Send a `ClientMessage::MemoryDecision` to the daemon and wait for the
/// acknowledgement frame. Opens a one-shot connection — the decision is
/// fire-and-forget for the user, but we still drain the ack so the daemon's
/// write buffer is unblocked.
async fn send_decision(path: &str, decision: &origin_daemon::protocol::ClientMessage) -> Result<()> {
    let mut client: Connection = Connector::connect(path).await?;
    let body = serde_json::to_vec(decision)?;
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await?;
    // Best-effort drain a single frame so the daemon's reply isn't orphaned.
    // Errors here are non-fatal — the decision has already been sent.
    let _ = client.read_frame_body().await;
    Ok(())
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
