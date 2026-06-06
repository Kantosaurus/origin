// SPDX-License-Identifier: Apache-2.0
//! End-to-end test for the Phase 11 Windows ReFS reflink path.
//!
//! Compiles only on Windows. **Runs** only when `ORIGIN_REFS_TEST_DIR` is
//! set and points at a directory on a ReFS or Dev Drive volume — when the
//! env var is unset the test prints a skip message and passes immediately,
//! so the suite stays green on the typical NTFS-only developer machine.
//!
//! ## Why an env-var gate
//!
//! The reflink driver invokes `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, which is
//! rejected by NTFS with `ERROR_INVALID_FUNCTION`. The conservative fallback
//! in `reflink_tree` collapses that (and any other failure) to
//! `Error::Unsupported(_)` so the eager-copy path in
//! `Workspace::clone_into` can take over. That means we cannot tell from
//! the outside whether the FSCTL was even attempted — running the test on
//! NTFS would simply assert `Unsupported`, which proves nothing about the
//! actual FSCTL implementation. Gating on `ORIGIN_REFS_TEST_DIR` makes the
//! test self-selecting: CI / developers with a Dev Drive opt in by setting
//! the env var; everyone else skips with no false signal either way.
//!
//! ## CI wiring
//!
//! On a Windows runner with a Dev Drive provisioned (Windows 11 22H2+):
//!
//! ```yaml
//! - run: cargo test -p origin-cow --test windows_refs_reflink
//!   env:
//!     ORIGIN_REFS_TEST_DIR: D:\dev-drive\origin-test
//! ```

#![cfg(target_os = "windows")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::doc_markdown)]

use std::fs;
use std::path::PathBuf;

use origin_cow::reflink_windows::reflink_tree;

fn refs_root() -> Option<PathBuf> {
    std::env::var_os("ORIGIN_REFS_TEST_DIR").map(PathBuf::from)
}

/// Unique subdirectory per run so concurrent invocations and prior
/// failures cannot see each other's state.
fn unique_subdir(root: &std::path::Path, prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    root.join(format!("{prefix}-{nanos}"))
}

#[test]
fn refs_reflink_clones_a_small_tree_when_env_set() {
    let Some(refs_dir) = refs_root() else {
        eprintln!("skip: ORIGIN_REFS_TEST_DIR not set; need a ReFS/Dev Drive path");
        return;
    };
    fs::create_dir_all(&refs_dir).expect("mkdir refs root");

    let src = unique_subdir(&refs_dir, "src");
    let dst = unique_subdir(&refs_dir, "dst");

    // Seed src with a nested tree.
    fs::create_dir_all(src.join("nested/deeper")).expect("mkdir nested");
    fs::write(src.join("top.bin"), &b"alpha"[..]).expect("write top.bin");
    fs::write(src.join("nested/mid.bin"), &b"beta-beta"[..]).expect("write mid.bin");
    fs::write(src.join("nested/deeper/leaf.bin"), &b"gamma-gamma-gamma"[..]).expect("write leaf");

    // Call the real FSCTL path. On a true ReFS volume this should succeed;
    // any error means the implementation is wrong (because the env var
    // promised we're on ReFS).
    reflink_tree(&src, &dst).unwrap_or_else(|e| {
        // Clean up before panicking so a failed run does not leak gigabytes
        // of test data into the user's Dev Drive.
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
        panic!(
            "reflink_tree failed on ORIGIN_REFS_TEST_DIR={} — verify the path \
             is actually on ReFS/Dev Drive: {e}",
            refs_dir.display()
        );
    });

    // Contents must match byte-for-byte.
    assert_eq!(fs::read(dst.join("top.bin")).expect("read top"), b"alpha");
    assert_eq!(
        fs::read(dst.join("nested/mid.bin")).expect("read mid"),
        b"beta-beta"
    );
    assert_eq!(
        fs::read(dst.join("nested/deeper/leaf.bin")).expect("read leaf"),
        b"gamma-gamma-gamma"
    );

    // Mutate src; dst must NOT see the change (block-level COW isolation).
    fs::write(src.join("top.bin"), &b"alpha-modified"[..]).expect("rewrite top.bin");
    assert_eq!(
        fs::read(dst.join("top.bin")).expect("re-read dst top"),
        b"alpha",
        "post-clone write to src leaked into dst — isolation contract violated"
    );

    // Best-effort cleanup. Don't unwrap — the test's primary assertion
    // already passed; leaking the test dir is acceptable if Windows holds
    // a transient handle.
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dst);
}
