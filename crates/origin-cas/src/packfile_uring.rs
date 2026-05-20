//! `tokio-uring`-backed pack-file read path.
//!
//! Index walk reuses the existing mmap-resident `PackReader::find(hash)` and
//! returns the payload `(offset, len)`. The payload read is the only operation
//! that issues an io_uring SQE.
//!
//! This module is Linux-only and gated on the `uring` cargo feature. On other
//! platforms (and without the feature), the crate falls back to the standard
//! mmap-based `PackReader::read` path defined in `packfile.rs`.

#![cfg(all(target_os = "linux", feature = "uring"))]

use crate::{Hash, PackError, PackReader};
use std::path::Path;
use tokio_uring::fs::{File, OpenOptions};

/// Look up `hash` via the mmap'd index and read the payload via io_uring.
///
/// # Errors
/// - `PackError::Truncated` if the hash isn't in the index or the index
///   entry's range falls short of EOF.
/// - `PackError::Io` for any uring submission error.
pub async fn read_at_uring(reader: &PackReader, hash: Hash) -> Result<Vec<u8>, PackError> {
    let entry = reader.find(&hash).ok_or(PackError::Truncated)?;
    read_offset_len(reader.path(), entry.offset, entry.len).await
}

pub(crate) async fn read_offset_len(path: &Path, offset: u64, len: u32) -> Result<Vec<u8>, PackError> {
    let file = File::open(path).await?;
    let buf = vec![0u8; len as usize];
    let (res, buf) = file.read_at(buf, offset).await;
    let n = res?;
    if n != len as usize {
        return Err(PackError::Truncated);
    }
    let _ = file.close().await;
    Ok(buf)
}

/// Async writer helper — appends a vector of payloads to a brand-new pack file.
///
/// Mirrors the on-disk format produced by [`crate::PackBuilder`]:
///
/// ```text
/// magic     : 4 bytes ("OCPK")
/// version   : u16 BE
/// reserved  : u16 BE
/// payloads  : repeated [hash:32][len:u32 BE][bytes:len]
/// index     : repeated [hash:32][offset:u64 BE][len:u32 BE]
/// footer    : [entries:u64 BE][index_offset:u64 BE][magic:4 "OCFT"]
/// ```
///
/// Issues a `write_at` per logical chunk (header, per-entry header, payload,
/// index, footer) so each is a single io_uring SQE.
///
/// # Errors
/// - `PackError::Truncated` if any individual payload exceeds `u32::MAX` or
///   the total file would not fit in `u64`.
/// - `PackError::Io` for any uring submission / write error, including the
///   case where `path` already exists (the file is opened `create_new`).
pub async fn write_payloads_uring(path: &Path, payloads: &[(Hash, Vec<u8>)]) -> Result<(), PackError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path).await?;

    // Header.
    let mut cursor: u64 = 0;
    let header: Vec<u8> = {
        let mut h = Vec::with_capacity(8);
        h.extend_from_slice(b"OCPK");
        h.extend_from_slice(&1u16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes()); // reserved
        h
    };
    let header_len = header.len() as u64;
    let (res, _) = file.write_all_at(header, cursor).await;
    res?;
    cursor += header_len;

    // Payloads.
    let mut index: Vec<(Hash, u64, u32)> = Vec::with_capacity(payloads.len());
    for (hash, bytes) in payloads {
        let len = u32::try_from(bytes.len()).map_err(|_| PackError::Truncated)?;
        let entry_offset = cursor;

        let mut head = Vec::with_capacity(32 + 4);
        head.extend_from_slice(hash.as_bytes());
        head.extend_from_slice(&len.to_be_bytes());
        let head_len = head.len() as u64;
        let (res, _) = file.write_all_at(head, cursor).await;
        res?;
        cursor = cursor.checked_add(head_len).ok_or(PackError::Truncated)?;

        let payload = bytes.clone();
        let payload_len = payload.len() as u64;
        let (res, _) = file.write_all_at(payload, cursor).await;
        res?;
        cursor = cursor.checked_add(payload_len).ok_or(PackError::Truncated)?;

        index.push((*hash, entry_offset, len));
    }

    // Index.
    let index_offset = cursor;
    let mut idx_bytes = Vec::with_capacity(index.len() * (32 + 8 + 4));
    for (h, off, len) in &index {
        idx_bytes.extend_from_slice(h.as_bytes());
        idx_bytes.extend_from_slice(&off.to_be_bytes());
        idx_bytes.extend_from_slice(&len.to_be_bytes());
    }
    let idx_len = idx_bytes.len() as u64;
    let (res, _) = file.write_all_at(idx_bytes, cursor).await;
    res?;
    cursor = cursor.checked_add(idx_len).ok_or(PackError::Truncated)?;

    // Footer.
    let entries = u64::try_from(index.len()).map_err(|_| PackError::Truncated)?;
    let mut footer = Vec::with_capacity(8 + 8 + 4);
    footer.extend_from_slice(&entries.to_be_bytes());
    footer.extend_from_slice(&index_offset.to_be_bytes());
    footer.extend_from_slice(b"OCFT");
    let (res, _) = file.write_all_at(footer, cursor).await;
    res?;

    let _ = file.sync_all().await;
    let _ = file.close().await;
    Ok(())
}
