// SPDX-License-Identifier: Apache-2.0
use origin_codegraph::record::Confidence;
use origin_codegraph::sidecar::{ExtractJob, LopdfTextSidecar, NoopSidecar, Sidecar};
use std::path::PathBuf;

#[test]
fn noop_returns_empty() {
    let s = NoopSidecar;
    let out = s
        .extract(ExtractJob::Path(PathBuf::from("nothing.pdf")))
        .expect("noop extract should succeed");
    assert!(out.is_empty(), "NoopSidecar must return empty");
}

#[test]
fn lopdf_extracts_text_from_pdf() {
    let s = LopdfTextSidecar;
    let path = PathBuf::from("tests/fixtures/empty.pdf");
    let out = s
        .extract(ExtractJob::Path(path))
        .expect("PDF extract should succeed");
    assert!(!out.is_empty(), "PDF should produce at least one entity");
    assert!(
        out.iter().any(|e| e.body.contains("ORIGIN")),
        "ORIGIN token missing: {out:?}",
    );
    for ent in &out {
        assert!(
            matches!(ent.confidence, Confidence::Extracted),
            "every PDF-extracted entity must carry Confidence::Extracted",
        );
    }
}

#[test]
fn unknown_file_kind_returns_empty() {
    let s = LopdfTextSidecar;
    let out = s
        .extract(ExtractJob::Path(PathBuf::from("Cargo.toml")))
        .expect("non-PDF must succeed");
    assert!(out.is_empty(), "non-PDF path must return empty Vec");
}
