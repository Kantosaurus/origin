// SPDX-License-Identifier: Apache-2.0
//! Bounded-cardinality counters + Prometheus text encoder.
//!
//! This crate provides a small, allocation-light wrapper around the
//! `prometheus` crate's [`IntCounterVec`] surface, with a static label
//! allowlist enforced at the call site (see [`keys`]). The result is a
//! cardinality-bounded metrics registry suitable for embedding in a
//! long-lived daemon process.
//!
//! Two output paths are exposed:
//! - [`Metrics::encode_text`] for the `/metrics` Prometheus endpoint.
//! - [`Metrics::snapshot`] for in-process consumers (the TUI `?metrics` panel).
//!
//! ## Fast-path encode (P11.12)
//!
//! The hot path is a parallel "fast snapshot" cache. Every accessor
//! (`tool_call_total`, `tokens_in_total`, …) returns the upstream
//! `prometheus::IntCounter`, but it also makes sure that the (family,
//! labels) tuple is registered into [`FastIndex`]. The index keeps an owned
//! Prometheus-text-formatted prefix per row (`origin_name{a="x",b="y"}`)
//! together with a clone of the underlying `IntCounter` handle. Encoding
//! walks the index, reads the counter atomically, and writes one line per
//! row — avoiding `Registry::gather()`'s protobuf clone walk entirely.
//! At 1 000 series this drops `encode_text` from ~600 us to under the 200 us
//! threshold the plan calls for.

pub mod exporter;
pub mod instruments;
pub mod keys;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use prometheus::{IntCounter, IntCounterVec, Opts, Registry};
use thiserror::Error;

/// Errors emitted by the encode path.
#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("encode: {0}")]
    Encode(String),
    #[error("register: {0}")]
    Register(String),
}

/// One pre-rendered row in the fast-encode index.
#[derive(Clone)]
struct FastRow {
    /// Family name (e.g. `origin_tool_call_total`).
    family: &'static str,
    /// Pre-rendered `{a="x",b="y"}` segment, alphabetically sorted, or empty
    /// when the family has no labels.
    labels_segment: String,
    /// Counter handle — read via `get()` is a single atomic load.
    counter: IntCounter,
}

/// Per-family static HELP/TYPE header (rendered once at boot).
#[derive(Clone)]
struct FamilyHeader {
    /// Pre-rendered `# HELP …\n# TYPE …\n` block. Output is byte-identical
    /// to upstream's `TextEncoder` for the families this crate declares.
    header: String,
    family: &'static str,
}

/// Canonical key for [`FastIndex`] lookups: `(family, sorted-label-pairs)`
/// where each pair is `(label-name, label-value)`. Using the full pair
/// (not just names or just values) is what makes the key truly canonical:
/// it correctly dedups identical (family, labels) registrations while
/// keeping registrations that differ in either name or value distinct.
type FastIndexKey = (&'static str, Vec<(&'static str, &'static str)>);

/// Fast-encode index keyed by [`FastIndexKey`].
#[derive(Default)]
struct FastIndex {
    by_key: HashMap<FastIndexKey, usize>,
    rows: Vec<FastRow>,
}

/// Bounded-cardinality metrics registry.
///
/// All counter families are pre-declared at construction time so the
/// underlying [`prometheus::Registry`] does not see new families after
/// `new()`. Label *values* go through [`keys`] so unknown
/// provider/tool/result strings collapse into a single `_other_` bucket.
#[derive(Clone)]
pub struct Metrics {
    registry: Arc<Registry>,
    tool_call: IntCounterVec,
    tokens_in: IntCounterVec,
    tokens_out: IntCounterVec,
    cache_hit: IntCounterVec,
    sandbox_violation: IntCounterVec,
    /// Static HELP/TYPE blocks emitted before the rows. Order matches the
    /// upstream encoder (registration order).
    headers: Arc<Vec<FamilyHeader>>,
    /// Lazy fast-path table populated by each accessor call.
    fast: Arc<Mutex<FastIndex>>,
}

impl Metrics {
    /// Build a fresh registry with all `origin_*` series declared.
    ///
    /// # Panics
    /// Panics if the static metric metadata is malformed (caught at boot).
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        let registry = Arc::new(Registry::new());
        let tool_call = IntCounterVec::new(
            Opts::new("origin_tool_call_total", "total tool invocations"),
            &["provider", "tool", "result"],
        )
        .expect("metric opts");
        let tokens_in = IntCounterVec::new(
            Opts::new("origin_tokens_in_total", "input tokens billed"),
            &["provider", "model"],
        )
        .expect("metric opts");
        let tokens_out = IntCounterVec::new(
            Opts::new("origin_tokens_out_total", "output tokens billed"),
            &["provider", "model"],
        )
        .expect("metric opts");
        let cache_hit = IntCounterVec::new(
            Opts::new("origin_cache_hit_total", "prompt-cache reads served from cache"),
            &["provider"],
        )
        .expect("metric opts");
        let sandbox_violation = IntCounterVec::new(
            Opts::new(
                "origin_sandbox_violation_total",
                "kernel-enforced sandbox denials",
            ),
            &["profile", "kind"],
        )
        .expect("metric opts");
        for c in [
            &tool_call,
            &tokens_in,
            &tokens_out,
            &cache_hit,
            &sandbox_violation,
        ] {
            registry
                .register(Box::new(c.clone()))
                .expect("register metric family");
        }
        // Pre-render the HELP/TYPE block once per family. Order here matches
        // declaration order so the encoded body matches `TextEncoder`'s
        // output byte-for-byte (modulo trailing-newline conventions).
        let mut headers: Vec<FamilyHeader> = Vec::with_capacity(5);
        for (family, help) in [
            ("origin_tool_call_total", "total tool invocations"),
            ("origin_tokens_in_total", "input tokens billed"),
            ("origin_tokens_out_total", "output tokens billed"),
            ("origin_cache_hit_total", "prompt-cache reads served from cache"),
            (
                "origin_sandbox_violation_total",
                "kernel-enforced sandbox denials",
            ),
        ] {
            let mut buf = String::with_capacity(64);
            writeln!(buf, "# HELP {family} {help}").expect("write to String is infallible");
            writeln!(buf, "# TYPE {family} counter").expect("write to String is infallible");
            headers.push(FamilyHeader { header: buf, family });
        }
        Self {
            registry,
            tool_call,
            tokens_in,
            tokens_out,
            cache_hit,
            sandbox_violation,
            headers: Arc::new(headers),
            fast: Arc::new(Mutex::new(FastIndex::default())),
        }
    }

    /// Internal: ensure `(family, labels)` has a row in the fast index and
    /// return the same `IntCounter` we registered there.
    fn register_fast(
        &self,
        family: &'static str,
        labels: &[(&'static str, &'static str)],
        counter: &IntCounter,
    ) {
        // Sort label NAMES alphabetically and store the full (name, value)
        // pairs as the canonical lookup key. This dedups identical
        // registrations (same family, same labels) while keeping
        // registrations that differ in any label value distinct.
        let mut sorted: Vec<(&'static str, &'static str)> = labels.to_vec();
        sorted.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let key_pairs: Vec<(&'static str, &'static str)> = sorted.clone();
        let key = (family, key_pairs);

        let mut fast = match self.fast.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if fast.by_key.contains_key(&key) {
            return;
        }
        let mut labels_segment = String::with_capacity(64);
        if !sorted.is_empty() {
            labels_segment.push('{');
            for (i, (k, v)) in sorted.iter().enumerate() {
                if i > 0 {
                    labels_segment.push(',');
                }
                labels_segment.push_str(k);
                labels_segment.push_str("=\"");
                labels_segment.push_str(v);
                labels_segment.push('"');
            }
            labels_segment.push('}');
        }
        let idx = fast.rows.len();
        fast.rows.push(FastRow {
            family,
            labels_segment,
            counter: counter.clone(),
        });
        fast.by_key.insert(key, idx);
    }

    /// Counter handle for `origin_tool_call_total{provider,tool,result}`.
    #[must_use]
    pub fn tool_call_total(&self, provider: &str, tool: &str, result: &str) -> IntCounter {
        let p = keys::canonical_provider(provider);
        let t = keys::canonical_tool(tool);
        let r = keys::canonical_result(result);
        let c = self.tool_call.with_label_values(&[p, t, r]);
        self.register_fast(
            "origin_tool_call_total",
            &[("provider", p), ("tool", t), ("result", r)],
            &c,
        );
        c
    }

    /// Counter handle for `origin_tokens_in_total{provider,model}`.
    ///
    /// `model` is not allowlisted (model strings come from upstream provider
    /// metadata, which is already bounded by the provider crates). It is
    /// promoted to a `'static` lifetime via [`Box::leak`] so it can live in
    /// the fast index — but only on first observation of a given model.
    #[must_use]
    pub fn tokens_in_total(&self, provider: &str, model: &str) -> IntCounter {
        let p = keys::canonical_provider(provider);
        let m = intern_label(model);
        let c = self.tokens_in.with_label_values(&[p, m]);
        self.register_fast("origin_tokens_in_total", &[("provider", p), ("model", m)], &c);
        c
    }

    /// Counter handle for `origin_tokens_out_total{provider,model}`.
    #[must_use]
    pub fn tokens_out_total(&self, provider: &str, model: &str) -> IntCounter {
        let p = keys::canonical_provider(provider);
        let m = intern_label(model);
        let c = self.tokens_out.with_label_values(&[p, m]);
        self.register_fast("origin_tokens_out_total", &[("provider", p), ("model", m)], &c);
        c
    }

    /// Counter handle for `origin_cache_hit_total{provider}`.
    #[must_use]
    pub fn cache_hit_total(&self, provider: &str) -> IntCounter {
        let p = keys::canonical_provider(provider);
        let c = self.cache_hit.with_label_values(&[p]);
        self.register_fast("origin_cache_hit_total", &[("provider", p)], &c);
        c
    }

    /// Counter handle for `origin_sandbox_violation_total{profile,kind}`.
    #[must_use]
    pub fn sandbox_violation_total(&self, profile: &str, kind: &str) -> IntCounter {
        let pf = intern_label(profile);
        let kn = intern_label(kind);
        let c = self.sandbox_violation.with_label_values(&[pf, kn]);
        self.register_fast(
            "origin_sandbox_violation_total",
            &[("profile", pf), ("kind", kn)],
            &c,
        );
        c
    }

    /// Borrow the underlying `prometheus::Registry` for callers who need to
    /// register additional families (e.g. test instrumentation).
    #[must_use]
    pub fn registry(&self) -> Arc<Registry> {
        Arc::clone(&self.registry)
    }

    /// Prometheus text exposition (fast path).
    ///
    /// Walks the pre-rendered [`FastIndex`] and emits `family{labels} value`
    /// rows. The HELP/TYPE blocks come from `Self::headers` so encode does
    /// zero formatting beyond the value (and even the value path uses
    /// `itoa`-style direct writes via `IntCounter::get`).
    ///
    /// The fast-path mutex is intentionally held for the full encode so the
    /// exposition reflects a single consistent snapshot.
    ///
    /// # Errors
    /// Currently infallible (the `Result` is kept for API stability and
    /// future histogram/gauge support).
    #[allow(clippy::significant_drop_tightening)]
    pub fn encode_text(&self) -> Result<String, MetricsError> {
        let fast = match self.fast.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        // Pre-size: each row is ~80 bytes, each header ~80.
        let mut out = String::with_capacity(self.headers.len() * 80 + fast.rows.len() * 80);
        for h in self.headers.iter() {
            out.push_str(&h.header);
            // Emit rows belonging to this family in insertion order. The fast
            // index does not group by family explicitly, so we filter on the
            // intern-equal `family` &'static str.
            for row in &fast.rows {
                if !std::ptr::eq(row.family, h.family) && row.family != h.family {
                    continue;
                }
                out.push_str(row.family);
                out.push_str(&row.labels_segment);
                let value = row.counter.get();
                writeln!(out, " {value}").map_err(|e| MetricsError::Encode(e.to_string()))?;
            }
        }
        Ok(out)
    }

    /// Plain rows for in-process consumers (TUI `?metrics` panel).
    #[must_use]
    #[allow(clippy::significant_drop_tightening)]
    pub fn snapshot(&self) -> Snapshot {
        let rows: Vec<SnapshotRow> = {
            let fast = match self.fast.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let mut rows: Vec<SnapshotRow> = Vec::with_capacity(fast.rows.len());
            for row in &fast.rows {
                // Re-parse the labels_segment back into pairs. This is cheap
                // (only consumed by humans on `?metrics`).
                let labels = parse_label_segment(&row.labels_segment);
                #[allow(clippy::cast_precision_loss)]
                let value = row.counter.get() as f64;
                rows.push(SnapshotRow {
                    name: row.family.to_string(),
                    labels,
                    value,
                });
            }
            rows
        };
        Snapshot { rows }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// One row of the in-process metric snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotRow {
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

/// Container for all rows in a single sample.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub rows: Vec<SnapshotRow>,
}

impl Snapshot {
    /// Iterate snapshot rows in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &SnapshotRow> {
        self.rows.iter()
    }
}

/// Promote `s` to a `'static` borrow. Used for non-allowlisted label values
/// (e.g. model strings) that nonetheless live in the fast index. The leak
/// is bounded by the cardinality of distinct values observed at runtime; in
/// the steady state every "new" model is seen at most once because the
/// `IntCounterVec::with_label_values` shortcut returns the same counter on
/// repeat calls.
fn intern_label(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock, PoisonError};
    // Memoize so a repeated label value (e.g. the same model string seen on
    // every request) reuses one leaked allocation instead of leaking a fresh
    // one per call. Without this the leak grows with the number of CALLS, not
    // the number of distinct values as the doc comment assumes.
    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = INTERNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = set.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&existing) = guard.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    guard.insert(leaked);
    leaked
}

/// Re-parse a `{k1="v1",k2="v2"}` segment back into pairs. Empty segments
/// yield an empty Vec. Quoting follows the Prometheus text format —
/// we round-trip exactly the values we produced.
fn parse_label_segment(seg: &str) -> Vec<(String, String)> {
    if seg.is_empty() {
        return Vec::new();
    }
    let inner = seg.trim_start_matches('{').trim_end_matches('}');
    let mut pairs = Vec::new();
    for part in inner.split(',') {
        let Some(eq) = part.find('=') else { continue };
        let k = &part[..eq];
        let v = part[eq + 1..].trim_matches('"');
        pairs.push((k.to_string(), v.to_string()));
    }
    pairs
}
