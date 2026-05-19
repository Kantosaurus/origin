//! macOS reflink driver — `clonefile(2)`.
//!
//! APFS supports cheap copy-on-write clones via the `clonefile` syscall.
//! We invoke it directly through `extern "C"` rather than pulling in a
//! heavier wrapper crate; the call is the entire fast path.
//!
//! This file is only compiled on `target_os = "macos"`.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::Error;

extern "C" {
    fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32) -> libc::c_int;
}

/// Reflink-copy `src` to `dst`, recursively.
///
/// # Errors
/// Returns [`Error::Unsupported`] on any per-file `clonefile` failure
/// so the caller can fall back to eager copy. Returns [`Error::Io`]
/// for plain filesystem errors before the syscall is even reached.
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
    let src_c = CString::new(src.as_os_str().as_bytes())
        .map_err(|e| Error::Unsupported(format!("src path contains NUL: {e}")))?;
    let dst_c = CString::new(dst.as_os_str().as_bytes())
        .map_err(|e| Error::Unsupported(format!("dst path contains NUL: {e}")))?;
    // SAFETY: `src_c` and `dst_c` are owned `CString`s that live until
    // the end of this function. Their `as_ptr()` returns a valid,
    // NUL-terminated `*const c_char` pointing at memory we own. The
    // `clonefile` syscall reads both pointers and returns an `int`; it
    // does not retain the pointers past return.
    let rc = unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "clonefile failed for {} -> {}: errno {}",
            src.display(),
            dst.display(),
            std::io::Error::last_os_error()
        )))
    }
}
