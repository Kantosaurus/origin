// SPDX-License-Identifier: Apache-2.0
//! Optional OpenTelemetry OTLP exporter. Gated behind the `otel` cargo feature.
//!
//! When the `otel` feature is enabled this module builds a real
//! OpenTelemetry OTLP **metrics** pipeline (gRPC / tonic transport), wraps the
//! push exporter in a [`PeriodicReader`] driven by the Tokio runtime, assembles
//! an [`SdkMeterProvider`], and installs it as the process-global meter provider
//! via [`opentelemetry::global::set_meter_provider`].
//!
//! [`PeriodicReader`]: opentelemetry_sdk::metrics::PeriodicReader
//! [`SdkMeterProvider`]: opentelemetry_sdk::metrics::SdkMeterProvider

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
            .with_resource(Resource::new([KeyValue::new(
                "service.name",
                env!("CARGO_PKG_NAME"),
            )]))
            .build()
            .map_err(|e| format!("otlp metrics pipeline build failed: {e}"))?;

        global::set_meter_provider(provider.clone());
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
}
