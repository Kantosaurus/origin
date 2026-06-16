// SPDX-License-Identifier: Apache-2.0
//! Bearer-token authorization store for P13.2 remote IPC.
//!
//! Mirrors the in-memory bearer index from [`crate::pairing`] in a form
//! that the per-connection IPC dispatcher can clone and consult on
//! every incoming request. The pairing module owns issuance; this
//! module owns validation.
//!
//! ENFORCED: the remote QUIC transport gates on this store. When the daemon is
//! configured with `ORIGIN_QUIC_BIND` it stands up a bearer-gated
//! [`origin_ipc::quic::QuicListener`]; the accept loop reads the bearer the
//! client presented and calls [`BearerStore::validate`] *before* serving any
//! `ClientMessage`, closing the connection on a missing/invalid token
//! (deny-by-default). [`BearerStore::revoke`] is wired to the admin
//! logout/revoke path. The store is populated on pair-redeem, so a freshly
//! paired device authorizes immediately. The LOCAL socket transport is
//! unauthenticated by design (it is already gated by filesystem permissions on
//! the per-instance socket); only the remote QUIC surface consults this store.
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

    /// Deny-by-default authorization gate for the remote QUIC transport.
    ///
    /// Returns `true` only when `bearer` is a currently-issued (non-revoked)
    /// token. A missing/empty/unknown/revoked token returns `false`, so the
    /// remote accept loop closes the connection rather than serving any request.
    /// This is the single enforcement point the daemon consults before bridging
    /// a remote connection into the dispatcher.
    #[must_use]
    pub fn authorized(&self, bearer: &str) -> bool {
        !bearer.is_empty() && self.inner.read().contains_key(bearer)
    }

    /// Revoke a bearer (e.g. on logout / admin removal). No-op if the
    /// token was never registered.
    pub fn revoke(&self, bearer: &str) {
        self.inner.write().remove(bearer);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::BearerStore;

    #[test]
    fn validate_gate_accepts_issued_and_denies_others() {
        let store = BearerStore::new();
        store.insert("orb_good".to_string(), "laptop".to_string());

        // A valid, issued bearer is authorized and resolves its device.
        assert!(store.authorized("orb_good"));
        assert_eq!(store.validate("orb_good").as_deref(), Some("laptop"));

        // Deny-by-default: an unknown token and an empty token are rejected.
        assert!(!store.authorized("orb_evil"), "unknown bearer must be denied");
        assert!(!store.authorized(""), "empty bearer must be denied");
        assert!(store.validate("orb_evil").is_none());
    }

    #[test]
    fn revoke_denies_a_previously_valid_bearer() {
        let store = BearerStore::new();
        store.insert("orb_good".to_string(), "laptop".to_string());
        assert!(store.authorized("orb_good"));

        store.revoke("orb_good");
        assert!(
            !store.authorized("orb_good"),
            "a revoked bearer must no longer authorize the remote path"
        );
        assert!(store.validate("orb_good").is_none());
    }
}
