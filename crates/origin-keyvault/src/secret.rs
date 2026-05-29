// SPDX-License-Identifier: Apache-2.0
//! `Secret<T>` — a guard wrapper that zeroizes on drop and redacts in `Debug`.
//!
//! `Secret` is intentionally minimal:
//!   * no `Clone` — duplicating secrets would defeat zeroize-on-drop;
//!   * no `Display` — a fmt slip should not leak the inner value;
//!   * no `Serialize`/`Deserialize` — secrets cross trust boundaries only
//!     through the [`crate::KeyVault`] façade.

use core::fmt;

use zeroize::Zeroize;

/// A value that will be wiped from memory when dropped and never printed.
pub struct Secret<T: Zeroize> {
    inner: T,
}

impl<T: Zeroize> Secret<T> {
    /// Wraps `value` so it is zeroized on drop.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self { inner: value }
    }

    /// Exposes the inner value. Callers must avoid logging or persisting it.
    #[must_use]
    pub const fn expose(&self) -> &T {
        &self.inner
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never include the inner value — the whole point of this type.
        f.write_str("Secret<redacted>")
    }
}

impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.inner.zeroize();
    }
}
