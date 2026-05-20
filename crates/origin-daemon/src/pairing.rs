//! Pairing state machine for P13 remote-IPC bring-up.
//!
//! The daemon generates short-lived 6-digit numeric codes that a remote
//! client redeems via QUIC. On a successful redeem the daemon mints an
//! opaque `orb_…` bearer token bound to the client-supplied `device_id`.
//! Tokens are kept in-memory; the daemon also mirrors them into the
//! `KeyVault` under the `origin-remote` service so they survive restarts
//! (wiring in P13.2.3).
//!
//! All operations are O(1) under a single `parking_lot::Mutex`; the
//! state is small (one pending code + one bearer per device) so lock
//! contention is not a concern.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rand::Rng;
use thiserror::Error;

/// A pending pairing session that has not yet been redeemed.
#[derive(Debug, Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct PairingSession {
    pub code: String,
    pub created_at: Instant,
    pub expires_at: Instant,
}

/// A bearer token minted on successful redeem.
//
// The bearer string itself is the `HashMap<String, BearerToken>` key in
// `Inner::issued` — we do not duplicate it as a struct field, both to
// keep the per-token footprint minimal and to avoid leaking the bearer
// through any future `Debug`/`tracing` of this struct. The P11.14
// secret-redaction lint enforces this discipline.
#[derive(Debug, Clone)]
pub struct BearerToken {
    pub device_id: String,
    pub issued_at: Instant,
}

/// Outcome of [`Pairing::redeem`].
#[derive(Debug, Clone)]
pub enum RedeemResult {
    Issued { bearer: String, device_id: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
#[allow(clippy::module_name_repetitions)]
pub enum PairingError {
    #[error("unknown or already-redeemed code")]
    UnknownCode,
    #[error("code expired")]
    Expired,
}

#[derive(Default)]
pub struct Pairing {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    pending: HashMap<String, PairingSession>,
    issued: HashMap<String, BearerToken>,
}

impl Pairing {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Generate a new pending pairing code valid for `ttl`.
    pub fn start(&self, ttl: Duration) -> PairingSession {
        let code = generate_code();
        let now = Instant::now();
        let session = PairingSession {
            code: code.clone(),
            created_at: now,
            expires_at: now + ttl,
        };
        self.inner.lock().pending.insert(code, session.clone());
        session
    }

    /// Redeem a previously-started code, binding it to `device_id` and
    /// minting an `orb_…` bearer. Codes are single-use: the entry is
    /// removed from the pending map before any expiry check, so a stale
    /// code returns `Expired` once and `UnknownCode` thereafter.
    ///
    /// # Errors
    /// Returns [`PairingError::UnknownCode`] if the code was never
    /// issued (or already redeemed), or [`PairingError::Expired`] if the
    /// code's TTL elapsed before redeem.
    pub fn redeem(&self, code: &str, device_id: &str) -> Result<RedeemResult, PairingError> {
        let bearer = {
            let mut inner = self.inner.lock();
            let session = inner.pending.remove(code).ok_or(PairingError::UnknownCode)?;
            if Instant::now() > session.expires_at {
                return Err(PairingError::Expired);
            }
            let bearer = generate_bearer();
            inner.issued.insert(
                bearer.clone(),
                BearerToken {
                    device_id: device_id.to_string(),
                    issued_at: Instant::now(),
                },
            );
            bearer
        };
        Ok(RedeemResult::Issued {
            bearer,
            device_id: device_id.to_string(),
        })
    }

    /// Look up the device id associated with a previously-minted bearer.
    /// Returns `None` if the token is unknown (or was revoked).
    #[must_use]
    pub fn validate_bearer(&self, token: &str) -> Option<String> {
        self.inner.lock().issued.get(token).map(|t| t.device_id.clone())
    }
}

fn generate_code() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{n:06}")
}

fn generate_bearer() -> String {
    let mut bytes = [0_u8; 24];
    rand::thread_rng().fill(&mut bytes);
    format!("orb_{}", hex::encode(bytes))
}
