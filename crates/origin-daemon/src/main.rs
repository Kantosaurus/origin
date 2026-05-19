use std::env;

use anyhow::Result;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::{PromptReply, PromptRequest};
use origin_daemon::session::Session;
use origin_daemon::session_store::SessionStore;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::Listener;
use origin_permission::prompt::AlwaysAllow;
use origin_provider_anthropic::Anthropic;
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
    let provider = std::sync::Arc::new(Anthropic::new(api_key));

    let db_path = env::var("ORIGIN_DB").unwrap_or_else(|_| default_db_path());
    let session_store = std::sync::Arc::new(SessionStore::open(&db_path)?);
    info!(db = %db_path, "session store ready");

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    loop {
        let mut conn = listener.accept().await?;
        let provider = std::sync::Arc::clone(&provider);
        let session_store = std::sync::Arc::clone(&session_store);
        tokio::spawn(async move {
            loop {
                let Ok(body) = conn.read_frame_body().await else {
                    break;
                };
                let req: PromptRequest = match serde_json::from_slice(&body) {
                    Ok(r) => r,
                    Err(e) => {
                        error!(error = %e, "bad prompt request");
                        let _ = conn
                            .write_frame(FrameKind::ErrorFrame, format!("bad request: {e}").as_bytes())
                            .await;
                        continue;
                    }
                };
                let mut session = Session::new("anthropic", &req.model);
                match run_loop(
                    &mut session,
                    &req.user_text,
                    provider.as_ref(),
                    &AlwaysAllow,
                    LoopOptions::default(),
                )
                .await
                {
                    Ok(summary) => {
                        let reply = PromptReply {
                            assistant_text: summary.assistant_text,
                            turns: summary.turns,
                        };
                        // PromptReply is always serializable (plain strings + u32).
                        #[allow(clippy::expect_used)]
                        let bytes = serde_json::to_vec(&reply).expect("PromptReply is always serializable");
                        if let Err(e) = conn.write_frame(FrameKind::Response, &bytes).await {
                            error!(error = %e, "write reply");
                            break;
                        }
                        // Persist session metadata and all messages.
                        if let Err(e) = session_store.persist_session(&session) {
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
                    Err(e) => {
                        let _ = conn
                            .write_frame(FrameKind::ErrorFrame, format!("loop error: {e}").as_bytes())
                            .await;
                    }
                }
            }
        });
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
