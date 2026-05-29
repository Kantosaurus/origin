// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::builtins::diagnostics::{diagnostics, DiagnosticsArgs};
use origin_tools::ra_bridge::{DiagnosticsHandle, RaDiagnostic, Severity};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
struct FakeRa {
    inner: Arc<std::sync::RwLock<Vec<RaDiagnostic>>>,
}

#[async_trait::async_trait]
impl DiagnosticsHandle for FakeRa {
    async fn diagnostics(
        &self,
        _path: Option<&Path>,
        _sev: Severity,
    ) -> Result<Vec<RaDiagnostic>, origin_tools::ToolError> {
        Ok(self.inner.read().unwrap().clone())
    }
    async fn notify_file_changed(&self, _path: &Path, _contents: &str) {}
}

#[tokio::test]
async fn empty_diagnostics_returns_empty_array() {
    let h = FakeRa::default();
    let out = diagnostics(
        DiagnosticsArgs {
            path: None,
            severity: Severity::Any,
        },
        &h as &dyn DiagnosticsHandle,
    )
    .await
    .unwrap();
    assert_eq!(out.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn populated_diagnostics_round_trip() {
    let h = FakeRa::default();
    h.inner.write().unwrap().push(RaDiagnostic {
        file: "a.rs".into(),
        line: 10,
        col: 5,
        severity: 1,
        message: "boom".into(),
        code: None,
    });
    let out = diagnostics(
        DiagnosticsArgs {
            path: None,
            severity: Severity::Any,
        },
        &h as &dyn DiagnosticsHandle,
    )
    .await
    .unwrap();
    assert_eq!(out.as_array().unwrap().len(), 1);
    assert_eq!(out[0]["message"], "boom");
    assert_eq!(out[0]["line"], 10);
}
