use std::env;

use anyhow::Result;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::{PromptReply, PromptRequest};
use origin_daemon::session::Session;
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

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    loop {
        let mut conn = listener.accept().await?;
        let provider = std::sync::Arc::clone(&provider);
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
