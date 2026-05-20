//! Headless one-shot (`origin run`). Connects to the daemon, sends a
//! single Prompt, drains the stream, exits. No Ratatui renderer.

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

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

    async fn read_frame_body(&mut self) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Local(c) => Ok(c.read_frame_body().await?),
            Self::Remote(c) => {
                let (_k, body) = c.read_frame().await.map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(body)
            }
        }
    }
}

/// Drive a single prompt through the daemon and exit.
///
/// When `remote` is `Some(url)`, dials a QUIC daemon at the parsed
/// address; otherwise connects to the local IPC socket. The `bearer`
/// is plumbed through the signature but not yet sent on the wire —
/// remote auth lands in a follow-up task.
///
/// # Errors
/// Returns when the daemon transport closes or returns an error frame.
pub async fn run(
    text: String,
    json: bool,
    remote: Option<String>,
    bearer: Option<String>,
    model: Option<String>,
) -> Result<()> {
    // Future work: send `bearer` as part of the remote handshake so the
    // QUIC daemon can authorize the connection. Today the pair flow
    // mints the bearer but the wire format is still TBD.
    let _ = bearer;

    let model = model.unwrap_or_else(|| {
        std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into())
    });

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
