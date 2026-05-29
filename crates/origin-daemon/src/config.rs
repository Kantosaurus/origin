// SPDX-License-Identifier: Apache-2.0
//! Daemon-wide configuration knobs sourced from env vars.
//!
//! Each accessor here is a small free function so it can be unit-tested
//! without spinning up the rest of the daemon. The binary in `main.rs`
//! re-exports them where it needs them.

/// Resolve the bearer TTL (seconds) surfaced in
/// [`StreamEvent::PairIssued`](crate::protocol::StreamEvent::PairIssued).
///
/// Default: one day ([`origin_mem::SECS_PER_DAY`]). Overridable via the
/// `ORIGIN_BEARER_TTL_SECS` env var. Saturates at `u32::MAX` — the wire
/// field is a `u32`. Non-numeric overrides are ignored.
#[must_use]
pub fn bearer_ttl_secs() -> u32 {
    if let Ok(raw) = std::env::var("ORIGIN_BEARER_TTL_SECS") {
        if let Ok(n) = raw.parse::<u32>() {
            return n;
        }
    }
    u32::try_from(origin_mem::SECS_PER_DAY).unwrap_or(u32::MAX)
}
