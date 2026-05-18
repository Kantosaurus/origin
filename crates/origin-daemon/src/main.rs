use std::env;

use anyhow::Result;
use origin_ipc::frame::FrameKind;
use origin_ipc::transport::Listener;
use tracing::{error, info};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let listener = Listener::bind(&path).await?;
    info!(path = %path, "origin-daemon listening");

    loop {
        let mut conn = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "accept failed; continuing");
                continue;
            }
        };
        tokio::spawn(async move {
            loop {
                let Ok(body) = conn.read_frame_body().await else {
                    break; // client disconnected
                };
                if let Err(e) = conn.write_frame(FrameKind::Response, &body).await {
                    error!(error = %e, "write failed");
                    break;
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
