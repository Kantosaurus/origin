// SPDX-License-Identifier: Apache-2.0
//! Linux reflink driver — `FICLONE` ioctl.
//!
//! On Btrfs / XFS-cow / Bcachefs the `FICLONE` ioctl (`0x40049409`)
//! reflinks the entire source file into a freshly created destination
//! file. We walk the source tree and reflink every regular file; any
//! per-file failure aborts the whole operation by returning
//! [`Error::Unsupported`], which causes `Workspace::clone_into` to
//! clean up the partial output and retry via eager copy.
//!
//! This file is only compiled on `target_os = "linux"`.

use std::fs::{self, File};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::Error;

// `FICLONE` from Linux's <linux/fs.h>:
//   #define FICLONE _IOW(0x94, 9, int)
// expands to the encoded ioctl number `0x40049409`. We declare it via
// `nix::ioctl_write_int_bad!` so the call site stays in safe Rust.
nix::ioctl_write_int_bad!(ficlone, 0x4004_9409);

/// Reflink-copy `src` to `dst`, recursively.
///
/// # Errors
/// Returns [`Error::Unsupported`] on any per-file ioctl failure so the
/// caller can fall back to eager copy. Returns [`Error::Io`] for plain
/// filesystem errors (`read_dir`, `create_dir_all`, etc.).
pub fn reflink_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    fs::create_dir_all(dst).map_err(Error::Io)?;
    for entry in fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let file_type = entry.file_type().map_err(Error::Io)?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            reflink_tree(&from, &to)?;
        } else if file_type.is_file() {
            reflink_file(&from, &to)?;
        }
    }
    Ok(())
}

fn reflink_file(src: &Path, dst: &Path) -> Result<(), Error> {
    let src_f = File::open(src).map_err(Error::Io)?;
    let dst_f = File::create(dst).map_err(Error::Io)?;
    let src_fd = src_f.as_raw_fd();
    let dst_fd = dst_f.as_raw_fd();
    // SAFETY: `dst_fd` and `src_fd` are owned by `dst_f` / `src_f`
    // which outlive the ioctl call. The `FICLONE` ioctl takes a single
    // `int` argument (the source fd) and writes to the file referenced
    // by the first fd. Both fds are valid kernel file descriptors for
    // the entire scope of this call. The macro-generated wrapper is
    // marked `unsafe` only because every nix ioctl wrapper is; we are
    // passing well-typed `RawFd` values, not raw pointers.
    let res = unsafe { ficlone(dst_fd, src_fd) };
    match res {
        Ok(_) => Ok(()),
        Err(err) => Err(Error::Unsupported(format!(
            "FICLONE failed for {} -> {}: {}",
            src.display(),
            dst.display(),
            io::Error::from(err)
        ))),
    }
}
