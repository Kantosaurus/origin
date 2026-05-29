// SPDX-License-Identifier: Apache-2.0
//! Workspace clone isolation contract.
//!
//! The fundamental invariant of `origin-cow` is that writes to a clone
//! path are **not** observable from the parent path, regardless of which
//! cloning strategy was selected. On filesystems that support reflinks
//! (Btrfs / XFS-cow / APFS / `ReFS`) this is provided by copy-on-write at
//! the block layer; on every other filesystem (NTFS without `ReFS`, ext4,
//! HFS+, tmpfs, …) the `HardlinkOverlay` fallback satisfies the same
//! contract via eager copy.

use std::fs;

use origin_cow::{Strategy, Workspace};
use tempfile::tempdir;

/// Build a small source tree with nested directories and several files
/// of differing content so the isolation assertion is non-trivial.
fn populate(root: &std::path::Path) {
    fs::create_dir_all(root.join("subdir")).expect("mkdir subdir");
    fs::create_dir_all(root.join("subdir/deep")).expect("mkdir deep");
    fs::write(root.join("a.txt"), b"alpha-original").expect("write a");
    fs::write(root.join("subdir/b.txt"), b"beta-original").expect("write b");
    fs::write(root.join("subdir/deep/c.txt"), b"gamma-original").expect("write c");
}

#[test]
fn clone_returns_a_strategy() {
    let src = tempdir().expect("src tempdir");
    let dst_holder = tempdir().expect("dst tempdir");
    populate(src.path());

    let ws = Workspace::open(src.path());
    let dst = dst_holder.path().join("clone");
    let clone = ws.clone_into(&dst).expect("clone_into");

    // Strategy is one of the two documented variants.
    match clone.strategy() {
        Strategy::Reflink | Strategy::HardlinkOverlay => {}
    }
    assert_eq!(clone.path(), dst.as_path());
}

#[test]
fn clone_copies_every_file_byte_for_byte() {
    let src = tempdir().expect("src tempdir");
    let dst_holder = tempdir().expect("dst tempdir");
    populate(src.path());

    let ws = Workspace::open(src.path());
    let dst = dst_holder.path().join("clone");
    let _clone = ws.clone_into(&dst).expect("clone_into");

    assert_eq!(fs::read(dst.join("a.txt")).expect("read a"), b"alpha-original");
    assert_eq!(
        fs::read(dst.join("subdir/b.txt")).expect("read b"),
        b"beta-original"
    );
    assert_eq!(
        fs::read(dst.join("subdir/deep/c.txt")).expect("read c"),
        b"gamma-original"
    );
}

#[test]
fn writes_to_clone_are_not_observable_from_parent() {
    let src = tempdir().expect("src tempdir");
    let dst_holder = tempdir().expect("dst tempdir");
    populate(src.path());

    let parent = Workspace::open(src.path());
    let dst = dst_holder.path().join("clone");
    let clone = parent.clone_into(&dst).expect("clone_into");

    // Mutate every file in the clone, then add a new one.
    fs::write(clone.path().join("a.txt"), b"alpha-MUTATED").expect("mutate a");
    fs::write(clone.path().join("subdir/b.txt"), b"beta-MUTATED").expect("mutate b");
    fs::write(
        clone.path().join("subdir/deep/c.txt"),
        b"gamma-MUTATED-and-longer",
    )
    .expect("mutate c");
    fs::write(clone.path().join("new.txt"), b"only-in-clone").expect("new file");

    // Parent must be byte-identical to what we wrote in `populate`.
    assert_eq!(
        fs::read(parent.path().join("a.txt")).expect("parent a"),
        b"alpha-original",
        "parent a.txt was perturbed by clone write"
    );
    assert_eq!(
        fs::read(parent.path().join("subdir/b.txt")).expect("parent b"),
        b"beta-original",
        "parent subdir/b.txt was perturbed by clone write"
    );
    assert_eq!(
        fs::read(parent.path().join("subdir/deep/c.txt")).expect("parent c"),
        b"gamma-original",
        "parent subdir/deep/c.txt was perturbed by clone write"
    );
    assert!(
        !parent.path().join("new.txt").exists(),
        "clone-only file leaked into parent"
    );
}

#[test]
fn parent_writes_after_clone_do_not_perturb_clone() {
    let src = tempdir().expect("src tempdir");
    let dst_holder = tempdir().expect("dst tempdir");
    populate(src.path());

    let parent = Workspace::open(src.path());
    let dst = dst_holder.path().join("clone");
    let clone = parent.clone_into(&dst).expect("clone_into");

    // Mutate parent after the clone has been taken.
    fs::write(parent.path().join("a.txt"), b"alpha-PARENT-AFTER").expect("parent mutate");

    assert_eq!(
        fs::read(clone.path().join("a.txt")).expect("clone a"),
        b"alpha-original",
        "clone a.txt observed a post-clone parent write"
    );
}

#[test]
fn clone_into_creates_destination_parent_chain() {
    let src = tempdir().expect("src tempdir");
    let dst_holder = tempdir().expect("dst tempdir");
    populate(src.path());

    let ws = Workspace::open(src.path());
    // Destination has a non-existent intermediate dir.
    let dst = dst_holder.path().join("nested/parent/clone");
    let clone = ws.clone_into(&dst).expect("clone_into nested");
    assert!(clone.path().join("a.txt").exists());
}

#[test]
fn cloning_an_empty_tree_succeeds() {
    let src = tempdir().expect("src tempdir");
    let dst_holder = tempdir().expect("dst tempdir");
    // No files at all — just the root.

    let ws = Workspace::open(src.path());
    let dst = dst_holder.path().join("clone");
    let clone = ws.clone_into(&dst).expect("clone empty");
    assert!(clone.path().is_dir());
}
