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

#[cfg(not(feature = "otel"))]
impl GenAiSpanGuard {
    /// No-op in the default build. Mirrors the `otel` method the daemon calls
    /// live so the call site is feature-agnostic and the default path stays
    /// byte-identical.
    #[inline]
    #[allow(clippy::unused_self, clippy::module_name_repetitions)]
    pub const fn set_response_attributes(&mut self, _response_id: &str, _cached_input_tokens: u64) {}
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

    /// In the default build the daemon-facing `set_response_attributes` is a
    /// no-op `&mut self` method: calling it links, runs, and justifies the
    /// daemon's `let mut` binding without any otel dependency.
    #[test]
    fn set_response_attributes_is_a_noop_without_otel() {
        use std::hint::black_box;
        let mut guard = super::gen_ai_span(
            black_box("anthropic"),
            black_box("claude-sonnet-4-6"),
            black_box("chat"),
        );
        guard.set_response_attributes(black_box("resp_1"), black_box(64));
        drop(guard);
    }
}

// ---------------------------------------------------------------------------
// otel-enabled path: real instruments bound to the global meter.
// ---------------------------------------------------------------------------

#[cfg(feature = "otel")]
#[allow(clippy::module_name_repetitions)]
pub use otel_impl::{
    gen_ai_span, init_instruments, record_gen_ai_usage, record_time_per_output_token,
    record_time_to_first_token, GenAiSpanGuard, Outcome, RequestParams,
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
        client_token_usage: Histogram<u64>,
        server_request_duration: Histogram<f64>,
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
            let client_token_usage = meter
                .u64_histogram(genai::CLIENT_TOKEN_USAGE)
                .with_description("GenAI client-side token usage distribution (input + output)")
                .with_unit("{token}")
                .try_init()?;
            let server_request_duration = meter
                .f64_histogram(genai::SERVER_REQUEST_DURATION)
                .with_description("GenAI server-side request duration")
                .with_unit("ms")
                .try_init()?;
            Ok(Self {
                input_tokens,
                output_tokens,
                tool_calls,
                operation_duration,
                time_to_first_token,
                time_per_output_token,
                client_token_usage,
                server_request_duration,
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
        // Convention distributions alongside the additive counters: the total
        // token usage and the request duration as histograms.
        inst.client_token_usage
            .record(input_tokens.saturating_add(output_tokens), &attrs);
        inst.server_request_duration.record(latency_ms, &attrs);
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

    /// Push a non-empty string attribute (the omit-if-empty pattern shared by
    /// all the convention builders, so a blank field never emits an attribute).
    fn push_str_attr(attrs: &mut Vec<KeyValue>, key: &'static str, value: &str) {
        if !value.is_empty() {
            attrs.push(KeyValue::new(key, value.to_owned()));
        }
    }

    /// Build a `GenAI` message span-event: the event name (`gen_ai.{role}.message`)
    /// and its role + (optional) content attributes. Pure and total.
    fn gen_ai_message_event(role: &str, content: &str) -> (&'static str, Vec<KeyValue>) {
        let name = match role {
            "system" => genai::EVENT_SYSTEM_MESSAGE,
            "assistant" => genai::EVENT_ASSISTANT_MESSAGE,
            "tool" => genai::EVENT_TOOL_MESSAGE,
            _ => genai::EVENT_USER_MESSAGE,
        };
        let mut attrs = Vec::with_capacity(2);
        attrs.push(KeyValue::new(genai::MESSAGE_ROLE, role.to_owned()));
        push_str_attr(&mut attrs, genai::MESSAGE_CONTENT, content);
        (name, attrs)
    }

    /// Build a `gen_ai.choice` event: 0-based index + (optional) finish reason.
    fn gen_ai_choice_event(index: i64, finish_reason: &str) -> (&'static str, Vec<KeyValue>) {
        let mut attrs = Vec::with_capacity(2);
        attrs.push(KeyValue::new(genai::CHOICE_INDEX, index));
        push_str_attr(&mut attrs, genai::RESPONSE_FINISH_REASONS, finish_reason);
        (genai::EVENT_CHOICE, attrs)
    }

    /// Build a `gen_ai.tool.message` event: tool-call id + (optional) content.
    fn gen_ai_tool_message_event(
        tool_call_id: &str,
        content: &str,
    ) -> (&'static str, Vec<KeyValue>) {
        let mut attrs = Vec::with_capacity(2);
        push_str_attr(&mut attrs, genai::TOOL_CALL_ID, tool_call_id);
        push_str_attr(&mut attrs, genai::MESSAGE_CONTENT, content);
        (genai::EVENT_TOOL_MESSAGE, attrs)
    }

    /// Requested sampling / generation parameters for a turn. Every field is
    /// optional; `None`/empty fields are omitted from the built attribute set.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct RequestParams<'a> {
        pub temperature: Option<f64>,
        pub top_p: Option<f64>,
        pub top_k: Option<i64>,
        pub max_tokens: Option<i64>,
        pub frequency_penalty: Option<f64>,
        pub presence_penalty: Option<f64>,
        pub seed: Option<i64>,
        pub choice_count: Option<i64>,
        pub stop_sequences: &'a [String],
        pub encoding_formats: &'a [String],
    }

    fn push_str_array(attrs: &mut Vec<KeyValue>, key: &'static str, values: &[String]) {
        if !values.is_empty() {
            let arr: Vec<opentelemetry::StringValue> =
                values.iter().map(|s| s.clone().into()).collect();
            attrs.push(KeyValue::new(
                key,
                opentelemetry::Value::Array(opentelemetry::Array::String(arr)),
            ));
        }
    }

    /// Build the `gen_ai.request.*` sampling attribute set from [`RequestParams`].
    /// Pure and total: every unset field is omitted. Uses each convention
    /// constant directly so the keys are the single source of truth.
    fn gen_ai_request_attributes(p: RequestParams<'_>) -> Vec<KeyValue> {
        let mut attrs = Vec::new();
        if let Some(v) = p.temperature {
            attrs.push(KeyValue::new(genai::REQUEST_TEMPERATURE, v));
        }
        if let Some(v) = p.top_p {
            attrs.push(KeyValue::new(genai::REQUEST_TOP_P, v));
        }
        if let Some(v) = p.top_k {
            attrs.push(KeyValue::new(genai::REQUEST_TOP_K, v));
        }
        if let Some(v) = p.max_tokens {
            attrs.push(KeyValue::new(genai::REQUEST_MAX_TOKENS, v));
        }
        if let Some(v) = p.frequency_penalty {
            attrs.push(KeyValue::new(genai::REQUEST_FREQUENCY_PENALTY, v));
        }
        if let Some(v) = p.presence_penalty {
            attrs.push(KeyValue::new(genai::REQUEST_PRESENCE_PENALTY, v));
        }
        if let Some(v) = p.seed {
            attrs.push(KeyValue::new(genai::REQUEST_SEED, v));
        }
        if let Some(v) = p.choice_count {
            attrs.push(KeyValue::new(genai::REQUEST_CHOICE_COUNT, v));
        }
        push_str_array(&mut attrs, genai::REQUEST_STOP_SEQUENCES, p.stop_sequences);
        push_str_array(&mut attrs, genai::REQUEST_ENCODING_FORMATS, p.encoding_formats);
        attrs
    }

    /// Build the `gen_ai.response.*` / cached-usage attribute set. Empty id and
    /// zero cached tokens are omitted.
    fn gen_ai_response_attributes(response_id: &str, cached_input_tokens: u64) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(2);
        push_str_attr(&mut attrs, genai::RESPONSE_ID, response_id);
        if cached_input_tokens > 0 {
            attrs.push(KeyValue::new(
                genai::USAGE_CACHED_INPUT_TOKENS,
                i64::try_from(cached_input_tokens).unwrap_or(i64::MAX),
            ));
        }
        attrs
    }

    /// Build the `gen_ai.agent.*` / conversation / data-source attribute set.
    /// Each empty field is omitted.
    fn gen_ai_agent_attributes(
        agent_id: &str,
        agent_name: &str,
        agent_description: &str,
        conversation_id: &str,
        data_source_id: &str,
    ) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(5);
        push_str_attr(&mut attrs, genai::AGENT_ID, agent_id);
        push_str_attr(&mut attrs, genai::AGENT_NAME, agent_name);
        push_str_attr(&mut attrs, genai::AGENT_DESCRIPTION, agent_description);
        push_str_attr(&mut attrs, genai::CONVERSATION_ID, conversation_id);
        push_str_attr(&mut attrs, genai::DATA_SOURCE_ID, data_source_id);
        attrs
    }

    /// Build the provider/server attribute set (`gen_ai.provider.name`,
    /// `server.address`, `server.port`). Empty/`None` fields are omitted.
    fn gen_ai_server_attributes(
        provider_name: &str,
        server_address: &str,
        server_port: Option<i64>,
    ) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(3);
        push_str_attr(&mut attrs, genai::PROVIDER_NAME, provider_name);
        push_str_attr(&mut attrs, genai::SERVER_ADDRESS, server_address);
        if let Some(port) = server_port {
            attrs.push(KeyValue::new(genai::SERVER_PORT, port));
        }
        attrs
    }

    /// Build the `gen_ai.tool.*` attribute set for an executed tool. Empty
    /// fields are omitted.
    fn gen_ai_tool_attributes(
        tool_name: &str,
        tool_call_id: &str,
        tool_description: &str,
        tool_type: &str,
    ) -> Vec<KeyValue> {
        let mut attrs = Vec::with_capacity(4);
        push_str_attr(&mut attrs, genai::TOOL_NAME, tool_name);
        push_str_attr(&mut attrs, genai::TOOL_CALL_ID, tool_call_id);
        push_str_attr(&mut attrs, genai::TOOL_DESCRIPTION, tool_description);
        push_str_attr(&mut attrs, genai::TOOL_TYPE, tool_type);
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

    impl GenAiSpanGuard {
        /// Attach a built attribute set to the live span (no-op when no span /
        /// no attributes).
        fn set_attributes(&mut self, attrs: Vec<KeyValue>) {
            use opentelemetry::trace::Span as _;
            if attrs.is_empty() {
                return;
            }
            if let Some(span) = self.span.as_mut() {
                for kv in attrs {
                    span.set_attribute(kv);
                }
            }
        }

        /// Add a named span event with `attrs` (no-op when no span).
        fn add_event(&mut self, name: &'static str, attrs: Vec<KeyValue>) {
            use opentelemetry::trace::Span as _;
            if let Some(span) = self.span.as_mut() {
                span.add_event(name, attrs);
            }
        }

        /// Attach the requested sampling/generation parameters
        /// (`gen_ai.request.*`) to the span.
        pub fn set_request_params(&mut self, params: RequestParams<'_>) {
            self.set_attributes(gen_ai_request_attributes(params));
        }

        /// Attach the response id + cached-input-token usage to the span.
        pub fn set_response_attributes(&mut self, response_id: &str, cached_input_tokens: u64) {
            self.set_attributes(gen_ai_response_attributes(response_id, cached_input_tokens));
        }

        /// Attach the agent / conversation / data-source attributes to the span.
        pub fn set_agent_attributes(
            &mut self,
            agent_id: &str,
            agent_name: &str,
            agent_description: &str,
            conversation_id: &str,
            data_source_id: &str,
        ) {
            self.set_attributes(gen_ai_agent_attributes(
                agent_id,
                agent_name,
                agent_description,
                conversation_id,
                data_source_id,
            ));
        }

        /// Attach provider/server (`gen_ai.provider.name`, `server.*`) attributes.
        pub fn set_server_attributes(
            &mut self,
            provider_name: &str,
            server_address: &str,
            server_port: Option<i64>,
        ) {
            self.set_attributes(gen_ai_server_attributes(
                provider_name,
                server_address,
                server_port,
            ));
        }

        /// Attach `gen_ai.tool.*` attributes for an executed tool.
        pub fn set_tool_attributes(
            &mut self,
            tool_name: &str,
            tool_call_id: &str,
            tool_description: &str,
            tool_type: &str,
        ) {
            self.set_attributes(gen_ai_tool_attributes(
                tool_name,
                tool_call_id,
                tool_description,
                tool_type,
            ));
        }

        /// Record a `GenAI` message (`gen_ai.{role}.message`) span event. Carries
        /// the message body, so callers should gate this on a content-capture
        /// opt-in (the convention treats message content as opt-in / PII).
        pub fn record_message(&mut self, role: &str, content: &str) {
            let (name, attrs) = gen_ai_message_event(role, content);
            self.add_event(name, attrs);
        }

        /// Record a `gen_ai.choice` span event (index + optional finish reason).
        pub fn record_choice(&mut self, index: i64, finish_reason: &str) {
            let (name, attrs) = gen_ai_choice_event(index, finish_reason);
            self.add_event(name, attrs);
        }

        /// Record a `gen_ai.tool.message` span event. Carries tool-result
        /// content, so it should be gated on the same content opt-in as
        /// [`Self::record_message`].
        pub fn record_tool_message(&mut self, tool_call_id: &str, content: &str) {
            let (name, attrs) = gen_ai_tool_message_event(tool_call_id, content);
            self.add_event(name, attrs);
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

        /// Find an attribute value by its convention key.
        fn find<'a>(attrs: &'a [opentelemetry::KeyValue], key: &str) -> Option<&'a Value> {
            attrs
                .iter()
                .find(|kv| kv.key.as_str() == key)
                .map(|kv| &kv.value)
        }

        #[test]
        fn message_event_builders_emit_correct_names_and_keys() {
            let (name, attrs) = super::gen_ai_message_event("user", "hi");
            assert_eq!(name, genai::EVENT_USER_MESSAGE);
            assert_eq!(find(&attrs, genai::MESSAGE_ROLE), Some(&Value::from("user")));
            assert_eq!(find(&attrs, genai::MESSAGE_CONTENT), Some(&Value::from("hi")));
            assert_eq!(
                super::gen_ai_message_event("system", "x").0,
                genai::EVENT_SYSTEM_MESSAGE
            );
            assert_eq!(
                super::gen_ai_message_event("assistant", "x").0,
                genai::EVENT_ASSISTANT_MESSAGE
            );
            // Empty content omits the content attribute (no blank attribute).
            let (_, empty_content) = super::gen_ai_message_event("user", "");
            assert!(find(&empty_content, genai::MESSAGE_CONTENT).is_none());
        }

        #[test]
        fn choice_event_carries_finish_reason_and_index() {
            let (name, attrs) = super::gen_ai_choice_event(0, "stop");
            assert_eq!(name, genai::EVENT_CHOICE);
            assert_eq!(find(&attrs, genai::CHOICE_INDEX), Some(&Value::I64(0)));
            assert_eq!(
                find(&attrs, genai::RESPONSE_FINISH_REASONS),
                Some(&Value::from("stop"))
            );
            // An empty finish reason is omitted (the index stays).
            let (_, no_reason) = super::gen_ai_choice_event(1, "");
            assert_eq!(find(&no_reason, genai::CHOICE_INDEX), Some(&Value::I64(1)));
            assert!(find(&no_reason, genai::RESPONSE_FINISH_REASONS).is_none());
        }

        #[test]
        fn tool_message_event_uses_tool_call_id() {
            let (name, attrs) = super::gen_ai_tool_message_event("call_123", "result");
            assert_eq!(name, genai::EVENT_TOOL_MESSAGE);
            assert_eq!(find(&attrs, genai::TOOL_CALL_ID), Some(&Value::from("call_123")));
            assert_eq!(find(&attrs, genai::MESSAGE_CONTENT), Some(&Value::from("result")));
            assert!(super::gen_ai_tool_message_event("", "").1.is_empty());
        }

        #[test]
        fn request_attributes_pin_sampling_keys() {
            let stop = vec!["X".to_string(), "Y".to_string()];
            let attrs = super::gen_ai_request_attributes(super::RequestParams {
                temperature: Some(0.7),
                top_p: Some(0.9),
                top_k: Some(40),
                max_tokens: Some(1000),
                frequency_penalty: Some(0.1),
                presence_penalty: Some(0.2),
                seed: Some(7),
                choice_count: Some(1),
                stop_sequences: &stop,
                encoding_formats: &[],
            });
            assert_eq!(find(&attrs, genai::REQUEST_TEMPERATURE), Some(&Value::F64(0.7)));
            assert_eq!(find(&attrs, genai::REQUEST_MAX_TOKENS), Some(&Value::I64(1000)));
            assert_eq!(find(&attrs, genai::REQUEST_SEED), Some(&Value::I64(7)));
            assert_eq!(find(&attrs, genai::REQUEST_CHOICE_COUNT), Some(&Value::I64(1)));
            assert!(
                matches!(find(&attrs, genai::REQUEST_STOP_SEQUENCES), Some(Value::Array(_))),
                "expected a stop-sequence string array"
            );
            // The default (all unset) yields no attributes.
            assert!(super::gen_ai_request_attributes(super::RequestParams::default()).is_empty());
        }

        #[test]
        fn response_attributes_include_cached_tokens_and_id() {
            let attrs = super::gen_ai_response_attributes("resp_42", 128);
            assert_eq!(find(&attrs, genai::RESPONSE_ID), Some(&Value::from("resp_42")));
            assert_eq!(
                find(&attrs, genai::USAGE_CACHED_INPUT_TOKENS),
                Some(&Value::I64(128))
            );
            // Empty id + zero cached tokens => nothing emitted.
            assert!(super::gen_ai_response_attributes("", 0).is_empty());
        }

        #[test]
        fn agent_server_and_tool_attributes_pin_keys() {
            let a = super::gen_ai_agent_attributes("id1", "origin", "desc", "conv1", "ds1");
            assert_eq!(find(&a, genai::AGENT_ID), Some(&Value::from("id1")));
            assert_eq!(find(&a, genai::AGENT_NAME), Some(&Value::from("origin")));
            assert_eq!(find(&a, genai::CONVERSATION_ID), Some(&Value::from("conv1")));
            assert_eq!(find(&a, genai::DATA_SOURCE_ID), Some(&Value::from("ds1")));
            assert!(super::gen_ai_agent_attributes("", "", "", "", "").is_empty());

            let s = super::gen_ai_server_attributes("anthropic", "api.host", Some(443));
            assert_eq!(find(&s, genai::PROVIDER_NAME), Some(&Value::from("anthropic")));
            assert_eq!(find(&s, genai::SERVER_ADDRESS), Some(&Value::from("api.host")));
            assert_eq!(find(&s, genai::SERVER_PORT), Some(&Value::I64(443)));

            let t = super::gen_ai_tool_attributes("Bash", "tc1", "run shell", "function");
            assert_eq!(find(&t, genai::TOOL_NAME), Some(&Value::from("Bash")));
            assert_eq!(find(&t, genai::TOOL_TYPE), Some(&Value::from("function")));
        }

        /// The *daemon-emitted* server-attribute shape: the daemon only has the
        /// provider name reachable through the `Provider` trait (no base URL /
        /// port), so it passes a non-empty provider name with an empty address
        /// and `None` port. The builder must then emit exactly one attribute —
        /// `gen_ai.provider.name` — and omit the unreachable `server.*` fields
        /// rather than emitting blanks. This pins the exact set the live loop
        /// produces via `GenAiSpanGuard::set_server_attributes`.
        #[test]
        fn server_attributes_provider_name_only_is_the_daemon_shape() {
            let s = super::gen_ai_server_attributes("anthropic", "", None);
            assert_eq!(s.len(), 1, "only the provider name is emitted");
            assert_eq!(find(&s, genai::PROVIDER_NAME), Some(&Value::from("anthropic")));
            assert!(find(&s, genai::SERVER_ADDRESS).is_none());
            assert!(find(&s, genai::SERVER_PORT).is_none());
        }

        /// The *daemon-emitted* agent-attribute shape: the daemon threads the
        /// session id as `gen_ai.conversation.id` and leaves the agent/data-source
        /// fields empty, so exactly one attribute is produced.
        #[test]
        fn agent_attributes_conversation_only_is_the_daemon_shape() {
            let a = super::gen_ai_agent_attributes("", "", "", "sess-123", "");
            assert_eq!(a.len(), 1, "only the conversation id is emitted");
            assert_eq!(find(&a, genai::CONVERSATION_ID), Some(&Value::from("sess-123")));
            assert!(find(&a, genai::AGENT_ID).is_none());
        }

        /// The *daemon-emitted* request-param shape: the loop has no explicit
        /// sampling knobs, only an extended-thinking budget it maps onto
        /// `gen_ai.request.max_tokens`. So a `RequestParams` carrying only
        /// `max_tokens` must produce exactly the one `gen_ai.request.max_tokens`
        /// attribute (every other field omitted).
        #[test]
        fn request_attributes_max_tokens_only_is_the_daemon_shape() {
            let attrs = super::gen_ai_request_attributes(super::RequestParams {
                max_tokens: Some(8192),
                ..Default::default()
            });
            assert_eq!(attrs.len(), 1, "only max_tokens is emitted");
            assert_eq!(find(&attrs, genai::REQUEST_MAX_TOKENS), Some(&Value::I64(8192)));
        }

        /// The *daemon-emitted* response-attribute shape: the loop now threads a
        /// real, non-empty per-turn response id (derived from the session id +
        /// turn) plus the prompt-cache read count, so BOTH `gen_ai.response.id`
        /// and `gen_ai.usage.cached_input_tokens` are present.
        #[test]
        fn response_attributes_carry_real_id_and_cached_tokens() {
            let attrs = super::gen_ai_response_attributes("sess-123#t2", 256);
            assert_eq!(find(&attrs, genai::RESPONSE_ID), Some(&Value::from("sess-123#t2")));
            assert_eq!(
                find(&attrs, genai::USAGE_CACHED_INPUT_TOKENS),
                Some(&Value::I64(256))
            );
        }

        /// Exercise the live `GenAiSpanGuard` recording methods end-to-end on a
        /// real span so they are not dead code and never panic. Bounded by a
        /// timeout; the provider is `mem::forget`-ed to avoid a teardown flush.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn span_record_methods_run_without_panicking() {
            let built = tokio::time::timeout(Duration::from_secs(20), async {
                let provider =
                    crate::exporter::otel::install(crate::exporter::otel::DEFAULT_ENDPOINT)
                        .expect("install");
                {
                    let mut span = gen_ai_span("anthropic", "m", genai::OPERATION_CHAT);
                    span.set_request_params(super::RequestParams {
                        max_tokens: Some(100),
                        ..Default::default()
                    });
                    span.set_response_attributes("resp_1", 64);
                    span.set_agent_attributes("a", "origin", "d", "conv", "ds");
                    span.set_server_attributes("anthropic", "host", Some(443));
                    span.set_tool_attributes("Bash", "tc", "desc", "function");
                    span.record_message("user", "hi");
                    span.record_choice(0, "stop");
                    span.record_tool_message("tc", "ok");
                }
                std::mem::forget(provider);
            })
            .await;
            built.expect("span record methods must not hang");
        }
    }
}
