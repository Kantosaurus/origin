// SPDX-License-Identifier: Apache-2.0
//! Optional OpenTelemetry OTLP exporter. Gated behind the `otel` cargo feature.
//!
//! When the `otel` feature is enabled this module builds a real
//! OpenTelemetry OTLP **metrics** pipeline (gRPC / tonic transport), wraps the
//! push exporter in a [`PeriodicReader`] driven by the Tokio runtime, assembles
//! an [`SdkMeterProvider`], and installs it as the process-global meter provider
//! via [`opentelemetry::global::set_meter_provider`].
//!
//! Alongside metrics it also installs a **trace** (span) OTLP pipeline against
//! the same endpoint (a [`BatchSpanProcessor`] feeding the global tracer
//! provider), so spans and metrics correlate on `service.name`. Both share the
//! [`Resource`] built by `resource()`. The `GenAI` semantic-convention attribute
//! and metric names the export attaches live in [`crate::keys::genai`].
//!
//! [`PeriodicReader`]: opentelemetry_sdk::metrics::PeriodicReader
//! [`SdkMeterProvider`]: opentelemetry_sdk::metrics::SdkMeterProvider
//! [`BatchSpanProcessor`]: opentelemetry_sdk::trace::BatchSpanProcessor
//! [`Resource`]: opentelemetry_sdk::Resource

#[cfg(feature = "otel")]
pub mod otel {
    use std::time::Duration;

    use opentelemetry::global;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig as _;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::{runtime, Resource};

    /// Default OTLP/gRPC collector endpoint, matching the OpenTelemetry spec.
    pub const DEFAULT_ENDPOINT: &str = "http://localhost:4317";

    /// How often the [`PeriodicReader`] flushes accumulated metrics to the
    /// collector.
    ///
    /// [`PeriodicReader`]: opentelemetry_sdk::metrics::PeriodicReader
    const EXPORT_PERIOD: Duration = Duration::from_secs(30);

    /// Per-export deadline handed to the gRPC client.
    const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

    /// The OTLP `Resource` shared by the metrics and trace pipelines so both
    /// signals carry an identical `service.name` in the collector.
    fn resource() -> Resource {
        Resource::new([KeyValue::new("service.name", env!("CARGO_PKG_NAME"))])
    }

    /// Build a real OTLP metrics [`SdkMeterProvider`] pointing at `endpoint`
    /// and install it as the process-global meter provider.
    ///
    /// The pipeline uses the gRPC (tonic) transport. A [`PeriodicReader`]
    /// running on the ambient Tokio runtime ([`runtime::Tokio`]) flushes
    /// samples every [`EXPORT_PERIOD`]. Building the provider only validates
    /// the configuration and spins up the background reader task — it does **not**
    /// perform a blocking handshake, so an unreachable collector still installs
    /// cleanly; failed exports are retried/dropped on the background task.
    ///
    /// On success the freshly built provider is returned **in addition to**
    /// being set globally, so callers that want to `shutdown()` it on teardown
    /// can hold the handle.
    ///
    /// # Errors
    /// Returns a `String` describing the failure if the OTLP exporter pipeline
    /// cannot be constructed (e.g. a malformed endpoint or a transport the
    /// enabled features do not support).
    ///
    /// [`PeriodicReader`]: opentelemetry_sdk::metrics::PeriodicReader
    /// [`runtime::Tokio`]: opentelemetry_sdk::runtime::Tokio
    pub fn install(endpoint: &str) -> Result<SdkMeterProvider, String> {
        let exporter = opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(endpoint)
            .with_timeout(EXPORT_TIMEOUT);

        let provider = opentelemetry_otlp::new_pipeline()
            .metrics(runtime::Tokio)
            .with_exporter(exporter)
            .with_period(EXPORT_PERIOD)
            .with_timeout(EXPORT_TIMEOUT)
            .with_resource(resource())
            .build()
            .map_err(|e| format!("otlp metrics pipeline build failed: {e}"))?;

        global::set_meter_provider(provider.clone());

        // Best-effort: also stand up the trace (span) pipeline against the same
        // endpoint and install it as the global tracer provider. A failure here
        // must not tear down the already-installed metrics pipeline, so the
        // result is intentionally swallowed (the caller still gets the meter
        // provider it asked for). Spans are emitted via the global tracer; no
        // call-site change is required to start collecting them.
        let _trace = install_traces(endpoint);

        Ok(provider)
    }

    /// Build a real OTLP **trace** `TracerProvider` pointing at `endpoint` and
    /// install it as the process-global tracer provider.
    ///
    /// Mirrors [`install`]: the gRPC (tonic) transport feeds a
    /// [`BatchSpanProcessor`] driven by the ambient Tokio runtime
    /// ([`runtime::Tokio`]), so building only validates configuration and spins
    /// up the background batch task — an unreachable collector still installs
    /// cleanly and failed exports are retried/dropped off the hot path. The
    /// span `Resource` matches the metrics pipeline so both signals correlate
    /// on `service.name`. The returned handle is also set globally, so callers
    /// that want to `shutdown()` it on teardown can hold it.
    ///
    /// # Errors
    /// Returns a `String` describing the failure if the OTLP trace pipeline
    /// cannot be constructed (e.g. a malformed endpoint or a transport the
    /// enabled features do not support).
    ///
    /// [`BatchSpanProcessor`]: opentelemetry_sdk::trace::BatchSpanProcessor
    /// [`runtime::Tokio`]: opentelemetry_sdk::runtime::Tokio
    pub fn install_traces(
        endpoint: &str,
    ) -> Result<opentelemetry_sdk::trace::TracerProvider, String> {
        let exporter = opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(endpoint)
            .with_timeout(EXPORT_TIMEOUT);

        let provider = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(exporter)
            .with_trace_config(
                opentelemetry_sdk::trace::Config::default().with_resource(resource()),
            )
            .install_batch(runtime::Tokio)
            .map_err(|e| format!("otlp trace pipeline build failed: {e}"))?;

        global::set_tracer_provider(provider.clone());
        Ok(provider)
    }
}

#[cfg(all(test, feature = "otel"))]
mod tests {
    use std::time::Duration;

    use super::otel;

    /// Installing against a syntactically valid endpoint must succeed even when
    /// no collector is listening: the `PeriodicReader` is built lazily and the
    /// gRPC channel connects on first export (which happens on the background
    /// task), so `install` returns `Ok` without a live collector.
    ///
    /// `runtime::Tokio` (the periodic-reader driver) requires a **multi-thread**
    /// runtime, so the test must use that flavor — a current-thread `tokio::test`
    /// would deadlock the reader. We also bound the whole thing in a `timeout`
    /// so a misconfigured pipeline fails fast instead of hanging the suite, and
    /// we `mem::forget` the provider to avoid a flush-on-drop against the absent
    /// collector (the test runtime tears the reader task down on exit).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_builds_a_real_pipeline() {
        // `install` is sync but spawns the reader via `tokio::spawn`, so it must
        // run inside the (multi-thread) runtime context — hence calling it in the
        // async body rather than `spawn_blocking`.
        let built = tokio::time::timeout(Duration::from_secs(20), async {
            let a = otel::install("http://localhost:4317")
                .expect("building an OTLP metrics pipeline against a valid endpoint must succeed");
            let b = otel::install(otel::DEFAULT_ENDPOINT)
                .expect("default OTLP endpoint must build a valid pipeline");
            // Skip Drop (which would attempt a flush to the absent collector and
            // can block); the test runtime tears the reader task down on exit.
            std::mem::forget(a);
            std::mem::forget(b);
        })
        .await;
        built.expect("install must not hang");
    }

    /// The trace (span) pipeline builds against a syntactically valid endpoint
    /// without a live collector, mirroring the metrics pipeline: the
    /// `BatchSpanProcessor` connects on first export off the hot path. We bound
    /// it in a `timeout` and `mem::forget` the provider so its drop does not
    /// attempt a flush against the absent collector on test teardown.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn install_traces_builds_a_real_pipeline() {
        let built = tokio::time::timeout(Duration::from_secs(20), async {
            let p = otel::install_traces(otel::DEFAULT_ENDPOINT)
                .expect("building an OTLP trace pipeline against a valid endpoint must succeed");
            std::mem::forget(p);
        })
        .await;
        built.expect("install_traces must not hang");
    }

    /// The keys module exposes the `GenAI` semantic-convention names under the
    /// `otel` feature, and the internal-family mapping resolves to them.
    #[test]
    fn keys_expose_gen_ai_convention_names() {
        use crate::keys::{gen_ai_for_internal, genai};
        assert_eq!(genai::SYSTEM, "gen_ai.system");
        assert_eq!(genai::USAGE_INPUT_TOKENS, "gen_ai.usage.input_tokens");
        assert_eq!(
            gen_ai_for_internal("origin_tokens_in_total"),
            Some(genai::USAGE_INPUT_TOKENS)
        );
    }
}
