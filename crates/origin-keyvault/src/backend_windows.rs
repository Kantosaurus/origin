//! Windows backend — Credential Manager via `CredWriteW` / `CredReadW` /
//! `CredDeleteW` from the `windows` crate.
//!
//! Target names use the `"origin/{provider}/{account}"` convention so the
//! eventual `CredEnumerateW`-backed `list` (P11) can filter by a stable
//! prefix. All FFI calls run on `spawn_blocking` because the Windows
//! credential APIs are synchronous.

use async_trait::async_trait;
use tokio::task::spawn_blocking;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};

use crate::{backend::Backend, Error};

// `ERROR_NOT_FOUND` (1168) lifted to `HRESULT` via `HRESULT_FROM_WIN32`.
// HRESULT bits: severity=1 (failure), facility=Win32 (0x7), code=0x0490 (1168).
// Hex `0x8007_0490` interpreted as signed i32 — written out so clippy does
// not complain about a `u32 -> i32` cast wrap.
#[allow(clippy::unreadable_literal)]
const ERROR_NOT_FOUND_HRESULT: i32 = -2_147_023_728_i32;

pub struct WindowsBackend;

impl WindowsBackend {
    pub const fn new() -> Self {
        Self
    }
}

fn target_name(provider: &str, account: &str) -> Vec<u16> {
    format!("origin/{provider}/{account}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn join_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Backend(format!("join: {e}"))
}

#[async_trait]
impl Backend for WindowsBackend {
    async fn set(&self, provider: &str, account: &str, value: &[u8]) -> Result<(), Error> {
        let mut name = target_name(provider, account);
        let mut blob = value.to_vec();
        let blob_len = u32::try_from(blob.len())
            .map_err(|_| Error::Backend("value too large for CredWriteW".to_owned()))?;

        spawn_blocking(move || {
            let cred = CREDENTIALW {
                Flags: CRED_FLAGS(0),
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(name.as_mut_ptr()),
                Comment: PWSTR::null(),
                LastWritten: FILETIME::default(),
                CredentialBlobSize: blob_len,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: PWSTR::null(),
                UserName: PWSTR::null(),
            };
            // SAFETY: `cred` references buffers (`name`, `blob`) that
            // remain valid for the duration of this call. `CredWriteW`
            // copies the data into the credential store and does not
            // retain pointers past return.
            unsafe { CredWriteW(&cred, 0) }.map_err(|e| Error::Backend(format!("CredWriteW: {e}")))
        })
        .await
        .map_err(join_err)?
    }

    async fn get(&self, provider: &str, account: &str) -> Result<Vec<u8>, Error> {
        let name = target_name(provider, account);
        let p_owned = provider.to_owned();
        let a_owned = account.to_owned();

        spawn_blocking(move || {
            let mut out_ptr: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `name` is a UTF-16 NUL-terminated buffer owned by
            // this closure and pointed to by `PCWSTR`. `out_ptr` is an out
            // parameter; on success Windows sets it to a heap allocation
            // that must be freed with `CredFree`. The pointer is consumed
            // before the free call below.
            let result = unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, 0, &mut out_ptr) };
            if let Err(e) = result {
                if e.code().0 == ERROR_NOT_FOUND_HRESULT {
                    return Err(Error::NotFound {
                        provider: p_owned,
                        account: a_owned,
                    });
                }
                return Err(Error::Backend(format!("CredReadW: {e}")));
            }

            // SAFETY: `out_ptr` is non-null on success per Win32 contract.
            // We read the blob length and pointer, then copy the bytes
            // into an owned `Vec` so the freed allocation is not aliased.
            let bytes = unsafe {
                let cred = &*out_ptr;
                let len = cred.CredentialBlobSize as usize;
                if cred.CredentialBlob.is_null() || len == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(cred.CredentialBlob, len).to_vec()
                }
            };
            // SAFETY: `out_ptr` was allocated by Windows in `CredReadW`
            // above and is freed here exactly once. After this point the
            // pointer must not be dereferenced.
            unsafe { CredFree(out_ptr.cast()) };
            Ok(bytes)
        })
        .await
        .map_err(join_err)?
    }

    async fn delete(&self, provider: &str, account: &str) -> Result<(), Error> {
        let name = target_name(provider, account);
        let p_owned = provider.to_owned();
        let a_owned = account.to_owned();

        spawn_blocking(move || {
            // SAFETY: `name` is a UTF-16 NUL-terminated buffer owned by
            // this closure for the duration of the call. `CredDeleteW`
            // does not retain the pointer past return.
            let result = unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, 0) };
            match result {
                Ok(()) => Ok(()),
                Err(e) if e.code().0 == ERROR_NOT_FOUND_HRESULT => Err(Error::NotFound {
                    provider: p_owned,
                    account: a_owned,
                }),
                Err(e) => Err(Error::Backend(format!("CredDeleteW: {e}"))),
            }
        })
        .await
        .map_err(join_err)?
    }

    async fn list(&self, _provider: &str) -> Result<Vec<String>, Error> {
        // P11 will wire `CredEnumerateW` for proper enumeration.
        Ok(Vec::new())
    }
}
