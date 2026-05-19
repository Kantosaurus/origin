use std::env;
use std::sync::Arc;

use anyhow::Result;
use origin_cas::Store;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::{ClientMessage, MemoryAction, PromptReply, PromptRequest};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_daemon::stream_relay::relay_to_connection;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::{Listener, SharedConnection};
use origin_permission::prompt::AlwaysAllow;
use origin_provider::Provider;
use origin_provider_anthropic::Anthropic;
use origin_stream::Subscriber;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let api_key =
        env::var("ANTHROPIC_API_KEY").map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY must be set"))?;

    let cas_root = env::var("ORIGIN_CAS_ROOT").unwrap_or_else(|_| default_cas_root());
    let cas = Arc::new(
        origin_cas::Store::open(origin_cas::StoreConfig {
            root: cas_root.clone().into(),
            hot_capacity: 256,
            warm_pack_target_bytes: 4 * 1024 * 1024,
            cold_zstd_level: 3,
        })
        .map_err(|e| anyhow::anyhow!("cas open: {e}"))?,
    );
    info!(cas_root = %cas_root, "cas store ready");

    let provider: Arc<dyn Provider> = Arc::new(Anthropic::new(api_key).with_cas(Arc::clone(&cas)));

    let db_path = env::var("ORIGIN_DB").unwrap_or_else(|_| default_db_path());
    let session_store = Arc::new(SessionStore::open(&db_path)?);
    info!(db = %db_path, "session store ready");

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    loop {
        let conn = listener.accept().await?;
        let shared_conn: SharedConnection = Arc::new(Mutex::new(conn));

        spawn_handler_task(
            shared_conn,
            Arc::clone(&provider),
            Arc::clone(&session_store),
            Arc::clone(&cas),
        );
    }
}

fn spawn_handler_task(
    conn: SharedConnection,
    provider: Arc<dyn Provider>,
    session_store: Arc<SessionStore>,
    cas: Arc<Store>,
) {
    tokio::spawn(async move {
        loop {
            let body = {
                let mut g = conn.lock().await;
                match g.read_frame_body().await {
                    Ok(b) => b,
                    Err(_) => break,
                }
            };
            // Try the new ClientMessage envelope first, then fall back to the
            // legacy raw `PromptRequest` shape (back-compat for clients that
            // pre-date P6.7).
            let msg: ClientMessage = if let Ok(m) = serde_json::from_slice::<ClientMessage>(&body) {
                m
            } else {
                #[allow(deprecated)]
                let res = from_legacy_prompt_request(&body);
                match res {
                    Ok(m) => m,
                    Err(e) => {
                        error!(error = %e, "bad client message");
                        let _ = conn
                            .lock()
                            .await
                            .write_frame(FrameKind::ErrorFrame, format!("bad request: {e}").as_bytes())
                            .await;
                        continue;
                    }
                }
            };

            match msg {
                ClientMessage::Prompt(req) => {
                    if !handle_request(
                        &conn,
                        provider.as_ref(),
                        session_store.as_ref(),
                        Arc::clone(&cas),
                        req,
                    )
                    .await
                    {
                        break;
                    }
                }
                ClientMessage::MemoryDecision { proposal_id, action } => {
                    // TODO(P6.9): wire MemoryHandle here so Accept/Edit
                    // actually persist into origin-mem. For P6.7 we just log
                    // and acknowledge the decision.
                    handle_memory_decision_stub(&conn, proposal_id, &action).await;
                }
            }
        }
    });
}

/// Back-compat shim: legacy clients send raw `PromptRequest` JSON without a
/// `kind` discriminator. Wrap such bodies into `ClientMessage::Prompt`.
#[deprecated(note = "send ClientMessage::Prompt explicitly; legacy fallback for pre-P6.7 clients")]
fn from_legacy_prompt_request(body: &[u8]) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_slice::<PromptRequest>(body).map(ClientMessage::Prompt)
}

/// Per-connection acknowledgement for a [`ClientMessage::MemoryDecision`].
/// The P6.7 implementation is intentionally a stub — the daemon owns no
/// `MemoryHandle` yet, so the decision is logged and acknowledged with a
/// `Response` frame so the client can unblock its `send_decision` call.
async fn handle_memory_decision_stub(
    conn: &origin_ipc::transport::SharedConnection,
    proposal_id: u32,
    action: &MemoryAction,
) {
    let kind = match action {
        MemoryAction::Accept => "accept",
        MemoryAction::Reject => "reject",
        MemoryAction::Edit { .. } => "edit",
    };
    info!(proposal_id, action = %kind, "memory decision (stub)");
    let body = format!("{{\"ok\":true,\"proposal_id\":{proposal_id},\"action\":\"{kind}\"}}");
    let _ = conn
        .lock()
        .await
        .write_frame(FrameKind::Response, body.as_bytes())
        .await;
}

/// Run one request to completion. Returns `false` if the response write
/// failed (the connection is dead and the handler task should exit).
///
/// Bug-2 guard: the relay (Event frames) and the handler (Response frame) both
/// write to the same `SharedConnection`. If the handler grabs the conn mutex
/// and writes `Response` while the relay still has buffered events to flush,
/// the CLI sees `Response` first and exits, dropping later events. We fix this
/// by spawning a *per-request* relay task here, capturing its `JoinHandle`, and
/// awaiting it AFTER `run_loop` returns (the per-turn rings are closed by then)
/// and BEFORE writing the `Response`. Dropping `tx_sub` after `run_loop` closes
/// the per-request `Subscriber` channel, which terminates the relay's outer
/// loop deterministically.
async fn handle_request(
    conn: &SharedConnection,
    provider: &dyn Provider,
    session_store: &SessionStore,
    cas: Arc<Store>,
    req: PromptRequest,
) -> bool {
    let mut session = Session::new("anthropic", &req.model);
    let (tx_sub, mut rx_sub) = mpsc::channel::<Subscriber>(1);
    let conn_for_relay = Arc::clone(conn);
    let relay_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        while let Some(sub) = rx_sub.recv().await {
            if let Err(e) = relay_to_connection(sub, Arc::clone(&conn_for_relay)).await {
                error!(error = %e, "relay terminated");
                break;
            }
        }
    });

    // Scope `opts` so its `relay_tx` Sender clone is dropped on this line —
    // otherwise the channel has TWO senders (one in `tx_sub`, one in `opts`)
    // and `rx_sub.recv()` never returns None, so the relay task hangs forever.
    let loop_result = {
        let opts = LoopOptions {
            max_turns: 25,
            cas: Some(cas),
            relay_tx: Some(tx_sub.clone()),
            streaming_disabled: false,
            proposer: None,
            event_tx: None,
        };
        run_loop(&mut session, &req.user_text, provider, &AlwaysAllow, &opts).await
    };
    // Close the per-request Subscriber channel so the relay task exits its
    // outer loop once it finishes flushing the last ring.
    drop(tx_sub);
    // Wait for the relay to flush every Event frame for this request before we
    // write the Response. Errors here are non-fatal (already logged inside the
    // relay task); we just need to know it finished.
    if let Err(e) = relay_handle.await {
        error!(error = %e, "relay join");
    }

    match loop_result {
        Ok(summary) => {
            let reply = PromptReply {
                assistant_text: summary.assistant_text,
                turns: summary.turns,
            };
            // PromptReply is always serializable (plain strings + u32).
            #[allow(clippy::expect_used)]
            let bytes = serde_json::to_vec(&reply).expect("PromptReply is always serializable");
            {
                let mut g = conn.lock().await;
                if let Err(e) = g.write_frame(FrameKind::Response, &bytes).await {
                    error!(error = %e, "write reply");
                    return false;
                }
            }
            persist(session_store, &session);
            true
        }
        Err(e) => {
            let _ = conn
                .lock()
                .await
                .write_frame(FrameKind::ErrorFrame, format!("loop error: {e}").as_bytes())
                .await;
            true
        }
    }
}

fn persist(session_store: &SessionStore, session: &Session) {
    if let Err(e) = session_store.persist_session(session) {
        error!(error = %e, "persist_session failed");
    }
    for (i, m) in session.messages.iter().enumerate() {
        #[allow(clippy::expect_used)]
        // Turn count in a session cannot exceed u32::MAX in practice.
        let turn = u32::try_from(i).expect("turn fits u32");
        if let Err(e) = session_store.persist_message(&session.id.to_string(), turn, m) {
            error!(error = %e, "persist_message failed");
        }
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

fn default_db_path() -> String {
    let mut p = std::env::temp_dir();
    p.push("origin.db");
    p.to_string_lossy().into_owned()
}

fn default_cas_root() -> String {
    let mut p = std::env::temp_dir();
    p.push("origin-cas");
    p.to_string_lossy().into_owned()
}
