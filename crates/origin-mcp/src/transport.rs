//! Transport abstraction. Stdio + HTTP/SSE both implement this.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("transport: {0}")]
    Other(String),
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Send `request_json` and return the matching response JSON.
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError>;
}
