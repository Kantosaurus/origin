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

/// Build/re-bind the `GenAI` instruments against the current global meter.
///
/// No-op without the `otel` feature. With the feature this is called by
/// [`crate::exporter::otel::install`] right after the global meter provider is
/// set, so recordings land on the real OTLP pipeline.
#[cfg(not(feature = "otel"))]
#[inline]
#[allow(clippy::module_name_repetitions)]
pub const fn init_instruments() {}

// ---------------------------------------------------------------------------
// otel-enabled path: real instruments bound to the global meter.
// ---------------------------------------------------------------------------

#[cfg(feature = "otel")]
#[allow(clippy::module_name_repetitions)]
pub use otel_impl::{init_instruments, record_gen_ai_usage};

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
            Ok(Self {
                input_tokens,
                output_tokens,
                tool_calls,
                operation_duration,
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

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        use std::time::Duration;

        use opentelemetry::Value;

        use super::{gen_ai_attributes, init_instruments, record_gen_ai_usage};
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

        /// Recording before any provider is installed is a safe no-op (does not
        /// panic, does nothing). The default global provider is the no-op
        /// provider, and `INSTRUMENTS` is `None`, so this exercises both guards.
        #[test]
        fn record_before_install_is_noop() {
            record_gen_ai_usage("anthropic", "m", "m", 10, 20, 1.5, 2);
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
                // Skip Drop (would flush to the absent collector); the runtime
                // tears the reader task down on exit.
                std::mem::forget(provider);
            })
            .await;
            built.expect("record-after-install path must not hang");
        }
    }
}
