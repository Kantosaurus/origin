// SPDX-License-Identifier: Apache-2.0
//! Bearer-token authorization store for P13.2 remote IPC.
//!
//! Mirrors the in-memory bearer index from [`crate::pairing`] in a form
//! that the per-connection IPC dispatcher can clone and consult on
//! every incoming request. The pairing module owns issuance; this
//! module owns validation.
//!
//! Backed by a `parking_lot::RwLock` — reads (every IPC frame for
//! authenticated transports) are concurrent; writes (post-redeem
//! `insert`, revoke) take the exclusive path.

use parking_lot::RwLock;
use std::collections::HashMap;

#[derive(Default)]
pub struct BearerStore {
    inner: RwLock<HashMap<String, String>>,
}

impl BearerStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-minted bearer and the device id it is bound to.
    pub fn insert(&self, bearer: String, device_id: String) {
        self.inner.write().insert(bearer, device_id);
    }

    /// Look up the device id associated with `bearer`. Returns `None`
    /// when the token has not been issued or has been revoked.
    #[must_use]
    pub fn validate(&self, bearer: &str) -> Option<String> {
        self.inner.read().get(bearer).cloned()
    }

    /// Revoke a bearer (e.g. on logout / admin removal). No-op if the
    /// token was never registered.
    pub fn revoke(&self, bearer: &str) {
        self.inner.write().remove(bearer);
    }
}
