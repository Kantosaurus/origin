//! Shared SSE pump for provider crates.
//!
//! Wraps a `reqwest::Response` byte stream into an event stream yielding
//! `Result<eventsource_stream::Event, ProviderError>`. Provider crates layer
//! their own JSON parsing on top.

use crate::ProviderError;
use eventsource_stream::{Event, Eventsource};
use futures_util::{Stream, StreamExt};

/// Adapt a `reqwest::Response` body into an SSE event stream.
///
/// The returned stream yields `Result<Event, ProviderError>` for each SSE
/// frame parsed out of the chunked body. Callers should pin the stream
/// (`pin_utils::pin_mut!` or `Box::pin`) before polling.
///
/// # Errors
/// Each yielded item is `Err(ProviderError::Api)` if the SSE parser encounters
/// a malformed frame.
pub fn from_reqwest(resp: reqwest::Response) -> impl Stream<Item = Result<Event, ProviderError>> + Send {
    let byte_stream = resp
        .bytes_stream()
        .map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
    let async_read = tokio_util::io::StreamReader::new(byte_stream);
    tokio_util::io::ReaderStream::new(async_read)
        .eventsource()
        .map(|r| r.map_err(|e| ProviderError::Api(format!("sse: {e}"))))
}
