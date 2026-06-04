// SPDX-License-Identifier: Apache-2.0
//! Real OpenTelemetry `GenAI` metric instruments + a cheap, always-callable
//! recording API.
//!
//! ## What this module is for
//!
//! [`crate::exporter`] installs an OTLP metrics pipeline and a global
//! [`MeterProvider`], but nothing in origin actually *records* anything to it —
//! the daemon's hot path only touches the Prometheus [`crate::Metrics`]
//! counters. So the exported OTLP carries no application data under the
//! OpenTelemetry `GenAI` semantic conventions. This module closes that gap.
//!
//! It builds a small, fixed set of agent-relevant `gen_ai.*` instruments on the
//! installed meter and exposes [`record_gen_ai_usage`] — a single call the
//! daemon makes at the turn-completion emit site. The recording fans a turn's
//! token usage, latency and tool-call count out to those instruments with the
//! correct `gen_ai.*` attributes.
//!
//! ## Novel bit: pure attribute relabeling, side-effects behind a `OnceLock`
//!
//! The attribute set is built by [`gen_ai_attributes`], a **pure** function that
//! routes origin's internal label names (`provider`, `model`, …) through
//! [`crate::keys::gen_ai_attr_for_label`] so the convention key is *derived*,
//! never hand-typed at the call site. That keeps the relabel map
//! ([`crate::keys`]) the single source of truth and makes the whole thing
//! unit-testable with zero I/O: a test asserts the [`KeyValue`] set directly
//! rather than standing up an exporter.
//!
//! The instruments themselves live behind a process-global handle that
//! [`crate::exporter::otel::install`] re-binds after it sets the global meter
//! provider, so instruments always attach to the *real* provider regardless of
//! call ordering. Before install (or when the `otel` feature is off) every
//! recording is a no-op, so the daemon can call [`record_gen_ai_usage`]
//! unconditionally.

// ---------------------------------------------------------------------------
// otel-disabled path: zero-cost stubs so the daemon links + calls unchanged.
// The default (non-otel) build sees only this and is byte-identical to before
// modulo this `#[cfg]`-gated public function existing.
// ---------------------------------------------------------------------------

/// Record one turn's `GenAI` usage to the OpenTelemetry instruments.
///
/// This is the single call the daemon makes at its turn-completion emit site.
/// It fans the turn's measurements out to the `gen_ai.*` instruments built on
/// the installed meter, attributing each with `gen_ai.system` /
/// `gen_ai.request.model` / `gen_ai.response.model` (relabeled via
/// [`crate::keys`]).
///
/// When the `otel` feature is disabled — or when the exporter has not been
/// installed yet — this is a cheap no-op, so the daemon may call it
/// unconditionally on every turn.
///
/// `latency_ms` is the end-to-end client operation duration in milliseconds;
/// `tool_calls` is the number of model-issued tool calls in the turn.
#[cfg(not(feature = "otel"))]
#[inline]
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    clippy::module_name_repetitions
)]
pub const fn record_gen_ai_usage(
    _system: &str,
    _request_model: &str,
    _response_model: &str,
    _input_tokens: u64,
    _output_tokens: u64,
    _latency_ms: f64,
    _tool_calls: u64,
) {
}

/// Record the time-to-first-token (TTFT) for one turn to the
/// `gen_ai.server.time_to_first_token` histogram.
///
/// `ttft_ms` is the latency in milliseconds from request dispatch to the first
/// streamed token. The recording is attributed with `gen_ai.system` /
/// `gen_ai.request.model` and `gen_ai.operation.name` (relabeled via
/// [`crate::keys`]).
///
/// When the `otel` feature is disabled — or when the exporter has not been
/// installed yet — this is a cheap no-op, mirroring [`record_gen_ai_usage`], so
/// the daemon may call it unconditionally on every turn.
#[cfg(not(feature = "otel"))]
#[inline]
#[allow(clippy::module_name_repetitions)]
pub const fn record_time_to_first_token(
    _system: &str,
    _request_model: &str,
    _operation: &str,
    _ttft_ms: f64,
) {
}

/// Record the time-per-output-token (TPOT) for one turn to the
/// `gen_ai.server.time_per_output_token` histogram.
///
/// `tpot_ms` is the average inter-token latency in milliseconds across the
/// turn's generated output. The recording is attributed identically to
/// [`record_time_to_first_token`].
///
/// When the `otel` feature is disabled — or when the exporter has not been
/// installed yet — this is a cheap no-op, mirroring [`record_gen_ai_usage`], so
/// the daemon may call it unconditionally on every turn.
#[cfg(not(feature = "otel"))]
#[inline]
#[allow(clippy::module_name_repetitions)]
pub const fn record_time_per_output_token(
    _system: &str,
    _request_model: &str,
    _operation: &str,
    _tpot_ms: f64,
) {
}

/// Build/re-bind the `GenAI` instruments against the current global meter.
///
/// No-op without the `otel` feature. With the feature this is called by
/// [`crate::exporter::otel::install`] right after the global meter provider is
/// set, so recordings land on the real OTLP pipeline.
#[cfg(not(feature = "otel"))]
#[inline]
#[allow(clippy::module_name_repetitions)]
pub const fn init_instruments() {}

/// Zero-size no-op guard returned by [`gen_ai_span`] in the default build.
///
/// The `otel`-enabled build returns a guard that holds a real OpenTelemetry
/// span and ends it on `Drop`; this stub holds nothing and its `Drop` is a
/// no-op, so the daemon can open a span unconditionally around the provider
/// call with zero cost when the feature is off. It is deliberately *not*
/// `Copy` so that the `otel` and non-`otel` variants share the same move
/// semantics at the call site (the guard is held to scope, then dropped).
///
/// A trivial `Drop` is implemented so an explicit `drop(guard)` at the call
/// site type-checks and reads identically under both feature sets (without it
/// the daemon's explicit-drop would trip `clippy::drop_non_drop` on this
/// zero-size type).
#[cfg(not(feature = "otel"))]
#[derive(Debug, Default)]
#[allow(clippy::module_name_repetitions)]
pub struct GenAiSpanGuard;

#[cfg(not(feature = "otel"))]
impl Drop for GenAiSpanGuard {
    #[inline]
    fn drop(&mut self) {
        // No span exists in the non-otel build; ending it is a no-op.
    }
}

/// Open a `gen_ai` client span around a provider call.
///
/// In the default (non-`otel`) build this returns a zero-size
/// [`GenAiSpanGuard`] whose `Drop` does nothing — no global tracer is touched,
/// so the call is free and the daemon path stays byte-identical. With the
/// `otel` feature it starts a real span on the installed global tracer,
/// attaches `gen_ai.system` / `gen_ai.request.model` / `gen_ai.operation.name`
/// (relabeled via [`crate::keys`]), and ends the span when the returned guard
/// is dropped.
///
/// The returned guard must be bound to a local (`let _span = …`) so it lives
/// for the duration of the provider call; binding to `_` would drop — and thus
/// immediately end — the span.
#[cfg(not(feature = "otel"))]
#[inline]
#[must_use]
#[allow(clippy::module_name_repetitions, clippy::needless_pass_by_value)]
pub const fn gen_ai_span(
    _system: &str,
    _request_model: &str,
    _operation: &str,
) -> GenAiSpanGuard {
    GenAiSpanGuard
}

#[cfg(all(test, not(feature = "otel")))]
mod noop_tests {
    use super::record_gen_ai_usage;

    /// In the default (no-otel) build the new latency record fns are pure
    /// no-ops: calling them links and returns without panicking. This pins the
    /// otel/non-otel split so the daemon can call them unconditionally.
    ///
    /// Inputs are routed through `std::hint::black_box` so the call sites cannot
    /// be const-folded away (which would otherwise make the test body trivially
    /// `const` and trip `clippy::missing_const_for_fn`).
    #[test]
    fn latency_record_fns_are_noops_without_otel() {
        use std::hint::black_box;
        super::record_time_to_first_token(
            black_box("anthropic"),
            black_box("claude-sonnet-4-6"),
            black_box("chat"),
            black_box(12.5),
        );
        super::record_time_per_output_token(
            black_box("openai"),
            black_box("gpt-4o"),
            black_box("chat"),
            black_box(0.8),
        );
        // And the pre-existing usage fn stays a no-op alongside them.
        record_gen_ai_usage(
            black_box("anthropic"),
            black_box("m"),
            black_box("m"),
            black_box(10),
            black_box(20),
            black_box(1.5),
            black_box(2),
        );
    }

    /// In the default (no-otel) build `gen_ai_span` returns a zero-size guard
    /// whose `Drop` is a no-op: opening and dropping it links, runs, and
    /// touches no global tracer. This pins the contract the daemon relies on to
    /// open a span unconditionally around the provider call at zero cost.
    ///
    /// Inputs are routed through `std::hint::black_box` so the call cannot be
    /// const-folded away (which would make the body trivially `const`).
    #[test]
    fn gen_ai_span_is_a_noop_guard_without_otel() {
        use std::hint::black_box;
        let guard = super::gen_ai_span(
            black_box("anthropic"),
            black_box("claude-sonnet-4-6"),
            black_box("chat"),
        );
        // The guard is a unit-like struct; dropping it must be a no-op.
        drop(guard);
        // The Default impl yields the same no-op guard.
        let _default = super::GenAiSpanGuard;
    }
}

// ---------------------------------------------------------------------------
// otel-enabled path: real instruments bound to the global meter.
// ---------------------------------------------------------------------------

#[cfg(feature = "otel")]
#[allow(clippy::module_name_repetitions)]
pub use otel_impl::{
    gen_ai_span, init_instruments, record_gen_ai_usage, record_time_per_output_token,
    record_time_to_first_token, GenAiSpanGuard, Outcome,
};

#[cfg(feature = "otel")]
mod otel_impl {
    use std::sync::RwLock;

    use opentelemetry::metrics::{Counter, Histogram};
    use opentelemetry::{global, KeyValue};

    use crate::keys::{gen_ai_attr_for_label, genai};

    /// Instrumentation-scope name recorded on the meter (the convention asks the
    /// scope to identify the instrumenting library).
    const SCOPE_NAME: &str = "origin-metrics/gen_ai";

    /// The fixed set of agent-relevant `GenAI` instruments, all bound to one
    /// meter from the installed provider.
    ///
    /// We use additive counters for token usage and tool calls (they accumulate
    /// over the process lifetime) and a histogram for latency (the convention's
    /// `gen_ai.client.operation.duration` is a distribution). The instrument
    /// *names* are the `gen_ai.*` convention strings from [`genai`].
    struct GenAiInstruments {
        input_tokens: Counter<u64>,
        output_tokens: Counter<u64>,
        tool_calls: Counter<u64>,
        operation_duration: Histogram<f64>,
        time_to_first_token: Histogram<f64>,
        time_per_output_token: Histogram<f64>,
    }

    impl GenAiInstruments {
        /// Build the instrument set on a meter from the current global provider.
        ///
        /// `try_init` (not `init`) is used so a malformed instrument config
        /// surfaces as an error instead of a panic — the workspace forbids
        /// panics on the non-test path. Any failure leaves the slot empty and
        /// recording stays a no-op rather than taking down the daemon.
        fn build() -> Result<Self, opentelemetry::metrics::MetricsError> {
            let meter = global::meter(SCOPE_NAME);
            let input_tokens = meter
                .u64_counter(genai::USAGE_INPUT_TOKENS)
                .with_description("GenAI prompt (input) tokens billed")
                .with_unit("{token}")
                .try_init()?;
            let output_tokens = meter
                .u64_counter(genai::USAGE_OUTPUT_TOKENS)
                .with_description("GenAI completion (output) tokens billed")
                .with_unit("{token}")
                .try_init()?;
            let tool_calls = meter
                .u64_counter(genai::TOOL_CALL_COUNT)
                .with_description("GenAI model-issued tool calls")
                .with_unit("{call}")
                .try_init()?;
            let operation_duration = meter
                .f64_histogram(genai::OPERATION_DURATION)
                .with_description("GenAI end-to-end client operation duration")
                .with_unit("ms")
                .try_init()?;
            let time_to_first_token = meter
                .f64_histogram(genai::TIME_TO_FIRST_TOKEN)
                .with_description("GenAI latency from request to the first streamed token")
                .with_unit("ms")
                .try_init()?;
            let time_per_output_token = meter
                .f64_histogram(genai::TIME_PER_OUTPUT_TOKEN)
                .with_description("GenAI average inter-token (per-output-token) latency")
                .with_unit("ms")
                .try_init()?;
            Ok(Self {
                input_tokens,
                output_tokens,
                tool_calls,
                operation_duration,
                time_to_first_token,
                time_per_output_token,
            })
        }
    }

    /// Process-global instrument handle, re-bound by [`init_instruments`] after
    /// the exporter installs the real meter provider. `None` until then, which
    /// makes [`record_gen_ai_usage`] a no-op (safe to call before install).
    static INSTRUMENTS: RwLock<Option<GenAiInstruments>> = RwLock::new(None);

    /// Build/re-bind the `GenAI` instruments against the current global meter.
    ///
    /// Called by [`crate::exporter::otel::install`] right after it sets the
    /// global meter provider, so the instruments attach to the real OTLP
    /// pipeline instead of the no-op provider. Idempotent: a second call
    /// rebuilds against whatever provider is current.
    ///
    /// A build failure (malformed instrument config) is swallowed — the handle
    /// is left untouched and recording remains a no-op rather than panicking on
    /// the daemon's startup path.
    pub fn init_instruments() {
        if let Ok(built) = GenAiInstruments::build() {
            if let Ok(mut slot) = INSTRUMENTS.write() {
                *slot = Some(built);
            }
        }
    }

    /// See the crate-level [`crate::instruments::record_gen_ai_usage`] doc.
    ///
    /// The read guard is intentionally held across all four instrument writes so
    /// the borrowed `inst` reference stays valid for the whole recording — a
    /// single consistent view of the instrument set — hence the
    /// `significant_drop_tightening` allow (mirroring [`crate::Metrics`]).
    #[allow(clippy::too_many_arguments, clippy::significant_drop_tightening)]
    pub fn record_gen_ai_usage(
        system: &str,
        request_model: &str,
        response_model: &str,
        input_tokens: u64,
        output_tokens: u64,
        latency_ms: f64,
        tool_calls: u64,
    ) {
        let guard = match INSTRUMENTS.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(inst) = guard.as_ref() else {
            // Not installed yet (or build failed): no-op.
            return;
        };
        let attrs = gen_ai_attributes(system, request_model, response_model);
        inst.input_tokens.add(input_tokens, &attrs);
        inst.output_tokens.add(output_tokens, &attrs);
        inst.tool_calls.add(tool_calls, &attrs);
        inst.operation_duration.record(latency_ms, &attrs);
    }

    /// Build the `gen_ai.*` attribute set for a recording, **deriving** every
    /// convention key from origin's internal label name via
    /// [`gen_ai_attr_for_label`] so the relabel map is the single source of
    /// truth.
    ///
    /// - `provider` -> `gen_ai.system`
    /// - `model`    -> `gen_ai.request.model`
    ///
    /// `gen_ai.response.model` has no internal-label counterpart in the relabel
    /// map (the map only covers families/labels shared with the Prometheus
    /// path), so its key comes straight from the convention constant. An empty
    /// `response_model` is dropped so we don't emit a blank attribute when the
    /// provider echoed no response model.
    ///
    /// Pure and total: no I/O, no globals — directly unit-testable.
    fn gen_ai_attributes(
        system: &str,
        request_model: &str,
        response_model: &str,
    ) -> Vec<KeyValue> {
        let system_key = gen_ai_attr_for_label("provider").unwrap_or(genai::SYSTEM);
        let request_model_key = gen_ai_attr_for_label("model").unwrap_or(genai::REQUEST_MODEL);

        let mut attrs = Vec::with_capacity(3);
        attrs.push(KeyValue::new(system_key, system.to_owned()));
        attrs.push(KeyValue::new(request_model_key, request_model.to_owned()));
        if !response_model.is_empty() {
            attrs.push(KeyValue::new(
                genai::RESPONSE_MODEL,
                response_model.to_owned(),
            ));
        }
        attrs
    }

    /// The terminal outcome of a turn, mapped onto the convention's
    /// success/failure attributes by [`gen_ai_attributes_ext`].
    ///
    /// A turn ends *either* with a model-reported finish reason (`stop`,
    /// `length`, `tool_calls`, …) *or* with an error class — never both. Modeling
    /// the two as a single enum makes that exclusivity explicit at the call site
    /// and keeps the attribute mapping total. Borrowed string slices so the
    /// daemon can pass provider-supplied `&str` without an allocation.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Outcome<'a> {
        /// A successful turn carrying the model's finish reason; maps to
        /// `gen_ai.response.finish_reasons`.
        FinishReason(&'a str),
        /// A failed turn carrying an error class; maps to `error.type`.
        Error(&'a str),
    }

    /// Build the extended `gen_ai.*` attribute set attached on the latency
    /// recordings: the base relabeled keys from [`gen_ai_attributes`] **plus**
    /// `gen_ai.operation.name`, and — when an [`Outcome`] is supplied — either
    /// `gen_ai.response.finish_reasons` (success) or `error.type` (failure).
    ///
    /// Deriving the base keys through the relabel map keeps [`crate::keys`] the
    /// single source of truth; the operation-name and outcome keys come straight
    /// from the convention constants. An empty `operation` is dropped so we never
    /// emit a blank `gen_ai.operation.name`, and the [`Outcome`] enum guarantees
    /// at most one of finish-reason / error.type is present.
    ///
    /// Pure and total: no I/O, no globals — directly unit-testable.
    fn gen_ai_attributes_ext(
        system: &str,
        request_model: &str,
        response_model: &str,
        operation: &str,
        outcome: Option<Outcome<'_>>,
    ) -> Vec<KeyValue> {
        let mut attrs = gen_ai_attributes(system, request_model, response_model);
        if !operation.is_empty() {
            attrs.push(KeyValue::new(genai::OPERATION_NAME, operation.to_owned()));
        }
        match outcome {
            Some(Outcome::FinishReason(reason)) if !reason.is_empty() => {
                attrs.push(KeyValue::new(
                    genai::RESPONSE_FINISH_REASONS,
                    reason.to_owned(),
                ));
            }
            Some(Outcome::Error(error_type)) if !error_type.is_empty() => {
                attrs.push(KeyValue::new(genai::ERROR_TYPE, error_type.to_owned()));
            }
            _ => {}
        }
        attrs
    }

    /// See the crate-level [`crate::instruments::record_time_to_first_token`].
    ///
    /// Records `ttft_ms` to the `gen_ai.server.time_to_first_token` histogram
    /// with the extended attribute set (system / request.model / operation.name).
    /// No-op until the instruments are bound.
    ///
    /// The read guard is held across the single record so the borrowed `inst`
    /// stays valid (mirrors [`record_gen_ai_usage`]).
    #[allow(clippy::significant_drop_tightening)]
    pub fn record_time_to_first_token(
        system: &str,
        request_model: &str,
        operation: &str,
        ttft_ms: f64,
    ) {
        let guard = match INSTRUMENTS.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(inst) = guard.as_ref() else {
            return;
        };
        let attrs = gen_ai_attributes_ext(system, request_model, "", operation, None);
        inst.time_to_first_token.record(ttft_ms, &attrs);
    }

    /// See the crate-level [`crate::instruments::record_time_per_output_token`].
    ///
    /// Records `tpot_ms` to the `gen_ai.server.time_per_output_token` histogram
    /// with the extended attribute set. No-op until the instruments are bound.
    #[allow(clippy::significant_drop_tightening)]
    pub fn record_time_per_output_token(
        system: &str,
        request_model: &str,
        operation: &str,
        tpot_ms: f64,
    ) {
        let guard = match INSTRUMENTS.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(inst) = guard.as_ref() else {
            return;
        };
        let attrs = gen_ai_attributes_ext(system, request_model, "", operation, None);
        inst.time_per_output_token.record(tpot_ms, &attrs);
    }

    /// RAII guard that ends its wrapped `gen_ai` span on `Drop`.
    ///
    /// Holds a [`BoxedSpan`] started on the installed global tracer. The SDK
    /// span ends (and is queued for export) when this guard goes out of scope,
    /// so the daemon brackets the provider call simply by holding the guard for
    /// the call's duration. The guard owns the span outright — there is no
    /// shared state and nothing to poison — so `Drop` is infallible.
    ///
    /// [`BoxedSpan`]: opentelemetry::global::BoxedSpan
    #[allow(clippy::module_name_repetitions)]
    pub struct GenAiSpanGuard {
        /// `None` only in the degenerate case where construction could not
        /// produce a span (kept for symmetry; `start` always yields a span,
        /// a no-op one when no provider is installed).
        span: Option<opentelemetry::global::BoxedSpan>,
    }

    impl Drop for GenAiSpanGuard {
        fn drop(&mut self) {
            use opentelemetry::trace::Span as _;
            if let Some(mut span) = self.span.take() {
                span.end();
            }
        }
    }

    /// See the crate-level [`crate::instruments::gen_ai_span`].
    ///
    /// Starts a `gen_ai` **client** span on the installed global tracer named
    /// `"{operation} {request_model}"` (the convention's span-name shape) and
    /// attaches the relabeled `gen_ai.system` / `gen_ai.request.model` /
    /// `gen_ai.operation.name` attributes via [`gen_ai_attributes_ext`]. The
    /// span ends when the returned [`GenAiSpanGuard`] is dropped.
    ///
    /// Before any tracer provider is installed the global tracer is the no-op
    /// provider, so this still returns a (no-op) span guard without panicking —
    /// the daemon may call it unconditionally.
    #[must_use]
    #[allow(clippy::module_name_repetitions)]
    pub fn gen_ai_span(
        system: &str,
        request_model: &str,
        operation: &str,
    ) -> GenAiSpanGuard {
        use opentelemetry::global;
        use opentelemetry::trace::{SpanKind, Tracer as _};

        // Convention span name: "{operation} {model}" (e.g. "chat gpt-4o").
        // Fall back to just the operation when the model is unknown so we never
        // emit a trailing space.
        let name = if request_model.is_empty() {
            operation.to_owned()
        } else {
            format!("{operation} {request_model}")
        };
        let attrs = gen_ai_attributes_ext(system, request_model, "", operation, None);
        let tracer = global::tracer(SCOPE_NAME);
        let span = tracer
            .span_builder(name)
            .with_kind(SpanKind::Client)
            .with_attributes(attrs)
            .start(&tracer);
        GenAiSpanGuard { span: Some(span) }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        use std::time::Duration;

        use opentelemetry::Value;

        use super::{
            gen_ai_attributes, gen_ai_attributes_ext, gen_ai_span, init_instruments,
            record_gen_ai_usage, record_time_per_output_token, record_time_to_first_token, Outcome,
        };
        use crate::keys::genai;

        /// The attribute helper derives each convention key from the relabel map
        /// (not a hand-typed string) and carries the supplied values.
        #[test]
        fn attributes_use_relabeled_gen_ai_keys() {
            let attrs = gen_ai_attributes("anthropic", "claude-sonnet-4-6", "claude-sonnet-4-6");
            // Three attributes: system, request.model, response.model.
            assert_eq!(attrs.len(), 3);

            let system = attrs
                .iter()
                .find(|kv| kv.key.as_str() == genai::SYSTEM)
                .expect("gen_ai.system attribute must be present");
            assert_eq!(system.value, Value::from("anthropic".to_string()));

            let req_model = attrs
                .iter()
                .find(|kv| kv.key.as_str() == genai::REQUEST_MODEL)
                .expect("gen_ai.request.model attribute must be present");
            assert_eq!(req_model.value, Value::from("claude-sonnet-4-6".to_string()));

            let resp_model = attrs
                .iter()
                .find(|kv| kv.key.as_str() == genai::RESPONSE_MODEL)
                .expect("gen_ai.response.model attribute must be present");
            assert_eq!(
                resp_model.value,
                Value::from("claude-sonnet-4-6".to_string())
            );

            // Every key is a gen_ai.* convention name.
            for kv in &attrs {
                assert!(
                    kv.key.as_str().starts_with("gen_ai."),
                    "{} must be a gen_ai.* attribute",
                    kv.key.as_str()
                );
            }
        }

        /// The convention keys come from the relabel map, so they match exactly
        /// what `gen_ai_attr_for_label` returns — proving the map is applied
        /// rather than bypassed.
        #[test]
        fn keys_match_the_relabel_map() {
            use crate::keys::gen_ai_attr_for_label;
            let attrs = gen_ai_attributes("openai", "gpt-4o", "gpt-4o-2024");
            let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
            assert!(keys.contains(&gen_ai_attr_for_label("provider").unwrap()));
            assert!(keys.contains(&gen_ai_attr_for_label("model").unwrap()));
        }

        /// An empty response model is dropped (no blank attribute emitted).
        #[test]
        fn empty_response_model_is_omitted() {
            let attrs = gen_ai_attributes("ollama", "llama3", "");
            assert_eq!(attrs.len(), 2);
            assert!(
                attrs
                    .iter()
                    .all(|kv| kv.key.as_str() != genai::RESPONSE_MODEL),
                "blank response model must not appear as an attribute"
            );
        }

        /// The extended attribute helper carries the base relabeled keys
        /// (system, request.model, response.model) PLUS `gen_ai.operation.name`,
        /// and — when an outcome string is supplied — a finish-reason or
        /// error.type attribute. This is the pure mapping the latency
        /// recordings attach; it needs no live `MeterProvider`.
        #[test]
        fn ext_attributes_add_operation_name_and_finish_reason() {
            let attrs = gen_ai_attributes_ext(
                "anthropic",
                "claude-sonnet-4-6",
                "claude-sonnet-4-6",
                genai::OPERATION_CHAT,
                Some(Outcome::FinishReason("stop")),
            );

            let op = attrs
                .iter()
                .find(|kv| kv.key.as_str() == genai::OPERATION_NAME)
                .expect("gen_ai.operation.name attribute must be present");
            assert_eq!(op.value, Value::from("chat".to_string()));

            let finish = attrs
                .iter()
                .find(|kv| kv.key.as_str() == genai::RESPONSE_FINISH_REASONS)
                .expect("gen_ai.response.finish_reasons attribute must be present");
            assert_eq!(finish.value, Value::from("stop".to_string()));

            // The base relabeled keys are still present.
            assert!(attrs.iter().any(|kv| kv.key.as_str() == genai::SYSTEM));
            assert!(attrs
                .iter()
                .any(|kv| kv.key.as_str() == genai::REQUEST_MODEL));
        }

        /// An error outcome maps to the `error.type` attribute (not a finish
        /// reason).
        #[test]
        fn ext_attributes_map_error_outcome_to_error_type() {
            let attrs = gen_ai_attributes_ext(
                "openai",
                "gpt-4o",
                "",
                genai::OPERATION_CHAT,
                Some(Outcome::Error("rate_limit")),
            );
            let err = attrs
                .iter()
                .find(|kv| kv.key.as_str() == genai::ERROR_TYPE)
                .expect("error.type attribute must be present on an error outcome");
            assert_eq!(err.value, Value::from("rate_limit".to_string()));
            // A failed turn carries no finish reason.
            assert!(attrs
                .iter()
                .all(|kv| kv.key.as_str() != genai::RESPONSE_FINISH_REASONS));
        }

        /// With no outcome supplied, the ext helper is exactly the base set plus
        /// `gen_ai.operation.name` — no finish-reason / error.type attribute is
        /// emitted, and an empty outcome string is never a blank attribute.
        #[test]
        fn ext_attributes_omit_outcome_when_absent() {
            let attrs =
                gen_ai_attributes_ext("ollama", "llama3", "llama3", genai::OPERATION_CHAT, None);
            assert!(attrs
                .iter()
                .all(|kv| kv.key.as_str() != genai::RESPONSE_FINISH_REASONS
                    && kv.key.as_str() != genai::ERROR_TYPE));
            assert!(attrs
                .iter()
                .any(|kv| kv.key.as_str() == genai::OPERATION_NAME));
        }

        /// Recording before any provider is installed is a safe no-op (does not
        /// panic, does nothing). The default global provider is the no-op
        /// provider, and `INSTRUMENTS` is `None`, so this exercises both guards.
        #[test]
        fn record_before_install_is_noop() {
            record_gen_ai_usage("anthropic", "m", "m", 10, 20, 1.5, 2);
            record_time_to_first_token("anthropic", "m", genai::OPERATION_CHAT, 12.5);
            record_time_per_output_token("anthropic", "m", genai::OPERATION_CHAT, 0.8);
        }

        /// After installing a real meter provider and (re)binding the
        /// instruments, recording flows through real `gen_ai.*` instruments
        /// without error. We install via the in-process pipeline against a valid
        /// endpoint (no live collector needed — exports happen off the hot path)
        /// and then record. The assertion is that the full path — build
        /// instruments on the real meter, build the relabeled attribute set,
        /// `add`/`record` — runs to completion.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn record_after_install_drives_real_instruments() {
            let built = tokio::time::timeout(Duration::from_secs(20), async {
                let provider = crate::exporter::otel::install(crate::exporter::otel::DEFAULT_ENDPOINT)
                    .expect("install must build a real pipeline");
                // install() already calls init_instruments(); call again to prove
                // idempotency / rebind against the now-current provider.
                init_instruments();
                record_gen_ai_usage("anthropic", "claude-sonnet-4-6", "claude-sonnet-4-6", 1_234, 567, 42.0, 3);
                record_gen_ai_usage("openai", "gpt-4o", "", 10, 5, 1.0, 0);
                // The two new latency histograms also drive through the real
                // instruments with the extended (operation.name) attribute set.
                record_time_to_first_token(
                    "anthropic",
                    "claude-sonnet-4-6",
                    genai::OPERATION_CHAT,
                    18.0,
                );
                record_time_per_output_token(
                    "anthropic",
                    "claude-sonnet-4-6",
                    genai::OPERATION_CHAT,
                    0.6,
                );
                // Skip Drop (would flush to the absent collector); the runtime
                // tears the reader task down on exit.
                std::mem::forget(provider);
            })
            .await;
            built.expect("record-after-install path must not hang");
        }

        /// `gen_ai_span` constructs against the global tracer and ends cleanly
        /// on `Drop` without panicking — both before any provider is installed
        /// (the global no-op tracer) and after the trace pipeline is installed.
        /// This is the otel-side counterpart to the non-otel no-op guard test:
        /// it proves the real guard's `start` + `Drop`-`end` path runs to
        /// completion. We bound it in a timeout and `mem::forget` the provider
        /// so its drop does not flush to the absent collector on teardown.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn gen_ai_span_constructs_and_ends_without_panicking() {
            let built = tokio::time::timeout(Duration::from_secs(20), async {
                // Before install: the global tracer is the no-op provider, so
                // starting + dropping a span is a safe no-op.
                drop(gen_ai_span("anthropic", "claude-sonnet-4-6", genai::OPERATION_CHAT));

                let provider = crate::exporter::otel::install(crate::exporter::otel::DEFAULT_ENDPOINT)
                    .expect("install must build a real pipeline");
                // After install: the span now starts on the real tracer. Hold
                // it across a trivial scope, then let the guard drop to end it.
                {
                    let _span = gen_ai_span("anthropic", "claude-sonnet-4-6", genai::OPERATION_CHAT);
                }
                // An empty model falls back to an operation-only span name.
                drop(gen_ai_span("openai", "", genai::OPERATION_CHAT));
                std::mem::forget(provider);
            })
            .await;
            built.expect("gen_ai_span path must not hang");
        }
    }
}
