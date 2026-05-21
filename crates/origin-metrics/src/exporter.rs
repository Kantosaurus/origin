//! Optional OpenTelemetry OTLP exporter. Gated behind the `otel` cargo feature.
//!
//! The body is intentionally minimal at P11.12; richer integration lands
//! post-GA once the OTel-bridge crate is added to workspace deps.

#[cfg(feature = "otel")]
pub mod otel {
    use opentelemetry::global;
    // `WithExportConfig` is imported as a trait so downstream call sites have
    // a stable path even though the stub body below does not invoke it yet.
    // `WithExportConfig` is not dyn-compatible (generic methods, `-> Self`),
    // so we can't reference it through `dyn`. The `use as _` import keeps the
    // trait in scope without producing a `unused_imports` warning.
    use opentelemetry_otlp::WithExportConfig as _;

    /// Install a global OTel meter provider pointing at `endpoint`.
    ///
    /// # Errors
    /// Returns a string error if exporter setup fails.
    pub fn install(endpoint: &str) -> Result<(), String> {
        let _ = global::meter_provider();
        let _ = endpoint;
        // The actual exporter wiring depends on opentelemetry 0.24's surface;
        // the meter-provider boot is sketched here. The full body lands once
        // the crate compiles cleanly against the workspace `tracing` major
        // and the OTel-bridge crate is added to the workspace deps.
        Ok(())
    }
}
