use origin_tools::builtins::apply_patch::{apply_patch, ApplyPatchArgs};
use std::fs;
use tempfile::tempdir;

#[test]
fn applies_single_file_diff() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\nfn bar() {}\n").unwrap();
    let patch = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,2 +1,2 @@\n-fn foo() {{}}\n+fn FOO() {{}}\n fn bar() {{}}\n",
        path = p.to_string_lossy().replace('\\', "/")
    );
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(fs::read_to_string(&p).unwrap(), "fn FOO() {}\nfn bar() {}\n");
}

#[test]
fn rejects_patch_with_mismatched_context() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn foo() {}\n").unwrap();
    let bad = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,1 @@\n-fn DIFFERENT() {{}}\n+fn FOO() {{}}\n",
        path = p.to_string_lossy().replace('\\', "/")
    );
    let err = apply_patch(&ApplyPatchArgs { patch: bad }).unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Edit);
}
