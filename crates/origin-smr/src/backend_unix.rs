// SPDX-License-Identifier: Apache-2.0
//! POSIX shared-memory backend (Linux + macOS).
//!
//! Uses `shm_open` + `ftruncate` + `mmap`. Names are transformed to
//! start with `/` per POSIX shm convention.

use core::ffi::c_void;
use core::num::NonZeroUsize;
use core::ptr::NonNull;
use std::os::fd::{AsRawFd, OwnedFd};

use nix::fcntl::OFlag;
use nix::sys::mman::{mmap, munmap, shm_open, shm_unlink, MapFlags, ProtFlags};
use nix::sys::stat::Mode;
use nix::unistd::ftruncate;

use crate::ring::{Mapping, Ring, MIN_CAPACITY};
use crate::wrap::zero_cursors;
use crate::{RingConfig, RingError};

fn posix_name(name: &str) -> String {
    if name.starts_with('/') {
        name.to_owned()
    } else {
        format!("/{name}")
    }
}

/// Open (or create) a POSIX shared mapping. Caller upholds the SPSC
/// contract.
// Takes `RingConfig` by value to mirror the Windows backend's signature (the
// `Ring::open` dispatcher hands the owned config to whichever backend is
// compiled in); this path only reads its fields.
#[allow(clippy::needless_pass_by_value)]
pub fn open(cfg: RingConfig) -> Result<Ring, RingError> {
    if cfg.capacity_bytes < MIN_CAPACITY {
        return Err(RingError::CreationFailed(format!(
            "capacity {} below MIN_CAPACITY {MIN_CAPACITY}",
            cfg.capacity_bytes
        )));
    }
    let cap = cfg.capacity_bytes;
    let pname = posix_name(&cfg.name);

    let flags = if cfg.create {
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR
    } else {
        OFlag::O_RDWR
    };
    let fd: OwnedFd = shm_open(pname.as_str(), flags, Mode::S_IRUSR | Mode::S_IWUSR)
        .map_err(|e| RingError::CreationFailed(format!("shm_open({pname}): {e}")))?;

    if cfg.create {
        // On any failure after the O_CREAT|O_EXCL shm_open, unlink the freshly
        // created name so it doesn't leak in the shm namespace (which would also
        // make the next O_EXCL create fail with EEXIST).
        let len_off = match i64::try_from(cap) {
            Ok(v) => v,
            Err(_) => {
                let _ = shm_unlink(pname.as_str());
                return Err(RingError::CreationFailed("capacity exceeds i64".into()));
            }
        };
        if let Err(e) = ftruncate(&fd, len_off) {
            let _ = shm_unlink(pname.as_str());
            return Err(RingError::CreationFailed(format!("ftruncate: {e}")));
        }
    }

    let length = NonZeroUsize::new(cap).ok_or_else(|| RingError::CreationFailed("capacity is 0".into()))?;
    let mmap_result;
    // SAFETY: `fd` is a valid shared-memory descriptor sized to
    // exactly `cap` bytes (truncate above on the create side, or
    // already sized by the producer on the open side). Mapping a
    // shared region with PROT_READ|PROT_WRITE is the documented call.
    unsafe {
        mmap_result = mmap(
            None,
            length,
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_SHARED,
            &fd,
            0,
        );
    }
    let ptr = match mmap_result {
        Ok(p) => p,
        Err(e) => {
            if cfg.create {
                let _ = shm_unlink(pname.as_str());
            }
            return Err(RingError::MmapFailed(format!("mmap: {e}")));
        }
    };

    // We no longer need the fd — the mapping itself keeps the shared
    // memory alive. Drop happens implicitly when `fd` goes out of
    // scope. Borrow it once to suppress unused warnings.
    let _ = fd.as_raw_fd();
    drop(fd);

    // nix 0.29 `mmap` returns `NonNull<c_void>`; project it to the byte
    // pointer the ring works in.
    let raw_ptr = ptr.as_ptr().cast::<u8>();
    if cfg.create {
        // SAFETY: just-truncated mapping of `cap >= MIN_CAPACITY`
        // bytes; zeroing the first 128 cursor bytes is in-bounds.
        unsafe { zero_cursors(raw_ptr) };
    }

    Ok(Ring {
        mapping: Mapping {
            ptr: raw_ptr,
            capacity: cap,
            owns_name: cfg.create,
            raw_name: pname,
            native_handle: 0,
        },
    })
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // `Mapping::capacity` is `>= MIN_CAPACITY > 0` by construction
        // (validated in `open`); the fallback to 1 is unreachable but
        // avoids panicking from `Drop`.
        let length = NonZeroUsize::new(self.capacity).map_or(
            // Safe non-zero fallback; never actually taken.
            // `NonZeroUsize::MIN` is `1`.
            NonZeroUsize::MIN,
            |n| n,
        );
        // nix 0.29 `munmap` takes `NonNull<c_void>`. `self.ptr` came from
        // `mmap` (non-null by construction), so this `new` never yields None.
        if let Some(addr) = NonNull::new(self.ptr.cast::<c_void>()) {
            // SAFETY: `addr` was returned by `mmap` for this mapping with
            // exactly `capacity` bytes; `munmap` consumes it once.
            unsafe {
                let _ = munmap(addr, length.get());
            }
        }
        if self.owns_name {
            let _ = shm_unlink(self.raw_name.as_str());
        }
    }
}
