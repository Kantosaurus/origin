//! jemalloc backend — one MALLCTL arena per `ArenaId`. Created lazily on first
//! use; reset / destroy operate on the backing jemalloc arena.

use crate::arena_id::ArenaId;
use std::sync::Mutex;
use std::sync::OnceLock;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Per-`ArenaId` jemalloc arena index (`arenas.create` result). Allocated lazily.
static ARENAS: OnceLock<Mutex<[Option<u32>; ArenaId::COUNT]>> = OnceLock::new();

thread_local! {
    static THREAD_ARENA: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

fn arenas() -> &'static Mutex<[Option<u32>; ArenaId::COUNT]> {
    ARENAS.get_or_init(|| Mutex::new([None; ArenaId::COUNT]))
}

fn ensure_arena(id: ArenaId) -> u32 {
    let mut guard = arenas().lock().expect("arenas lock poisoned");
    if let Some(idx) = guard[id.backend_index()] {
        return idx;
    }
    // Safety: `arenas.create` writes a u32 result into the out-pointer.
    let mut new_idx: u32 = 0;
    let mut len: libc::size_t = std::mem::size_of::<u32>();
    let name = c"arenas.create";
    // SAFETY: jemalloc FFI; `mallctl` is the documented control surface.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr().cast(),
            std::ptr::from_mut::<u32>(&mut new_idx).cast(),
            std::ptr::from_mut::<libc::size_t>(&mut len),
            std::ptr::null_mut(),
            0,
        );
        assert_eq!(ret, 0, "jemalloc arenas.create failed: {ret}");
    }
    guard[id.backend_index()] = Some(new_idx);
    tracing::debug!(arena = id.label(), idx = new_idx, "jemalloc: created arena");
    new_idx
}

pub fn bind_thread_arena(id: ArenaId) -> Option<usize> {
    let prev = THREAD_ARENA.with(std::cell::Cell::get);
    let new = ensure_arena(id);
    set_thread_arena_raw(new);
    THREAD_ARENA.with(|c| c.set(Some(new)));
    prev.map(|v| v as usize)
}

pub fn restore_thread_arena(prev: Option<usize>) {
    let v: Option<u32> = prev.map(|n| u32::try_from(n).expect("arena idx fits u32"));
    if let Some(idx) = v {
        set_thread_arena_raw(idx);
        THREAD_ARENA.with(|c| c.set(Some(idx)));
    } else {
        THREAD_ARENA.with(|c| c.set(None));
    }
}

fn set_thread_arena_raw(idx: u32) {
    let name = c"thread.arena";
    let mut value: u32 = idx;
    // SAFETY: jemalloc FFI.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_mut::<u32>(&mut value).cast(),
            std::mem::size_of::<u32>(),
        );
        assert_eq!(ret, 0, "jemalloc thread.arena set failed: {ret}");
    }
}

pub fn reset_arena(id: ArenaId) -> Result<(), super::AllocError> {
    let idx = ensure_arena(id);
    let name = std::ffi::CString::new(format!("arena.{idx}.reset")).expect("arena name is valid ascii");
    // SAFETY: jemalloc FFI.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return Err(super::AllocError::Bind(id, format!("reset rc={ret}")));
        }
    }
    Ok(())
}

pub fn destroy_arena(id: ArenaId) -> Result<(), super::AllocError> {
    let idx = ensure_arena(id);
    let name = std::ffi::CString::new(format!("arena.{idx}.destroy")).expect("arena name is valid ascii");
    // SAFETY: jemalloc FFI.
    unsafe {
        let ret = tikv_jemalloc_sys::mallctl(
            name.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return Err(super::AllocError::Bind(id, format!("destroy rc={ret}")));
        }
    }
    // Forget the index — next bind allocates a fresh arena.
    let mut guard = arenas().lock().expect("arenas lock poisoned");
    guard[id.backend_index()] = None;
    Ok(())
}

/// Snapshot of per-arena resident bytes from `mallctl stats.arenas.<i>.resident`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArenaStat {
    pub resident_bytes: usize,
    pub allocated_bytes: usize,
    pub jemalloc_index: u32,
}

pub fn snapshot() -> Result<[ArenaStat; ArenaId::COUNT], super::AllocError> {
    // Refresh stats.
    let epoch = c"epoch";
    let mut ep: u64 = 1;
    let mut len: libc::size_t = std::mem::size_of::<u64>();
    // SAFETY: jemalloc FFI.
    unsafe {
        tikv_jemalloc_sys::mallctl(
            epoch.as_ptr().cast(),
            std::ptr::from_mut::<u64>(&mut ep).cast(),
            std::ptr::from_mut::<libc::size_t>(&mut len),
            std::ptr::from_mut::<u64>(&mut ep).cast(),
            std::mem::size_of::<u64>(),
        );
    }
    let mut out = [ArenaStat::default(); ArenaId::COUNT];
    let guard = arenas().lock().expect("arenas lock poisoned");
    for (slot, id_idx) in guard.iter().zip(0..ArenaId::COUNT) {
        if let Some(idx) = *slot {
            out[id_idx].jemalloc_index = idx;
            out[id_idx].resident_bytes = read_arena_stat(idx, "resident").unwrap_or(0);
            out[id_idx].allocated_bytes = read_arena_stat(idx, "small.allocated").unwrap_or(0)
                + read_arena_stat(idx, "large.allocated").unwrap_or(0);
        }
    }
    Ok(out)
}

fn read_arena_stat(idx: u32, leaf: &str) -> Option<usize> {
    let name = std::ffi::CString::new(format!("stats.arenas.{idx}.{leaf}")).expect("stat name ascii");
    let mut value: usize = 0;
    let mut len: libc::size_t = std::mem::size_of::<usize>();
    // SAFETY: jemalloc FFI.
    let ret = unsafe {
        tikv_jemalloc_sys::mallctl(
            name.as_ptr(),
            std::ptr::from_mut::<usize>(&mut value).cast(),
            std::ptr::from_mut::<libc::size_t>(&mut len),
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        Some(value)
    } else {
        None
    }
}
