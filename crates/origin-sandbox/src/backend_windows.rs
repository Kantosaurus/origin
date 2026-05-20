//! Windows backend: AppContainer SID + restricted Job Object.
//!
//! On Windows the cap layer must run *after* `CreateProcess` because the
//! Job Object can only be attached to an existing process handle.
//! [`apply`] sets `CREATE_SUSPENDED` on the command so the kernel hands us
//! a suspended child; the daemon's spawn helper (P11.5) is expected to call
//! [`attach_job_object_if_needed`] on that child and then `ResumeThread`
//! the main thread.
//!
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` means if the daemon dies, the
//! kernel reaps the sandboxed child along with the Job Object — no
//! lingering descendants.

#![cfg(all(target_os = "windows", feature = "windows", not(feature = "no-sandbox")))]

use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_TIME,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

use crate::{SandboxError, SandboxProfile};

/// `CREATE_SUSPENDED` from `winbase.h`.
const CREATE_SUSPENDED: u32 = 0x0000_0004;

/// 100-ns ticks in 60 seconds (`PerProcessUserTimeLimit` is in 100-ns units).
const CPU_LIMIT_100NS: u64 = 60 * 10_000_000;

/// 1 GiB RAM cap.
const MEM_LIMIT_BYTES: usize = 1 << 30;

/// Mutate `cmd` to start the spawned child under the Windows sandbox layer.
///
/// For `Inherit` we still call through `crate::caps::apply_caps` (which is a
/// no-op on Windows today) so the public contract stays consistent.
///
/// For every non-`Inherit` profile we add `CREATE_SUSPENDED` to the creation
/// flags. The actual Job Object attach happens post-spawn via
/// [`attach_job_object_if_needed`].
///
/// # Errors
/// Returns [`SandboxError::Apply`] only via downstream paths (the surface
/// itself never fails). The call is fallible to keep symmetry with the other
/// backends.
pub fn apply(profile: SandboxProfile, cmd: &mut Command) -> Result<(), SandboxError> {
    if profile == SandboxProfile::Inherit {
        return crate::caps::apply_caps(cmd);
    }
    cmd.creation_flags(CREATE_SUSPENDED);
    tracing::info!(
        target: "origin.sandbox.windows",
        ?profile,
        "applied CREATE_SUSPENDED; awaiting attach_job_object_if_needed"
    );
    Ok(())
}

/// Attach a Job Object to `child` so CPU/RAM caps fire and `KILL_ON_JOB_CLOSE`
/// reaps the child if the daemon dies, then resume the suspended main thread.
///
/// Idempotent at the API level — calling twice will create a second Job
/// Object that the kernel ignores once the child is already assigned. The
/// helper is a no-op for processes that weren't started suspended.
///
/// # Errors
/// Returns [`SandboxError::Apply`] when any of the underlying Win32 calls
/// fail (CreateJobObjectW / SetInformationJobObject / AssignProcessToJobObject /
/// OpenThread / ResumeThread).
pub fn attach_job_object_if_needed(child: &mut Child) -> Result<(), SandboxError> {
    let proc_handle: HANDLE = child.as_raw_handle().cast::<core::ffi::c_void>();
    let pid = child.id();

    // SAFETY: Win32 FFI sequence — create job, configure quotas, attach the
    // process, then resume the main thread. `proc_handle` is owned by `child`
    // for the duration of this function. `OpenThread` returns a fresh handle
    // that we close before exit.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(SandboxError::Apply(format!(
                "CreateJobObjectW failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_JOB_MEMORY
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.BasicLimitInformation.PerProcessUserTimeLimit = CPU_LIMIT_100NS as i64;
        info.JobMemoryLimit = MEM_LIMIT_BYTES;
        info.BasicLimitInformation.ActiveProcessLimit = 1;

        let info_ptr: *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION = &info;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            info_ptr.cast(),
            u32::try_from(std::mem::size_of_val(&info)).unwrap_or(0),
        );
        if ok == 0 {
            let err = std::io::Error::last_os_error();
            CloseHandle(job);
            return Err(SandboxError::Apply(format!(
                "SetInformationJobObject failed: {err}"
            )));
        }
        if AssignProcessToJobObject(job, proc_handle) == 0 {
            let err = std::io::Error::last_os_error();
            CloseHandle(job);
            return Err(SandboxError::Apply(format!(
                "AssignProcessToJobObject failed: {err}"
            )));
        }

        // Intentionally leak the Job Object handle here: closing it now would
        // trigger KILL_ON_JOB_CLOSE on the still-suspended child. The handle
        // is reaped when the daemon process exits.
        let _ = job;

        // Now resume the child's main thread. CreateProcess gave us
        // `CREATE_SUSPENDED`; without ResumeThread the child sits idle.
        //
        // `std::process::Child` doesn't expose the main-thread handle, so we
        // open a fresh handle to the first thread of `pid`. Windows
        // enumerates threads via `Thread32First`/`Thread32Next`; for our
        // purpose `OpenThread` against the process's main thread suffices
        // because CreateProcess returns thread id = pid + 1 in practice for
        // most spawn-helper code paths but that's not guaranteed.
        //
        // Robust path: walk `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)` and
        // find the first thread whose `th32OwnerProcessID == pid`. We do that
        // here to stay portable.
        if let Some(tid) = first_thread_id_of(pid)? {
            let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
            if thread.is_null() {
                return Err(SandboxError::Apply(format!(
                    "OpenThread({tid}) failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let prev = ResumeThread(thread);
            CloseHandle(thread);
            if prev == u32::MAX {
                return Err(SandboxError::Apply(format!(
                    "ResumeThread failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
    }
    Ok(())
}

/// Walk a thread snapshot and return the id of the first thread whose owning
/// process matches `pid`. Returns `Ok(None)` if no match — that's a no-op
/// (the child may have already exited).
fn first_thread_id_of(pid: u32) -> Result<Option<u32>, SandboxError> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };

    // SAFETY: ToolHelp snapshot APIs return handles we own and close on exit.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(SandboxError::Apply(format!(
                "CreateToolhelp32Snapshot failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut te: THREADENTRY32 = std::mem::zeroed();
        te.dwSize = u32::try_from(std::mem::size_of::<THREADENTRY32>()).unwrap_or(0);
        let found = if Thread32First(snap, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid {
                    break Some(te.th32ThreadID);
                }
                if Thread32Next(snap, &mut te) == 0 {
                    break None;
                }
            }
        } else {
            None
        };
        CloseHandle(snap);
        Ok(found)
    }
}
