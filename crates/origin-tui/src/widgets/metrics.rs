// SPDX-License-Identifier: Apache-2.0
//! `?metrics` panel widget (P11.12).
//!
//! Renders an [`origin_metrics::Metrics`] snapshot as a column-aligned
//! list of `name, labels, value` rows. The widget is pure-data: it
//! returns `Vec<String>` and leaves cell layout / clipping to the
//! cli renderer.

use origin_metrics::Metrics;

/// Pure-data view over a [`Metrics`] handle.
#[allow(clippy::module_name_repetitions)]
pub struct MetricsWidget<'a> {
    metrics: &'a Metrics,
}

impl<'a> MetricsWidget<'a> {
    /// Borrow a metrics handle.
    #[must_use]
    pub const fn new(metrics: &'a Metrics) -> Self {
        Self { metrics }
    }

    /// Render the snapshot as one line per metric series.
    ///
    /// The caller is responsible for clipping to the panel rect.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let snap = self.metrics.snapshot();
        let mut out: Vec<String> = Vec::with_capacity(snap.rows.len());
        for row in snap.iter() {
            let labels = row
                .labels
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            out.push(format!(
                "{name:<32} {labels:<48} {value:>10.0}",
                name = row.name,
                labels = labels,
                value = row.value,
            ));
        }
        out
    }
}
