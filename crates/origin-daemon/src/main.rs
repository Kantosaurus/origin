use std::env;
use std::sync::Arc;

use anyhow::Result;
use origin_cas::Store;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::{PromptReply, PromptRequest};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
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
        let (tx_sub, rx_sub) = mpsc::channel::<Subscriber>(1);

        spawn_relay_task(Arc::clone(&shared_conn), rx_sub);
        spawn_handler_task(
            shared_conn,
            tx_sub,
            Arc::clone(&provider),
            Arc::clone(&session_store),
            Arc::clone(&cas),
        );
    }
}

fn spawn_relay_task(conn: SharedConnection, mut rx_sub: mpsc::Receiver<Subscriber>) {
    tokio::spawn(async move {
        while let Some(sub) = rx_sub.recv().await {
            if let Err(e) = origin_daemon::stream_relay::relay_to_connection(sub, Arc::clone(&conn)).await {
                error!(error = %e, "relay terminated");
                break;
            }
        }
    });
}

fn spawn_handler_task(
    conn: SharedConnection,
    tx_sub: mpsc::Sender<Subscriber>,
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
            let req: PromptRequest = match serde_json::from_slice(&body) {
                Ok(r) => r,
                Err(e) => {
                    error!(error = %e, "bad prompt request");
                    let _ = conn
                        .lock()
                        .await
                        .write_frame(FrameKind::ErrorFrame, format!("bad request: {e}").as_bytes())
                        .await;
                    continue;
                }
            };
            if !handle_request(
                &conn,
                &tx_sub,
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
    });
}

/// Run one request to completion. Returns `false` if the response write
/// failed (the connection is dead and the handler task should exit).
async fn handle_request(
    conn: &SharedConnection,
    tx_sub: &mpsc::Sender<Subscriber>,
    provider: &dyn Provider,
    session_store: &SessionStore,
    cas: Arc<Store>,
    req: PromptRequest,
) -> bool {
    let mut session = Session::new("anthropic", &req.model);
    let opts = LoopOptions {
        max_turns: 25,
        cas: Some(cas),
        relay_tx: Some(tx_sub.clone()),
        streaming_disabled: false,
    };
    match run_loop(&mut session, &req.user_text, provider, &AlwaysAllow, &opts).await {
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
