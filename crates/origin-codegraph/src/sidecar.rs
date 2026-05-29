// SPDX-License-Identifier: Apache-2.0
//! `Sidecar` trait + reference implementations (P7.4, N6.8).
//!
//! Phase 5 will ship a real LLM-backed sidecar. P7.4 only defines the
//! trait surface plus two deterministic reference impls:
//!
//! - [`NoopSidecar`] — always returns an empty `Vec`. Useful as a default
//!   when sidecar extraction is disabled.
//! - [`LopdfTextSidecar`] — emits one [`ExtractedEntity`] per non-empty PDF
//!   page using `lopdf`'s text extraction, tagged
//!   [`Confidence::Extracted`].
//!
//! Non-PDF inputs always succeed and return an empty `Vec` — callers fan
//! the same job out to multiple sidecars and only one is expected to
//! handle any given input.

use crate::record::Confidence;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Input to a [`Sidecar`]: either a filesystem path or already-loaded bytes
/// with a hint at the kind (e.g. `"pdf"`).
#[derive(Debug, Clone)]
pub enum ExtractJob {
    Path(PathBuf),
    Bytes { kind_hint: &'static str, data: Vec<u8> },
}

/// One unit of extracted content from a sidecar. The `name` is a stable
/// pseudo-path (e.g. `"foo.pdf#page=1"`) suitable for use as an
/// `EntityId` after hashing; `body` is the raw extracted text.
#[derive(Debug, Clone)]
pub struct ExtractedEntity {
    pub name: String,
    pub body: String,
    pub confidence: Confidence,
}

/// Failure modes for [`Sidecar::extract`].
// `SidecarError` mirrors the `*Error` convention used across origin's other
// crates and stays public for callers downstream of the trait.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pdf: {0}")]
    Pdf(String),
}

/// Pluggable non-code extractor. Implementations are stateless and
/// thread-safe; the trait bounds let the daemon fan jobs across rayon /
/// tokio without per-call wrapping.
pub trait Sidecar: Send + Sync {
    /// Extract zero or more entities from `job`.
    ///
    /// # Errors
    /// Returns [`SidecarError::Io`] on file read failures and
    /// [`SidecarError::Pdf`] for PDF-specific errors.
    fn extract(&self, job: ExtractJob) -> Result<Vec<ExtractedEntity>, SidecarError>;
}

/// Sidecar that never extracts anything. Default for the daemon until
/// Phase 5 wires the real backend.
// Name is part of the Phase 7 public API; the `*Sidecar` suffix disambiguates
// across backend variants we'll add in later phases.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSidecar;

impl Sidecar for NoopSidecar {
    fn extract(&self, _job: ExtractJob) -> Result<Vec<ExtractedEntity>, SidecarError> {
        Ok(Vec::new())
    }
}

/// Deterministic PDF text extractor backed by `lopdf`. Emits one entity per
/// non-empty page.
// Name is part of the Phase 7 public API; the `*Sidecar` suffix disambiguates
// across backend variants we'll add in later phases.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Default, Clone, Copy)]
pub struct LopdfTextSidecar;

impl LopdfTextSidecar {
    fn is_pdf_path(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    }

    fn extract_from_bytes(bytes: &[u8], source_name: &str) -> Result<Vec<ExtractedEntity>, SidecarError> {
        let doc = lopdf::Document::load_mem(bytes).map_err(|e| SidecarError::Pdf(e.to_string()))?;
        let pages = doc.get_pages();
        let mut out = Vec::with_capacity(pages.len());
        for &page_num in pages.keys() {
            let text = doc
                .extract_text(&[page_num])
                .map_err(|e| SidecarError::Pdf(e.to_string()))?;
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push(ExtractedEntity {
                name: format!("{source_name}#page={page_num}"),
                body: trimmed.to_owned(),
                confidence: Confidence::Extracted,
            });
        }
        Ok(out)
    }
}

impl Sidecar for LopdfTextSidecar {
    fn extract(&self, job: ExtractJob) -> Result<Vec<ExtractedEntity>, SidecarError> {
        match job {
            ExtractJob::Path(path) => {
                if !Self::is_pdf_path(&path) {
                    return Ok(Vec::new());
                }
                let bytes = std::fs::read(&path)?;
                let name = path.to_string_lossy().into_owned();
                Self::extract_from_bytes(&bytes, &name)
            }
            ExtractJob::Bytes { kind_hint, data } => {
                if !kind_hint.eq_ignore_ascii_case("pdf") {
                    return Ok(Vec::new());
                }
                Self::extract_from_bytes(&data, "<bytes>")
            }
        }
    }
}
