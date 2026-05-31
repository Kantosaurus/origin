// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

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

/// Normalise an absolute filesystem path into the forward-slash form the patch
/// markers/headers expect (mirrors the unified-diff tests above).
fn fwd(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[test]
fn marker_add_file_creates_new_file() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("sub/new.rs");
    let patch = format!(
        "*** Begin Patch\n*** Add File: {path}\n+fn hello() {{}}\n+fn world() {{}}\n*** End Patch\n",
        path = fwd(&p)
    );
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(out["files_added"], 1);
    assert_eq!(out["files_updated"], 0);
    // Parent dir was created and content written with trailing newline.
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "fn hello() {}\nfn world() {}\n"
    );
}

#[test]
fn marker_add_file_errors_when_target_exists() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("exists.rs");
    fs::write(&p, "old\n").unwrap();
    let patch = format!("*** Add File: {path}\n+new\n", path = fwd(&p));
    let err = apply_patch(&ApplyPatchArgs { patch }).unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Io);
    // Untouched.
    assert_eq!(fs::read_to_string(&p).unwrap(), "old\n");
}

#[test]
fn marker_delete_file_removes_existing() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("gone.rs");
    fs::write(&p, "bye\n").unwrap();
    let patch = format!("*** Begin Patch\n*** Delete File: {path}\n*** End Patch\n", path = fwd(&p));
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["files_deleted"], 1);
    assert!(!p.exists());
}

#[test]
fn marker_delete_file_errors_when_missing() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("missing.rs");
    let patch = format!("*** Delete File: {path}\n", path = fwd(&p));
    let err = apply_patch(&ApplyPatchArgs { patch }).unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Io);
}

#[test]
fn marker_update_file_applies_hunk() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("u.rs");
    fs::write(&p, "fn foo() {}\nfn bar() {}\n").unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {path}\n@@ -1,2 +1,2 @@\n-fn foo() {{}}\n+fn FOO() {{}}\n fn bar() {{}}\n*** End Patch\n",
        path = fwd(&p)
    );
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["files_updated"], 1);
    assert_eq!(fs::read_to_string(&p).unwrap(), "fn FOO() {}\nfn bar() {}\n");
}

#[test]
fn marker_update_then_move_renames_after_edit() {
    let dir = tempdir().unwrap();
    let from = dir.path().join("from.rs");
    let to = dir.path().join("renamed/to.rs");
    fs::write(&from, "fn foo() {}\n").unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Update File: {from}\n*** Move to: {to}\n@@ -1,1 +1,1 @@\n-fn foo() {{}}\n+fn FOO() {{}}\n*** End Patch\n",
        from = fwd(&from),
        to = fwd(&to)
    );
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["files_updated"], 1);
    assert_eq!(out["files_moved"], 1);
    assert!(!from.exists());
    assert_eq!(fs::read_to_string(&to).unwrap(), "fn FOO() {}\n");
}

#[test]
fn marker_mixed_multi_file_patch() {
    let dir = tempdir().unwrap();
    let to_update = dir.path().join("update.rs");
    let to_delete = dir.path().join("delete.rs");
    let to_add = dir.path().join("add.rs");
    fs::write(&to_update, "let x = 1;\n").unwrap();
    fs::write(&to_delete, "trash\n").unwrap();
    let patch = format!(
        "*** Begin Patch\n\
         *** Add File: {add}\n+brand new\n\
         *** Update File: {upd}\n@@ -1,1 +1,1 @@\n-let x = 1;\n+let x = 2;\n\
         *** Delete File: {del}\n\
         *** End Patch\n",
        add = fwd(&to_add),
        upd = fwd(&to_update),
        del = fwd(&to_delete)
    );
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    assert_eq!(out["files_added"], 1);
    assert_eq!(out["files_updated"], 1);
    assert_eq!(out["files_deleted"], 1);
    assert_eq!(fs::read_to_string(&to_add).unwrap(), "brand new\n");
    assert_eq!(fs::read_to_string(&to_update).unwrap(), "let x = 2;\n");
    assert!(!to_delete.exists());
}

#[test]
fn marker_invalid_op_rolls_back_everything() {
    let dir = tempdir().unwrap();
    let to_delete = dir.path().join("present.rs");
    let to_add = dir.path().join("would_add.rs");
    let missing = dir.path().join("does_not_exist.rs");
    fs::write(&to_delete, "keep me\n").unwrap();
    // The Delete of `missing` is invalid; the whole patch must abort with no
    // file added and no file removed.
    let patch = format!(
        "*** Begin Patch\n\
         *** Add File: {add}\n+oops\n\
         *** Delete File: {present}\n\
         *** Delete File: {missing}\n\
         *** End Patch\n",
        add = fwd(&to_add),
        present = fwd(&to_delete),
        missing = fwd(&missing)
    );
    let err = apply_patch(&ApplyPatchArgs { patch }).unwrap_err();
    assert_eq!(err.class, origin_tools::ErrClass::Io);
    // Nothing changed on disk.
    assert!(!to_add.exists());
    assert!(to_delete.exists());
    assert_eq!(fs::read_to_string(&to_delete).unwrap(), "keep me\n");
}

#[test]
fn unified_diff_path_still_returns_legacy_shape() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("legacy.rs");
    fs::write(&p, "a\nb\n").unwrap();
    let patch = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,1 +1,1 @@\n-a\n+A\n",
        path = fwd(&p)
    );
    let out = apply_patch(&ApplyPatchArgs { patch }).unwrap();
    // Backward-compat: pure unified diff keeps the {ok, files_updated} shape
    // and does NOT grow the marker-only fields.
    assert_eq!(out["files_updated"], 1);
    assert!(out.get("files_added").is_none());
}
