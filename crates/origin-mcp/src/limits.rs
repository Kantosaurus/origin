//! Inbound MCP response size cap. Enforced at the transport layer so a
//! pathological server can't OOM the daemon before JSON-RPC parsing.

use crate::TransportError;

/// Hard cap on a single MCP response body. 16 MiB matches N10.13.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Returns `Err(TransportError::TooLarge { … })` when `observed > MAX_RESPONSE_BYTES`.
///
/// # Errors
/// Returns [`TransportError::TooLarge`] on overflow.
pub const fn enforce_cap(observed: usize) -> Result<(), TransportError> {
    if observed > MAX_RESPONSE_BYTES {
        return Err(TransportError::TooLarge {
            observed,
            cap: MAX_RESPONSE_BYTES,
        });
    }
    Ok(())
}
