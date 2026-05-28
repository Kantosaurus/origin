//! Regression test for the Windows reflink trim race.
//!
//! Before the fix, `clone_one_file` opened the destination at its final
//! path, FSCTL-duplicated a cluster-aligned (4 KiB) span, and only
//! *then* truncated back to `src_size`. A crash between FSCTL completion
//! and truncation would leave the destination at the aligned size with a
//! zero-padded garbage tail. The fix restructures the function to
//! reflink+truncate into a sibling temp file and only `MoveFileExW` it
//! into place once the trim succeeds — so the final path either does not
//! exist or is already the correct size.
//!
//! Like the sibling `windows_refs_reflink.rs` test, this test only runs
//! when `ORIGIN_REFS_TEST_DIR` is set to a ReFS / Dev Drive volume. On a
//! plain NTFS / dev workstation the FSCTL is rejected with
//! `ERROR_INVALID_FUNCTION` and we cannot observe the trim path at all,
//! so we skip cleanly rather than pretend to pass.

#![cfg(target_os = "windows")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::doc_markdown)]

use std::fs;
use std::path::PathBuf;

use origin_cow::reflink_windows::reflink_tree;

fn refs_root() -> Option<PathBuf> {
    std::env::var_os("ORIGIN_REFS_TEST_DIR").map(PathBuf::from)
}

fn unique_subdir(root: &std::path::Path, prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    root.join(format!("{prefix}-{nanos}"))
}

/// Pick a file size that (a) is non-zero so the FSCTL path actually runs
/// and (b) is *not* a multiple of the 4 KiB ReFS cluster, so any failure
/// to trim leaves an observably-too-large destination.
const PAYLOAD_LEN: usize = 4096 + 137;

#[test]
fn reflink_destination_is_trimmed_atomically() {
    let Some(refs_dir) = refs_root() else {
        eprintln!("skip: ORIGIN_REFS_TEST_DIR not set; need a ReFS/Dev Drive path to exercise FSCTL trim");
        return;
    };
    fs::create_dir_all(&refs_dir).expect("mkdir refs root");

    let src = unique_subdir(&refs_dir, "trim-src");
    let dst = unique_subdir(&refs_dir, "trim-dst");
    fs::create_dir_all(&src).expect("mkdir src");

    // Distinctive byte pattern so a stray zero-padded tail is loud if it
    // ever leaks past the truncate.
    let payload: Vec<u8> = (0..PAYLOAD_LEN).map(|i| ((i * 31 + 7) & 0xFF) as u8).collect();
    fs::write(src.join("file.bin"), &payload).expect("write src payload");

    reflink_tree(&src, &dst).unwrap_or_else(|e| {
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
        panic!(
            "reflink_tree failed on ORIGIN_REFS_TEST_DIR={} — verify the path is on ReFS/Dev Drive: {e}",
            refs_dir.display()
        );
    });

    // Final destination must be byte-exact length.
    let dst_meta = fs::metadata(dst.join("file.bin")).expect("stat dst");
    assert_eq!(
        dst_meta.len() as usize,
        PAYLOAD_LEN,
        "destination file was not trimmed back to src_size; trim race regression"
    );

    // And byte-exact content (no zero-padded tail).
    let dst_bytes = fs::read(dst.join("file.bin")).expect("read dst");
    assert_eq!(dst_bytes, payload, "destination bytes diverged from source");

    // No oversized intermediate / temp file may persist in the destination
    // directory after success — the atomic rename should have left only
    // the final name.
    let entries: Vec<_> = fs::read_dir(&dst)
        .expect("read_dir dst")
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "unexpected extra entries in dst after reflink: {entries:?}"
    );
    let only = entries[0].to_string_lossy().to_string();
    assert_eq!(only, "file.bin", "expected final name, got {only}");

    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&dst);
}
