//! Headless one-shot (`origin run`). Connects to the daemon, sends a
//! single Prompt, drains the stream, exits. No Ratatui renderer.

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

/// Drive a single prompt through the daemon and exit.
///
/// # Errors
/// Returns when the daemon transport closes or returns an error frame.
pub async fn run(
    text: String,
    json: bool,
    _remote: Option<String>,
    _bearer: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let model = model.unwrap_or_else(|| {
        std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into())
    });
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut conn = Connector::connect(&path).await?;
    let body = serde_json::to_vec(&ClientMessage::prompt(PromptRequest {
        system: String::new(),
        model,
        user_text: text,
    }))?;
    conn.write_raw(&encode(1, FrameKind::Request, &body)).await?;

    loop {
        let frame = conn.read_frame_body().await?;
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&frame) {
            // Acquire the stdout lock inside this iteration so the
            // (non-Send) guard never crosses an `.await`.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            print_event(&mut out, json, &ev)?;
            continue;
        }
        if json {
            use std::io::Write as _;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            writeln!(out, "{}", String::from_utf8_lossy(&frame))?;
        }
        break;
    }
    Ok(())
}

fn print_event(out: &mut impl std::io::Write, json: bool, ev: &StreamEvent) -> Result<()> {
    if json {
        let line = serde_json::to_string(ev)?;
        writeln!(out, "{line}")?;
    } else if let StreamEvent::TextDelta { text } = ev {
        write!(out, "{text}")?;
        out.flush()?;
    }
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
