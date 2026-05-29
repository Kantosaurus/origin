// SPDX-License-Identifier: Apache-2.0
//! Regression tests for the silent-drop bug in `drain_subscriber_into_response`:
//! when a `ToolUseDelta` arrives for an index that was never `ToolUseStart`ed
//! (e.g. because the start payload was malformed and warn-without-inserted),
//! the delta payload is discarded. The original code dropped it without any
//! diagnostic; this test asserts a warn-level log is emitted that names the
//! orphan index so the loss is at least observable in operator logs.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::module_name_repetitions,
    clippy::use_self
)]

use async_trait::async_trait;
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::session::Session;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use origin_stream::{Ring, TokenEvent, TokenKind};
use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;

/// A `MakeWriter` that appends every formatted log line to a shared buffer.
/// Mirrors `tracing_subscriber::fmt::with_test_writer` but works in an
/// integration test (the test_writer helper is only usable from `cargo test`
/// when the subscriber's `with_test_writer` thread-local is set up — using an
/// explicit `MakeWriter` is the portable form).
#[derive(Clone, Default)]
struct BufWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for BufWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().expect("lock").extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for BufWriter {
    type Writer = BufWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Provider whose `chat_stream` publishes a single `ToolUseDelta` with an
/// `index` that was never opened via `ToolUseStart`. Mirrors the production
/// shape where the start payload was malformed and the warn-without-insert
/// path at agent.rs:1997 ran instead of the happy-path insert.
struct OrphanDeltaProvider {
    orphan_index: u32,
    delta_bytes: Vec<u8>,
}

#[async_trait]
impl Provider for OrphanDeltaProvider {
    fn name(&self) -> &'static str {
        "orphan-delta"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Api("non-stream path should not be hit".into()))
    }
    async fn chat_stream(&self, _req: ChatRequest, ring: &Ring) -> Result<(), ProviderError> {
        // Build payload: 4-byte LE index || json_bytes.
        let mut payload = Vec::with_capacity(4 + self.delta_bytes.len());
        payload.extend_from_slice(&self.orphan_index.to_le_bytes());
        payload.extend_from_slice(&self.delta_bytes);
        ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, payload))
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
            .map_err(|e| ProviderError::Api(e.to_string()))?;
        ring.close();
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn orphan_tool_use_delta_emits_warn_with_index() {
    let writer = BufWriter::default();
    let buf_handle = writer.buf.clone();

    // Install a tracing subscriber that captures WARN+ to our buffer. Use a
    // dispatch guard so we don't pollute other tests' tracing state.
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::WARN)
        .with_writer(writer)
        .with_ansi(false)
        .without_time()
        .finish();
    let _dispatch_guard = tracing::subscriber::set_default(subscriber);

    let orphan_index: u32 = 0xDEAD_BEEF;
    let delta_bytes = br#"{"file_path":"/etc/passwd"}"#.to_vec();

    let provider = OrphanDeltaProvider {
        orphan_index,
        delta_bytes: delta_bytes.clone(),
    };
    let mut session = Session::new("test", "claude-opus-4-7");
    let summary = run_loop(
        &mut session,
        "hello",
        &provider,
        &AlwaysAllow,
        &LoopOptions::default(),
    )
    .await
    .expect("run_loop must succeed (no real tool_use blocks emerge)");
    // No assistant text was emitted, no tool calls fired.
    assert_eq!(summary.assistant_text, "");
    assert_eq!(summary.turns, 1);

    let captured = String::from_utf8(buf_handle.lock().expect("lock").clone()).expect("utf8 logs");
    // The warn must (a) be at WARN level and (b) name the orphan index in
    // some form so operators can correlate. We accept either decimal or hex.
    let mentions_index = captured.contains(&orphan_index.to_string()) || captured.contains("deadbeef");
    assert!(
        captured.contains("WARN"),
        "expected a WARN-level log entry; captured:\n{captured}"
    );
    assert!(
        mentions_index,
        "expected the warn to mention the orphan index {orphan_index} ({orphan_index:#x}); captured:\n{captured}"
    );
    // And the bytes length should be reported so operators can gauge how
    // much input was lost.
    assert!(
        captured.contains(&delta_bytes.len().to_string()),
        "expected the warn to mention the dropped payload bytes-len {}; captured:\n{captured}",
        delta_bytes.len()
    );
}
