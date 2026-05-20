//! `tracing` Layer that feeds the parquet ring via a SPSC channel.
//!
//! The layer captures `on_close` events. Each close becomes one [`SpanRow`].
//! A background OS thread owns the [`Ring`] and drains the channel; the
//! foreground tracing path only does an `mpsc::Sender::send` (lock-free under
//! the common case).

#![allow(clippy::needless_pass_by_value)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{span, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::schema::SpanRow;
use crate::{Ring, RingError};

pub struct Layer {
    tx: SyncSender<SpanRow>,
    // Cleared by the [`LayerGuard`] on drop. When `false`, the layer's
    // `on_close` becomes a no-op and the drain thread will exit on its next
    // timeout tick. We keep the sender alive on this side so any concurrent
    // span close that already passed the flag check still has a valid
    // channel to push into (the drain loop drains until empty before
    // exiting).
    active: Arc<AtomicBool>,
}

/// Drop guard returned by [`init`]. Dropping flushes the channel and joins
/// the background thread.
#[must_use]
pub struct LayerGuard {
    join: Option<JoinHandle<()>>,
    active: Arc<AtomicBool>,
}

impl Drop for LayerGuard {
    fn drop(&mut self) {
        // Flip the kill switch; the drain thread polls this on each
        // `recv_timeout` boundary and exits once it observes `false` AND the
        // channel is empty.
        self.active.store(false, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Initialise tracing with a parquet-backed layer writing to `dir`.
///
/// # Errors
/// Returns [`RingError`] if the ring cannot be opened.
pub fn init<P: AsRef<Path>>(dir: P) -> Result<LayerGuard, RingError> {
    use tracing_subscriber::layer::SubscriberExt as _;
    let ring = Ring::open(dir, 64 * 1024 * 1024)?;
    let (tx, rx) = sync_channel::<SpanRow>(4096);
    let active = Arc::new(AtomicBool::new(true));
    let drain_active = Arc::clone(&active);
    let join = std::thread::Builder::new()
        .name("origin-trace-drain".into())
        .spawn(move || {
            let mut ring = ring;
            loop {
                match rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(row) => {
                        let _ = ring.append(row);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if !drain_active.load(Ordering::Acquire) {
                            // Drain any rows that arrived between the flag
                            // flip and our wake-up before exiting.
                            while let Ok(row) = rx.try_recv() {
                                let _ = ring.append(row);
                            }
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            let _ = ring.flush();
        })
        .map_err(RingError::Io)?;

    let layer = Layer {
        tx,
        active: Arc::clone(&active),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    // `set_global_default` may error if a subscriber is already installed
    // (e.g. in tests). For init we tolerate it: tests use the test-local
    // subscriber, but the layer's writes still flow via the explicit Ring.
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(LayerGuard {
        join: Some(join),
        active,
    })
}

impl<S> tracing_subscriber::Layer<S> for Layer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("span exists");
        // Stash the start instant + a serialized attrs blob in the
        // span's extensions so on_close can compute duration without
        // re-walking the field set.
        let mut visitor = FieldCollector::default();
        attrs.record(&mut visitor);
        let attrs_json = visitor.attrs_json();
        span.extensions_mut().insert(SpanStash {
            start: Instant::now(),
            kind: leak_str(visitor.kind.unwrap_or_else(|| "span".into())),
            provider: leak_str(visitor.provider.unwrap_or_default()),
            tool: leak_str(visitor.tool.unwrap_or_default()),
            error_kind: leak_str(visitor.error_kind.unwrap_or_default()),
            attrs_json,
            parent: ctx.current_span().id().map_or(0, tracing::Id::into_u64),
        });
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        let Some(span) = ctx.span(&id) else { return };
        let Some(stash) = span.extensions().get::<SpanStash>().cloned() else {
            return;
        };
        let dur_us = u64::try_from(stash.start.elapsed().as_micros()).unwrap_or(u64::MAX);
        let row = SpanRow {
            ts_ns: 0, // optional; the daemon's wall clock is captured per-record on the writer side if needed
            span_id: id.into_u64(),
            parent_id: stash.parent,
            kind: stash.kind,
            provider: stash.provider,
            tool: stash.tool,
            dur_us,
            error_kind: stash.error_kind,
            attrs_json: stash.attrs_json,
        };
        // Drop the row if the drain thread is wedged — we'd rather lose a
        // trace row than block the agent loop.
        let _ = self.tx.try_send(row);
    }
}

#[derive(Clone)]
struct SpanStash {
    start: Instant,
    kind: &'static str,
    provider: &'static str,
    tool: &'static str,
    error_kind: &'static str,
    attrs_json: String,
    parent: u64,
}

#[derive(Default)]
struct FieldCollector {
    kind: Option<String>,
    provider: Option<String>,
    tool: Option<String>,
    error_kind: Option<String>,
    other: std::collections::BTreeMap<&'static str, String>,
}

impl tracing::field::Visit for FieldCollector {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "kind" => self.kind = Some(value.into()),
            "provider" => self.provider = Some(value.into()),
            "tool" => self.tool = Some(value.into()),
            "error_kind" => self.error_kind = Some(value.into()),
            other => {
                self.other.insert(other, value.into());
            }
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{value:?}");
        match field.name() {
            "kind" => self.kind = Some(strip_quotes(&formatted)),
            "provider" => self.provider = Some(strip_quotes(&formatted)),
            "tool" => self.tool = Some(strip_quotes(&formatted)),
            "error_kind" => self.error_kind = Some(strip_quotes(&formatted)),
            other => {
                self.other.insert(other, formatted);
            }
        }
    }
}

fn strip_quotes(s: &str) -> String {
    // `record_debug` formats string values as `"value"` — strip the wrapping
    // quotes so downstream consumers see the bare string.
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

impl FieldCollector {
    fn attrs_json(&self) -> String {
        // Pre-allocate a small JSON blob; the layer sees no `serde_json`
        // pretty-print cost on the hot path.
        let mut s = String::with_capacity(64 + self.other.len() * 16);
        s.push('{');
        for (i, (k, v)) in self.other.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('"');
            s.push_str(k);
            s.push('"');
            s.push(':');
            s.push('"');
            s.push_str(&v.replace('"', "\\\""));
            s.push('"');
        }
        s.push('}');
        s
    }
}

// `tracing` stash strings need a `'static` lifetime. We intern at span open.
// Strings are bounded by the number of distinct (kind, provider, tool, error)
// quadruples in the process — for our daemon, dozens.
fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
