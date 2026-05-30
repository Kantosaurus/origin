// SPDX-License-Identifier: Apache-2.0
//! Admin subcommand handlers (P13.4): `origin usage`, `origin sessions`,
//! `origin keyring`.
//!
//! Each handler opens a one-shot local-socket connection to the daemon at
//! `$ORIGIN_SOCK` (falling back to a platform default), sends one
//! [`ClientMessage`] envelope, reads one [`StreamEvent`] reply, and
//! renders it for the terminal. Errors propagate via [`anyhow`].

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

/// Actions accepted by [`sessions`].
pub enum SessionsAction {
    Ls,
    Resume(String),
    Rm(String),
    /// Rewind a session's transcript, keeping the first `keep` turns.
    Rewind { session_id: String, keep: u32 },
}

/// Actions accepted by [`keyring`].
pub enum KeyringAction {
    Add {
        provider: String,
        account: String,
        secret: String,
    },
    List {
        provider: String,
    },
    Remove {
        provider: String,
        account: String,
    },
}

/// Print the daemon's per-provider/per-model token usage as a fixed-width table.
///
/// Reads the metrics snapshot the daemon's `/metrics` exporter also
/// serves, so the CLI and the Prometheus scrape stay aligned.
///
/// # Errors
/// Returns if the daemon refuses, the IPC transport closes, or the event
/// shape doesn't match the expected reply.
pub async fn usage() -> Result<()> {
    let ev = round_trip(ClientMessage::GetUsage).await?;
    match ev {
        StreamEvent::UsageReport { rows } => {
            println!(
                "{:<14} {:<24} {:>14} {:>14} {:>10}",
                "PROVIDER", "MODEL", "TOKENS_IN", "TOKENS_OUT", "COST"
            );
            let mut total = 0.0_f64;
            for r in rows {
                total += r.cost_usd;
                println!(
                    "{:<14} {:<24} {:>14} {:>14} {:>10}",
                    r.provider,
                    r.model,
                    r.tokens_in,
                    r.tokens_out,
                    origin_cost::fmt_usd(r.cost_usd)
                );
            }
            println!(
                "{:<14} {:<24} {:>14} {:>14} {:>10}",
                "", "TOTAL", "", "", origin_cost::fmt_usd(total)
            );
            Ok(())
        }
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

/// Export a persisted session transcript to Markdown (default) or JSON,
/// writing to `out` when given, otherwise stdout.
///
/// # Errors
/// Returns if the daemon refuses, the IPC transport closes, the session is
/// unknown, or the output file cannot be written.
pub async fn export_session(session_id: String, json: bool, out: Option<String>) -> Result<()> {
    let format = if json { "json" } else { "md" }.to_string();
    let ev = round_trip(ClientMessage::ExportSession { session_id, format }).await?;
    match ev {
        StreamEvent::SessionExport { content } => {
            if let Some(path) = out {
                std::fs::write(&path, content)?;
                println!("wrote {path}");
            } else {
                print!("{content}");
            }
            Ok(())
        }
        StreamEvent::AdminError { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

/// List / resume / delete persisted sessions over the local IPC socket.
///
/// # Errors
/// Returns if the daemon refuses, the IPC transport closes, or the event
/// shape doesn't match the expected reply.
pub async fn sessions(action: SessionsAction) -> Result<()> {
    let msg = match action {
        SessionsAction::Ls => ClientMessage::ListSessions,
        SessionsAction::Resume(id) => ClientMessage::ResumeSession { session_id: id },
        SessionsAction::Rm(id) => ClientMessage::RemoveSession { session_id: id },
        SessionsAction::Rewind { session_id, keep } => ClientMessage::RewindSession {
            session_id,
            keep_turns: keep,
        },
    };
    let ev = round_trip(msg).await?;
    match ev {
        StreamEvent::SessionsListed { summaries } => {
            println!("{:<28} {:<26} {:>6}  TITLE", "ID", "MODEL", "MSGS");
            for s in summaries {
                println!(
                    "{:<28} {:<26} {:>6}  {}",
                    s.id,
                    s.model,
                    s.message_count,
                    s.title.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        StreamEvent::AdminOk => {
            println!("ok");
            Ok(())
        }
        StreamEvent::SessionResumed {
            session_id,
            messages_loaded,
            restored_to_turn,
            had_resume_token,
        } => {
            let suffix = if had_resume_token {
                " (resume token present)"
            } else {
                ""
            };
            println!(
                "resumed {session_id}: {messages_loaded} messages, last turn {restored_to_turn}{suffix}"
            );
            Ok(())
        }
        StreamEvent::AdminError { message } => Err(anyhow::anyhow!("{message}")),
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

/// Add / list / remove provider credentials in `KeyVault` over the local
/// IPC socket. `add` reads the secret from stdin when the positional
/// argument is the literal `"-"`.
///
/// # Errors
/// Returns if stdin can't be read, the daemon refuses, the IPC transport
/// closes, or the event shape doesn't match.
pub async fn keyring(action: KeyringAction) -> Result<()> {
    let msg = match action {
        KeyringAction::Add {
            provider,
            account,
            secret,
        } => {
            let secret = read_secret(secret)?;
            ClientMessage::KeyringAdd {
                provider,
                account,
                secret,
            }
        }
        KeyringAction::List { provider } => ClientMessage::KeyringList { provider },
        KeyringAction::Remove { provider, account } => ClientMessage::KeyringRemove { provider, account },
    };
    let ev = round_trip(msg).await?;
    match ev {
        StreamEvent::AdminOk => {
            println!("ok");
            Ok(())
        }
        StreamEvent::AdminError { message } => Err(anyhow::anyhow!("{message}")),
        StreamEvent::KeyringAccounts { provider, accounts } => {
            for a in accounts {
                println!("{provider}/{a}");
            }
            Ok(())
        }
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

fn read_secret(arg: String) -> Result<String> {
    if arg == "-" {
        use std::io::Read as _;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        // Strip a trailing CR as well as LF so secrets piped on Windows (CRLF
        // line endings) don't keep an invisible `\r`, which would corrupt the
        // stored credential and cause auth failures.
        Ok(buf.trim_end_matches(['\r', '\n']).to_string())
    } else {
        Ok(arg)
    }
}

/// Send one [`ClientMessage`] to the daemon and read one [`StreamEvent`] back.
///
/// # Errors
/// Propagates IPC connect/transport failures and JSON (de)serialization errors.
pub(crate) async fn round_trip(msg: ClientMessage) -> Result<StreamEvent> {
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut c = Connector::connect(&path).await?;
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body)).await?;
    let resp = c.read_frame_body().await?;
    let ev: StreamEvent = serde_json::from_slice(&resp)?;
    Ok(ev)
}

/// Resolve the local daemon socket / named-pipe path (`$ORIGIN_SOCK` or the
/// platform default).
#[must_use]
pub(crate) fn socket_path() -> String {
    std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path())
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
