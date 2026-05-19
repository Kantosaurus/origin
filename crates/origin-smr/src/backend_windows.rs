//! Windows shared-memory backend.
//!
//! Uses `CreateFileMappingW(INVALID_HANDLE_VALUE, …)` for the
//! page-file-backed branch (no real file involved — shared name lives
//! in the kernel's section namespace, scoped to the session).
//!
//! Names map straight through; no `/` prefix transform is needed
//! (POSIX-style is for `shm_open`). We still UTF-16 NUL-terminate the
//! string before handing it to the wide-string APIs.

use crate::ring::{Mapping, Ring, MIN_CAPACITY};
use crate::wrap::zero_cursors;
use crate::{RingConfig, RingError};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Open (or create) a shared mapping. Caller upholds the SPSC contract.
pub fn open(cfg: RingConfig) -> Result<Ring, RingError> {
    if cfg.capacity_bytes < MIN_CAPACITY {
        return Err(RingError::CreationFailed(format!(
            "capacity {} below MIN_CAPACITY {MIN_CAPACITY}",
            cfg.capacity_bytes
        )));
    }
    let cap = cfg.capacity_bytes;
    let cap_u64 = cap as u64;
    let cap_high = u32::try_from(cap_u64 >> 32)
        .map_err(|_| RingError::CreationFailed("capacity_high overflow".into()))?;
    let cap_low = u32::try_from(cap_u64 & 0xFFFF_FFFF)
        .map_err(|_| RingError::CreationFailed("capacity_low overflow".into()))?;

    let name_wide = wide(&cfg.name);

    let handle: HANDLE = if cfg.create {
        let create_result;
        // SAFETY: `INVALID_HANDLE_VALUE` requests page-file backing
        // (documented behaviour). `name_wide` is a NUL-terminated
        // UTF-16 buffer owned by this scope and outlives the call.
        // `PCWSTR::null()` for the security attributes is allowed;
        // we accept the default DACL.
        unsafe {
            create_result = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                cap_high,
                cap_low,
                PCWSTR(name_wide.as_ptr()),
            );
        }
        let h = create_result.map_err(|e| RingError::CreationFailed(format!("CreateFileMappingW: {e}")))?;
        // CreateFileMappingW returns success even if a mapping with
        // that name already exists — detect that via GetLastError and
        // fail loudly so two producers don't both think they own the
        // ring.
        let last;
        // SAFETY: `GetLastError` is a thread-local read with no
        // pointer arguments; always safe to call.
        unsafe {
            last = GetLastError();
        }
        if last.0 == 183 {
            // ERROR_ALREADY_EXISTS
            // SAFETY: `h` was just produced by `CreateFileMappingW`
            // and we are the sole owner; closing it is the documented
            // way to release on error.
            unsafe {
                let _ = CloseHandle(h);
            }
            return Err(RingError::CreationFailed(format!(
                "shm `{}` already exists",
                cfg.name
            )));
        }
        h
    } else {
        let open_result;
        // SAFETY: `OpenFileMappingW` only reads `name_wide`; we keep
        // it alive in this scope. `FILE_MAP_ALL_ACCESS` matches the
        // `PAGE_READWRITE` protection used at create time. `FALSE`
        // for `binherithandle` means children won't inherit.
        unsafe {
            open_result = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name_wide.as_ptr()));
        }
        open_result.map_err(|e| RingError::CreationFailed(format!("OpenFileMappingW: {e}")))?
    };

    let view: MEMORY_MAPPED_VIEW_ADDRESS;
    // SAFETY: `handle` is a valid file-mapping handle just produced
    // above; mapping the entire region (`dwNumberOfBytesToMap == 0`
    // means "full") into our address space is the documented call.
    unsafe {
        view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, cap);
    }
    if view.Value.is_null() {
        // SAFETY: handle was just opened/created above and is still
        // valid; closing on error is required to avoid leaks.
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(RingError::MmapFailed("MapViewOfFile returned NULL".into()));
    }

    let ptr = view.Value.cast::<u8>();
    if cfg.create {
        // SAFETY: `ptr` is the base of a writable mapping of
        // `cap >= MIN_CAPACITY > PAYLOAD_OFFSET` bytes — zeroing the
        // first 128 bytes (cursors) is in-bounds.
        unsafe { zero_cursors(ptr) };
    }

    Ok(Ring {
        mapping: Mapping {
            ptr,
            capacity: cap,
            owns_name: cfg.create,
            raw_name: cfg.name,
            native_handle: handle.0 as isize,
        },
    })
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `ptr` was returned by `MapViewOfFile` for this
        // `Mapping`; `UnmapViewOfFile` consumes it exactly once.
        unsafe {
            let addr = MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr.cast(),
            };
            let _ = UnmapViewOfFile(addr);
        }
        // SAFETY: `native_handle` came from `CreateFileMappingW` /
        // `OpenFileMappingW` for this mapping; closing it once is the
        // documented release path. Both producer and consumer close
        // their own handle (Windows refcounts the section object).
        unsafe {
            let h = HANDLE(self.native_handle as *mut core::ffi::c_void);
            let _ = CloseHandle(h);
        }
        // `owns_name` is informational on Windows — there's no
        // unlink-equivalent; the section name dies once the last
        // handle closes. Touching the field keeps clippy from
        // flagging it as dead.
        let _ = self.owns_name;
        let _ = &self.raw_name;
    }
}
