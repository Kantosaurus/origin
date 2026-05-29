// SPDX-License-Identifier: Apache-2.0
//! Append-only pack file.
//!
//! Format (all big-endian):
//!   magic:     4 bytes ("OCPK")
//!   version:   u16
//!   reserved:  u16
//!   payloads:  repeated [hash:32][len:u32][bytes:len]
//!   index:     repeated [hash:32][offset:u64][len:u32], count = entries
//!   footer:    [entries:u64][index_offset:u64][magic:4 "OCFT"]
//!
//! Writes are append-only; readers mmap the whole file and look entries up via
//! a HashMap built from the index.

use crate::Hash;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAGIC_HEADER: [u8; 4] = *b"OCPK";
const MAGIC_FOOTER: [u8; 4] = *b"OCFT";
const VERSION: u16 = 1;

/// Errors produced by pack file I/O.
#[derive(Debug, Error)]
pub enum PackError {
    /// Underlying I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Header or footer magic bytes did not match.
    #[error("bad magic")]
    BadMagic,
    /// On-disk version is not supported by this build.
    #[error("unsupported version {0}")]
    UnsupportedVersion(u16),
    /// File is shorter than required, or an entry would read past EOF.
    #[error("truncated")]
    Truncated,
}

/// Writer for a brand-new pack file. Writes payloads as they arrive, buffers
/// the index in memory, flushes on `finalize`.
pub struct PackBuilder {
    file: BufWriter<File>,
    path: PathBuf,
    payload_cursor: u64,
    index: Vec<(Hash, u64, u32)>,
}

impl PackBuilder {
    /// Create a new pack file at `path`. Fails if it already exists.
    ///
    /// # Errors
    /// Propagates I/O errors from file creation.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let path = path.as_ref().to_path_buf();
        let mut file = BufWriter::new(OpenOptions::new().write(true).create_new(true).open(&path)?);
        file.write_all(&MAGIC_HEADER)?;
        file.write_u16::<BigEndian>(VERSION)?;
        file.write_u16::<BigEndian>(0)?; // reserved
        Ok(Self {
            file,
            path,
            payload_cursor: 4 + 2 + 2, // header length
            index: Vec::new(),
        })
    }

    /// Append a payload addressed by `hash`. Duplicate hashes are stored once
    /// at this layer — callers (Store) handle dedup before reaching here.
    ///
    /// # Errors
    /// Propagates I/O errors. Also fails if `bytes.len()` exceeds `u32::MAX`.
    pub fn append(&mut self, hash: Hash, bytes: &[u8]) -> Result<(), PackError> {
        let len = u32::try_from(bytes.len()).map_err(|_| PackError::Truncated)?;
        self.file.write_all(hash.as_bytes())?;
        self.file.write_u32::<BigEndian>(len)?;
        self.file.write_all(bytes)?;
        let entry_offset = self.payload_cursor;
        self.payload_cursor += 32 + 4 + u64::from(len);
        self.index.push((hash, entry_offset, len));
        Ok(())
    }

    /// Flush the index + footer and close the file.
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub fn finalize(mut self) -> Result<PathBuf, PackError> {
        let index_offset = self.payload_cursor;
        for (h, off, len) in &self.index {
            self.file.write_all(h.as_bytes())?;
            self.file.write_u64::<BigEndian>(*off)?;
            self.file.write_u32::<BigEndian>(*len)?;
        }
        let entries = u64::try_from(self.index.len()).unwrap_or(0);
        self.file.write_u64::<BigEndian>(entries)?;
        self.file.write_u64::<BigEndian>(index_offset)?;
        self.file.write_all(&MAGIC_FOOTER)?;
        // Drain the BufWriter into the kernel...
        self.file.flush()?;
        // ...then force the kernel to push the bytes (and metadata) out
        // to stable storage. Without this, `flush()` only moves bytes
        // from userspace into the page cache; a host crash before OS
        // writeback would leave the pack missing its index + footer and
        // the file unopenable by `PackReader`.
        self.file.get_mut().sync_all()?;
        Ok(self.path)
    }
}

/// mmap-backed reader. Holds an `Mmap` keeping the file mapped while alive.
pub struct PackReader {
    map: Mmap,
    index: HashMap<Hash, (u64, u32)>,
    path: PathBuf,
}

/// Minimal index entry: payload offset (after the in-file hash+len header) and
/// payload length in bytes. Used by alternate I/O backends (e.g. `tokio-uring`)
/// to issue a single `read_at` for the payload bytes.
#[derive(Debug, Clone, Copy)]
pub struct IndexEntry {
    /// Byte offset of the payload bytes (i.e. **past** the embedded
    /// `[hash:32][len:u32]` entry header), measured from the start of the
    /// pack file.
    pub offset: u64,
    /// Length of the payload, in bytes.
    pub len: u32,
}

impl PackReader {
    /// Open a previously-finalized pack file.
    ///
    /// # Errors
    /// Returns `PackError::BadMagic` / `UnsupportedVersion` / `Truncated` for
    /// malformed inputs; otherwise propagates I/O errors.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PackError> {
        let path_buf = path.as_ref().to_path_buf();
        let mut file = File::open(path.as_ref())?;
        let len = file.metadata()?.len();
        if len < 4 + 2 + 2 + 8 + 8 + 4 {
            return Err(PackError::Truncated);
        }

        let mut header_magic = [0u8; 4];
        file.read_exact(&mut header_magic)?;
        if header_magic != MAGIC_HEADER {
            return Err(PackError::BadMagic);
        }
        let version = file.read_u16::<BigEndian>()?;
        if version != VERSION {
            return Err(PackError::UnsupportedVersion(version));
        }

        file.seek(SeekFrom::End(-(8 + 8 + 4)))?;
        let entries = file.read_u64::<BigEndian>()?;
        let index_offset = file.read_u64::<BigEndian>()?;
        let mut footer_magic = [0u8; 4];
        file.read_exact(&mut footer_magic)?;
        if footer_magic != MAGIC_FOOTER {
            return Err(PackError::BadMagic);
        }

        // SAFETY: file is opened read-only above; mmap inherits that. No other
        // process should be mutating an in-flight pack file; concurrent
        // builders create disjoint files by name.
        let map = unsafe { Mmap::map(&file)? };

        // Each index entry is 44 bytes (32 hash + 8 offset + 4 len), so the
        // file can hold at most `map.len() / 44` of them. Clamp the preallocation
        // to that ceiling so a corrupt/huge `entries` field cannot trigger an
        // enormous (OOM) `HashMap` allocation before we even read the index.
        let entry_size = 32 + 8 + 4;
        let max_entries = map.len() / entry_size;
        let cap = usize::try_from(entries).unwrap_or(usize::MAX).min(max_entries);
        let mut index = HashMap::with_capacity(cap);
        let mut cursor = usize::try_from(index_offset).map_err(|_| PackError::Truncated)?;
        for _ in 0..entries {
            // Checked add: a corrupt `index_offset` near usize::MAX must not
            // wrap past the bounds check into an out-of-bounds slice panic.
            if cursor
                .checked_add(entry_size)
                .is_none_or(|end| end > map.len())
            {
                return Err(PackError::Truncated);
            }
            let mut h = [0u8; 32];
            h.copy_from_slice(&map[cursor..cursor + 32]);
            cursor += 32;
            let off = (&map[cursor..cursor + 8]).read_u64::<BigEndian>()?;
            cursor += 8;
            let len = (&map[cursor..cursor + 4]).read_u32::<BigEndian>()?;
            cursor += 4;
            index.insert(Hash::from_bytes(h), (off, len));
        }

        Ok(Self {
            map,
            index,
            path: path_buf,
        })
    }

    /// Iterate every hash recorded in this pack's index.
    pub fn hashes(&self) -> impl Iterator<Item = Hash> + '_ {
        self.index.keys().copied()
    }

    /// Look up a hash and return a slice into the mmap'd region. `None` if
    /// the hash isn't present.
    #[must_use]
    pub fn read(&self, hash: Hash) -> Option<PackSlice<'_>> {
        let (off, len) = self.index.get(&hash).copied()?;
        // Use checked arithmetic throughout: `off`/`len` originate from the
        // on-disk index and may be corrupt; an overflow must yield `None`, never
        // wrap past the bounds check into an out-of-bounds slice panic.
        let start = usize::try_from(off).ok()?.checked_add(32 + 4)?; // skip embedded hash+len
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        if end > self.map.len() {
            return None;
        }
        Some(PackSlice(&self.map[start..end]))
    }

    /// Path the pack was opened from. Used by alternate I/O backends
    /// (e.g. `tokio-uring`) that need to re-open the file as an async handle.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Look up the on-disk location of `hash`'s payload bytes. Returns the
    /// payload offset (skipping the embedded `[hash:32][len:u32]` entry header)
    /// and the payload length. `None` if the hash isn't in this pack.
    #[must_use]
    pub fn find(&self, hash: &Hash) -> Option<IndexEntry> {
        let (off, len) = self.index.get(hash).copied()?;
        let payload_offset = off.checked_add(32 + 4)?;
        Some(IndexEntry {
            offset: payload_offset,
            len,
        })
    }
}

/// Borrow into the mmap'd pack region. Zero-copy.
pub struct PackSlice<'a>(&'a [u8]);

impl AsRef<[u8]> for PackSlice<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}
