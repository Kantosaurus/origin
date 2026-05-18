use std::env;

use anyhow::Result;
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut client = Connector::connect(&path).await?;
    let req = encode(1, FrameKind::Request, b"hello");
    client.write_raw(&req).await?;
    let body = client.read_frame_body().await?;
    println!("daemon said: {}", String::from_utf8_lossy(&body));
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
