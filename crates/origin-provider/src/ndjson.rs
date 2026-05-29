// SPDX-License-Identifier: Apache-2.0
//! Shared NDJSON line splitter for provider crates.
//!
//! Wraps a `reqwest::Response` byte stream into a stream of UTF-8 lines
//! (one JSON object per line, newline-delimited). Provider crates layer
//! their own JSON parsing on top.

use crate::ProviderError;
use async_stream::try_stream;
use futures_util::{Stream, StreamExt};

/// Adapt a `reqwest::Response` body into a stream of NDJSON lines.
///
/// Each yielded item is one complete line (without the trailing `\n`),
/// parsed as UTF-8. Empty lines are skipped. Any trailing bytes after the
/// final `\n` (or the entire body if it lacks a final newline) are flushed
/// as one last line if non-empty.
///
/// # Errors
/// Yields `ProviderError::Transport` if the underlying HTTP byte stream
/// errors, or `ProviderError::Api` if a line is not valid UTF-8.
pub fn from_reqwest(resp: reqwest::Response) -> impl Stream<Item = Result<String, ProviderError>> + Send {
    try_stream! {
        let mut bytes = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
            buf.extend_from_slice(&chunk);

            // Drain complete lines.
            loop {
                let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
                    break;
                };
                // Take bytes [0, nl) as the line; drop the trailing '\n'.
                let mut line: Vec<u8> = buf.drain(..=nl).collect();
                line.pop(); // remove '\n'
                // Drop a trailing '\r' so CRLF-terminated streams yield clean
                // lines; otherwise a blank "\r\n" line becomes "\r", which is
                // non-empty and fails downstream JSON parsing.
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                let s = String::from_utf8(line)
                    .map_err(|e| ProviderError::Api(format!("ndjson: invalid utf-8: {e}")))?;
                yield s;
            }
        }

        // Flush any trailing bytes that lack a terminating newline.
        if !buf.is_empty() {
            let s = String::from_utf8(buf)
                .map_err(|e| ProviderError::Api(format!("ndjson: invalid utf-8: {e}")))?;
            if !s.is_empty() {
                yield s;
            }
        }
    }
}
