//! Replay loaded resume tokens to the next daemon over IPC.
//!
//! On supervisor startup (or after a restart), the supervisor loads any
//! persisted [`ResumeToken`]s from `<state_dir>/resume/*.json` and replays
//! one `ClientMessage::ResumeRequest { token }` per token to the freshly
//! launched daemon. The daemon responds with `ServerMessage::ResumeAck`.
//!
//! The wire envelope matches `origin_daemon::protocol::ClientMessage`. We
//! hand-build the JSON to avoid a daemon dependency (which would balloon
//! the supervisor's link surface).

use crate::resume_token::ResumeToken;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::Connector;

/// Replay every loaded token to a daemon listening at `ipc_endpoint`.
///
/// # Errors
/// Propagates connect, serialization, and write errors. The function does
/// **not** await the per-token `ResumeAck`; the caller can drain the
/// response stream separately if it wants to verify each ack.
pub async fn replay_all(tokens: Vec<ResumeToken>, ipc_endpoint: &str) -> anyhow::Result<()> {
    if tokens.is_empty() {
        return Ok(());
    }
    let mut conn = Connector::connect(ipc_endpoint).await?;
    for token in tokens {
        let envelope = serde_json::json!({
            "kind": "resume_request",
            "token": token,
        });
        let body = serde_json::to_vec(&envelope)?;
        conn.write_frame(FrameKind::Request, &body).await?;
    }
    Ok(())
}
