//! Windows reflink driver — `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for `ReFS` / Dev Drive.
//!
//! UNTESTED ON THIS HOST: implementation compiles but requires a `ReFS` or
//! Dev Drive volume to exercise. The [`Workspace::clone_into`](crate::Workspace::clone_into)
//! caller falls back to eager copy on any error (including
//! [`Error::Unsupported`]), so a broken reflink does not break workflow
//! isolation — but a broken reflink that silently succeeds without
//! copying would. Be extremely conservative about returning `Ok(())`.
//!
//! Windows reflinks are supported only on `ReFS` (and, increasingly, Dev
//! Drive volumes) via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`. The ioctl
//! requires:
//!
//! * Source and destination opened on the same volume.
//! * Destination opened with `GENERIC_WRITE | FILE_WRITE_DATA`.
//! * Per-call ranges aligned to a 4 KiB cluster boundary.
//! * Destination pre-extended to at least `SourceOffset + ByteCount`.
//!
//! On NTFS (the default Windows filesystem) the FSCTL fails with
//! `ERROR_INVALID_FUNCTION`; we translate that — and any other failure
//! — into [`Error::Unsupported`] so the caller's eager-copy fallback
//! takes over. We deliberately do **not** try to detect `ReFS` up front:
//! the FSCTL itself is the canonical signal and avoids a second-source
//! truth that could drift.
//!
//! This file is only compiled on `target_os = "windows"`.

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_FUNCTION, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileSizeEx, MoveFileExW, SetEndOfFile, SetFilePointerEx, CREATE_ALWAYS,
    FILE_ATTRIBUTE_NORMAL, FILE_BEGIN, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE};
use windows::Win32::System::IO::DeviceIoControl;

use crate::Error;

/// `ReFS`/NTFS cluster size required by the FSCTL.
///
/// `FSCTL_DUPLICATE_EXTENTS_TO_FILE` mandates 4 KiB-aligned ranges on
/// every shipping volume; we round the per-file byte count up to this
/// alignment and pre-extend the destination accordingly.
const REFS_CLUSTER: i64 = 4096;

/// HRESULT facility/code helpers — `HRESULT_FROM_WIN32(n) = 0x80070000 | (n & 0xFFFF)`
/// for `n != 0`. We check the high 16 bits to confirm it's a Win32-origin
/// HRESULT before extracting the low 16 bits as a Win32 error code.
const HRESULT_WIN32_MASK: u32 = 0xFFFF_0000;
const HRESULT_WIN32_FACILITY: u32 = 0x8007_0000;

// UNTESTED — Phase 11 ReFS reflink via FSCTL_DUPLICATE_EXTENTS_TO_FILE. Verify on a ReFS/Dev Drive before trusting in production.
/// Recursively reflink `src` onto `dst` using
/// `FSCTL_DUPLICATE_EXTENTS_TO_FILE`.
///
/// Returns [`Error::Unsupported`] when the destination volume does not
/// support extent duplication (e.g. NTFS); the caller falls back to the
/// eager-copy path in `hardlink_fallback.rs`.
///
/// # Errors
/// * [`Error::Unsupported`] — destination volume does not implement
///   the FSCTL, or any per-file step fails. We collapse every failure
///   into `Unsupported` so the caller can cleanly fall back.
/// * [`Error::Io`] — a directory walk failure that is *not* an FSCTL
///   miss (e.g. permission denied on `read_dir`).
pub fn reflink_tree(src: &Path, dst: &Path) -> Result<(), Error> {
    fs::create_dir_all(dst).map_err(Error::Io)?;
    walk_and_clone(src, dst)
}

fn walk_and_clone(src: &Path, dst: &Path) -> Result<(), Error> {
    for entry in fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let file_type = entry.file_type().map_err(Error::Io)?;
        let from = entry.path();
        let to = dst.join(entry.file_name());

        if file_type.is_dir() {
            fs::create_dir_all(&to).map_err(Error::Io)?;
            walk_and_clone(&from, &to)?;
        } else if file_type.is_file() {
            clone_one_file(&from, &to)?;
        } else {
            // Symlinks / sockets: skip silently (matches hardlink_fallback semantics).
            continue;
        }
    }
    Ok(())
}

fn clone_one_file(src: &Path, dst: &Path) -> Result<(), Error> {
    // Open source: read-only, share-read so other reflink probes can also open it.
    let src_handle = open_read(src)?;
    let _src_guard = HandleGuard(src_handle);

    // Determine source size; required to size the destination and to
    // compute the (cluster-aligned) byte count for the FSCTL.
    let mut src_size: i64 = 0;
    // SAFETY: src_handle is a valid open handle from CreateFileW; we
    // pass a pointer to a stack-allocated i64 that lives for the call.
    unsafe { GetFileSizeEx(src_handle, &mut src_size) }
        .map_err(|e| unsupported(format!("GetFileSizeEx({}): {e}", src.display())))?;

    // Reflink and trim run against a sibling temp file; we only expose
    // the final `dst` name via an atomic `MoveFileExW(..,
    // MOVEFILE_REPLACE_EXISTING)` once the trim has succeeded. This
    // closes the crash window in which the destination at `dst` was
    // cluster-aligned-sized with a zero-padded garbage tail because the
    // FSCTL had completed but the subsequent `SetEndOfFile` trim had
    // not yet been issued.
    let tmp = temp_sibling(dst);

    // Open the temp destination: writable, create-always so we own a
    // fresh file even if a previous run left an orphan temp behind.
    let dst_handle = open_write_create(&tmp)?;
    let _dst_guard = HandleGuard(dst_handle);

    if src_size == 0 {
        // Empty file: nothing to duplicate. The CreateFileW above
        // already produced a zero-length temp; drop the handle so the
        // rename can take it, then move into place.
        drop(_dst_guard);
        return rename_replacing(&tmp, dst);
    }

    // Pre-extend the destination to the cluster-aligned size required
    // by the FSCTL. Without this, `FSCTL_DUPLICATE_EXTENTS_TO_FILE` will
    // fail with ERROR_INVALID_PARAMETER on `ReFS` because the target
    // range falls past EOF.
    let aligned_size = align_up(src_size, REFS_CLUSTER);
    if let Err(e) = set_file_size(dst_handle, aligned_size) {
        let _ = fs::remove_file(&tmp);
        return Err(unsupported(format!("SetEndOfFile({}): {e}", tmp.display())));
    }

    // Issue the FSCTL. Single call covers the whole file — the kernel
    // splits internally as needed; per-call upper bound on `ReFS` is
    // multi-GiB which is more than enough for any source file we ship.
    let data = DUPLICATE_EXTENTS_DATA {
        FileHandle: src_handle,
        SourceFileOffset: 0,
        TargetFileOffset: 0,
        ByteCount: aligned_size,
    };
    let mut bytes_returned: u32 = 0;
    // SAFETY: dst_handle / src_handle are valid open kernel handles
    // held alive by the guards above. `data` is `#[repr(C)]` and lives
    // for the duration of the call. `bytes_returned` is a valid
    // stack-allocated u32. We pass `None` for the OVERLAPPED pointer
    // (synchronous call).
    let r = unsafe {
        DeviceIoControl(
            dst_handle,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            Some(std::ptr::from_ref(&data).cast::<core::ffi::c_void>()),
            u32::try_from(std::mem::size_of::<DUPLICATE_EXTENTS_DATA>()).unwrap_or(u32::MAX),
            None,
            0,
            Some(&mut bytes_returned),
            None,
        )
    };

    if let Err(e) = r {
        let hr = e.code().0;
        // ERROR_INVALID_FUNCTION (1) → 0x80070001: volume doesn't
        // support this FSCTL (NTFS, FAT32, etc). Fall back to eager copy.
        // Any other failure also falls back — we never want to leave a
        // half-written destination claiming success.
        let win32 = hresult_to_win32(hr);
        // Drop the handle and remove the temp before reporting Unsupported
        // so we never leave an oversized orphan at the final path's sibling.
        drop(_dst_guard);
        let _ = fs::remove_file(&tmp);
        if win32 == Some(ERROR_INVALID_FUNCTION.0) {
            return Err(Error::Unsupported(format!(
                "FSCTL_DUPLICATE_EXTENTS_TO_FILE not supported on dst volume ({}); \
                 falling back to eager copy",
                dst.display()
            )));
        }
        return Err(Error::Unsupported(format!(
            "FSCTL_DUPLICATE_EXTENTS_TO_FILE failed for {} → {}: {e} (hr=0x{hr:08x})",
            src.display(),
            dst.display(),
        )));
    }

    // Trim the temp back down to the exact source size; the aligned
    // tail past EOF is otherwise observable as a zero-padded suffix.
    // SetFilePointerEx + SetEndOfFile is the canonical idiom. We do
    // this *before* the rename: if a crash interrupts us here, the
    // tail-padded file lives only at the temp name and is invisible to
    // callers — the worst case is an orphan temp the next walker will
    // overwrite with CREATE_ALWAYS.
    if aligned_size != src_size {
        if let Err(e) = seek_set(dst_handle, src_size) {
            drop(_dst_guard);
            let _ = fs::remove_file(&tmp);
            return Err(unsupported(format!(
                "SetFilePointerEx({}, {src_size}): {e}",
                tmp.display()
            )));
        }
        if let Err(e) = set_eof(dst_handle) {
            drop(_dst_guard);
            let _ = fs::remove_file(&tmp);
            return Err(unsupported(format!("SetEndOfFile-trim({}): {e}", tmp.display())));
        }
    }

    // Close the handle on the temp before renaming. MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING needs no other handle holding write
    // access to the source name; closing here also flushes any pending
    // metadata on this handle.
    drop(_dst_guard);
    rename_replacing(&tmp, dst)
}

/// Build a sibling temp path next to `dst`. We keep the temp in the
/// same directory so `MoveFileExW` stays within a single volume (which
/// is required for a rename instead of a copy+delete).
fn temp_sibling(dst: &Path) -> std::path::PathBuf {
    let parent = dst.parent().unwrap_or(Path::new("."));
    // Two nanos-precision tokens collide only on the same nanosecond
    // *and* same destination — vanishingly unlikely, and the temp is
    // opened with CREATE_ALWAYS which would overwrite anyway.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = dst
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("reflink"));
    parent.join(format!(".{file_name}.reflink-tmp-{nanos}"))
}

/// Atomic rename `from` → `to`, replacing any existing `to`. Uses
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)` so
/// the directory entry update is itself durable.
fn rename_replacing(from: &Path, to: &Path) -> Result<(), Error> {
    let from_w = to_wide(from);
    let to_w = to_wide(to);
    // SAFETY: both `from_w` and `to_w` are NUL-terminated UTF-16 buffers
    // that outlive this call; `MoveFileExW` does not retain the pointers.
    let r = unsafe {
        MoveFileExW(
            PCWSTR(from_w.as_ptr()),
            PCWSTR(to_w.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(e) = r {
        // The temp still exists at `from`; remove it so we don't leak
        // an oversized orphan.
        let _ = fs::remove_file(from);
        return Err(unsupported(format!(
            "MoveFileExW({} → {}): {e}",
            from.display(),
            to.display()
        )));
    }
    Ok(())
}

/// Open a file with read access (used for the source handle).
fn open_read(path: &Path) -> Result<HANDLE, Error> {
    let wide = to_wide(path);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
    // call. `CreateFileW` does not retain the pointer.
    let r = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0x8000_0000, // GENERIC_READ
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
    };
    r.map_err(|e| {
        Error::Io(std::io::Error::other(format!(
            "CreateFileW(read, {}): {e}",
            path.display()
        )))
    })
}

/// Open a fresh writable file with the access rights required by
/// `FSCTL_DUPLICATE_EXTENTS_TO_FILE` on the called handle.
fn open_write_create(path: &Path) -> Result<HANDLE, Error> {
    let wide = to_wide(path);
    // FILE_GENERIC_WRITE includes FILE_WRITE_DATA which the FSCTL
    // requires on the destination handle.
    let access: u32 = FILE_GENERIC_WRITE.0;
    let attrs: FILE_FLAGS_AND_ATTRIBUTES = FILE_ATTRIBUTE_NORMAL;
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
    // call. `CreateFileW` does not retain the pointer.
    let r = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            access,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            attrs,
            HANDLE::default(),
        )
    };
    r.map_err(|e| {
        Error::Io(std::io::Error::other(format!(
            "CreateFileW(write, {}): {e}",
            path.display()
        )))
    })
}

fn set_file_size(handle: HANDLE, size: i64) -> windows::core::Result<()> {
    seek_set(handle, size)?;
    set_eof(handle)
}

fn seek_set(handle: HANDLE, offset: i64) -> windows::core::Result<()> {
    // SAFETY: handle is a valid open kernel handle held by the caller's guard.
    unsafe { SetFilePointerEx(handle, offset, None, FILE_BEGIN) }
}

fn set_eof(handle: HANDLE) -> windows::core::Result<()> {
    // SAFETY: handle is a valid open kernel handle held by the caller's guard.
    unsafe { SetEndOfFile(handle) }
}

const fn align_up(n: i64, align: i64) -> i64 {
    // For positive `n` and power-of-two `align`. `align` is REFS_CLUSTER (4096).
    let mask = align - 1;
    (n + mask) & !mask
}

fn to_wide(path: &Path) -> Vec<u16> {
    let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
    v.push(0);
    v
}

const fn unsupported(msg: String) -> Error {
    Error::Unsupported(msg)
}

/// Extract the Win32 error code from an `HRESULT`, or `None` if the
/// HRESULT does not encode a Win32 error (different facility, etc.).
const fn hresult_to_win32(hr: i32) -> Option<u32> {
    if hr == 0 {
        return None;
    }
    #[allow(clippy::cast_sign_loss)]
    let hr_u = hr as u32;
    if (hr_u & HRESULT_WIN32_MASK) == HRESULT_WIN32_FACILITY {
        Some(hr_u & 0xFFFF)
    } else {
        None
    }
}

/// RAII closer for raw `HANDLE`s — we never want to leak a kernel handle
/// across an early-return path, especially in the FSCTL error branches.
struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle (returned by CreateFileW) and
            // we close it at most once via this Drop impl.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}
