// SPDX-License-Identifier: Apache-2.0
//! Content-addressed hash type backed by blake3.

use core::fmt;

/// A 32-byte blake3 hash. The canonical CAS address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Hash([u8; 32]);

impl Hash {
    /// Hash an arbitrary byte slice.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Wrap an existing 32-byte hash.
    #[must_use]
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Borrow the raw 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
