// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_tools::builtins::lsp_nav::{lsp_navigate, LspNavArgs};
use origin_tools::ra_bridge::{NavCallItem, NavLocation, NavigationHandle};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
struct FakeNav {
    defs: Arc<std::sync::RwLock<Vec<NavLocation>>>,
    refs: Arc<std::sync::RwLock<Vec<NavLocation>>>,
    incoming: Arc<std::sync::RwLock<Vec<NavCallItem>>>,
    outgoing: Arc<std::sync::RwLock<Vec<NavCallItem>>>,
    last_include_decl: Arc<std::sync::RwLock<Option<bool>>>,
}

#[async_trait::async_trait]
impl NavigationHandle for FakeNav {
    async fn definition(
        &self,
        _p: &Path,
        _l: u32,
        _c: u32,
    ) -> Result<Vec<NavLocation>, origin_tools::ToolError> {
        Ok(self.defs.read().unwrap().clone())
    }
    async fn references(
        &self,
        _p: &Path,
        _l: u32,
        _c: u32,
        include_declaration: bool,
    ) -> Result<Vec<NavLocation>, origin_tools::ToolError> {
        *self.last_include_decl.write().unwrap() = Some(include_declaration);
        Ok(self.refs.read().unwrap().clone())
    }
    async fn incoming_calls(
        &self,
        _p: &Path,
        _l: u32,
        _c: u32,
    ) -> Result<Vec<NavCallItem>, origin_tools::ToolError> {
        Ok(self.incoming.read().unwrap().clone())
    }
    async fn outgoing_calls(
        &self,
        _p: &Path,
        _l: u32,
        _c: u32,
    ) -> Result<Vec<NavCallItem>, origin_tools::ToolError> {
        Ok(self.outgoing.read().unwrap().clone())
    }
}

fn loc(file: &str, line: u32, col: u32) -> NavLocation {
    NavLocation {
        file: file.into(),
        line,
        col,
    }
}

#[tokio::test]
async fn definition_returns_locations_as_json() {
    let h = FakeNav::default();
    h.defs.write().unwrap().push(loc("src/lib.rs", 42, 7));
    let out = lsp_navigate(
        LspNavArgs {
            op: "definition".into(),
            path: "src/main.rs".into(),
            line: 10,
            col: 3,
            include_declaration: false,
        },
        &h as &dyn NavigationHandle,
    )
    .await
    .unwrap();
    let arr = out.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["file"], "src/lib.rs");
    assert_eq!(arr[0]["line"], 42);
    assert_eq!(arr[0]["col"], 7);
}

#[tokio::test]
async fn references_passes_include_declaration_and_returns_array() {
    let h = FakeNav::default();
    h.refs.write().unwrap().push(loc("a.rs", 1, 1));
    h.refs.write().unwrap().push(loc("b.rs", 2, 2));
    let out = lsp_navigate(
        LspNavArgs {
            op: "references".into(),
            path: "src/main.rs".into(),
            line: 5,
            col: 9,
            include_declaration: true,
        },
        &h as &dyn NavigationHandle,
    )
    .await
    .unwrap();
    assert_eq!(out.as_array().unwrap().len(), 2);
    assert_eq!(*h.last_include_decl.read().unwrap(), Some(true));
}

#[tokio::test]
async fn incoming_calls_returns_named_items() {
    let h = FakeNav::default();
    h.incoming.write().unwrap().push(NavCallItem {
        name: "caller_fn".into(),
        file: "src/call.rs".into(),
        line: 3,
        col: 4,
    });
    let out = lsp_navigate(
        LspNavArgs {
            op: "incoming_calls".into(),
            path: "src/main.rs".into(),
            line: 8,
            col: 1,
            include_declaration: false,
        },
        &h as &dyn NavigationHandle,
    )
    .await
    .unwrap();
    let arr = out.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "caller_fn");
    assert_eq!(arr[0]["file"], "src/call.rs");
    assert_eq!(arr[0]["line"], 3);
}

#[tokio::test]
async fn outgoing_calls_dispatches_to_outgoing_handle() {
    let h = FakeNav::default();
    h.outgoing.write().unwrap().push(NavCallItem {
        name: "callee_fn".into(),
        file: "src/callee.rs".into(),
        line: 9,
        col: 2,
    });
    let out = lsp_navigate(
        LspNavArgs {
            op: "outgoing_calls".into(),
            path: "src/main.rs".into(),
            line: 8,
            col: 1,
            include_declaration: false,
        },
        &h as &dyn NavigationHandle,
    )
    .await
    .unwrap();
    assert_eq!(out.as_array().unwrap()[0]["name"], "callee_fn");
}

#[tokio::test]
async fn unknown_op_is_a_validation_error() {
    let h = FakeNav::default();
    let err = lsp_navigate(
        LspNavArgs {
            op: "frobnicate".into(),
            path: "src/main.rs".into(),
            line: 1,
            col: 1,
            include_declaration: false,
        },
        &h as &dyn NavigationHandle,
    )
    .await
    .unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Validation);
}

#[test]
fn lsp_navigate_tool_is_registered_and_deferred() {
    let meta = origin_tools::registry_iter()
        .find(|m| m.name == "LspNavigate")
        .expect("LspNavigate must be registered");
    // Deferred (hot:false) so it stays out of the pinned hot-11 set.
    assert!(!meta.hot, "LspNavigate should be a deferred tool");
    assert!(meta.token_budget > 0);
}
