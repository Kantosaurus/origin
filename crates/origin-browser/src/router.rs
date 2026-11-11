// SPDX-License-Identifier: Apache-2.0
//! Primary/fallback browser router.
//!
//! Policy: try `agent-browser` first. If the classifier flags the response as
//! bot-detected and the stealth fallback is enabled (`ORIGIN_STEALTH_FALLBACK=1`,
//! D11), replay the same verb against `CloakBrowser` and emit that response
//! instead. After two consecutive Cloak fallbacks in a session, mark the session
//! sticky so future verbs skip primary entirely. Without the opt-in the Cloak
//! sidecar is never spawned and a bot-detected response is returned as is.

use crate::agent_browser::AgentBrowserClient;
use crate::cloak::CloakClient;
use crate::detectors::{classify, Verdict};
use crate::protocol::{SnapshotResp, Verb};
use std::collections::HashMap;
use thiserror::Error;

// public error type for this module; the `Router` prefix is the documented
// API surface and surfaces in user-facing log lines.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum RouterError {
    #[error("primary: {0}")]
    Primary(String),
    #[error("fallback: {0}")]
    Fallback(String),
}

#[derive(Default)]
struct SessionState {
    cloak_streak: u8,
    sticky: bool,
}

/// D11: whether the stealth (`CloakBrowser`) fallback is enabled. Opt-in:
/// only `ORIGIN_STEALTH_FALLBACK=1` turns it on.
#[must_use]
pub fn stealth_fallback_enabled() -> bool {
    std::env::var("ORIGIN_STEALTH_FALLBACK").as_deref() == Ok("1")
}

// public router type; `Browser` prefix is the documented API surface and
// pairs with `BrowserClient`s the consumer constructs above.
#[allow(clippy::module_name_repetitions)]
pub struct BrowserRouter {
    primary: AgentBrowserClient,
    /// `None` unless the stealth fallback is enabled (D11).
    cloak: Option<CloakClient>,
    state: HashMap<String, SessionState>,
}

impl BrowserRouter {
    /// Production constructor: spawn real CLIs. The Cloak sidecar is spawned
    /// only when [`stealth_fallback_enabled`].
    ///
    /// # Errors
    /// Forwards spawn errors from either backend.
    pub async fn new() -> Result<Self, RouterError> {
        let primary = AgentBrowserClient::spawn()
            .await
            .map_err(|e| RouterError::Primary(e.to_string()))?;
        let cloak = if stealth_fallback_enabled() {
            Some(
                CloakClient::spawn()
                    .await
                    .map_err(|e| RouterError::Fallback(e.to_string()))?,
            )
        } else {
            None
        };
        Ok(Self {
            primary,
            cloak,
            state: HashMap::new(),
        })
    }

    /// Test constructor: spawn the backends with explicit commands; `cloak:
    /// None` runs without the stealth fallback.
    ///
    /// # Errors
    /// Forwards spawn errors from either backend.
    pub async fn with_commands(
        primary: (&str, Vec<String>),
        cloak: Option<(&str, Vec<String>)>,
    ) -> Result<Self, RouterError> {
        let p_args: Vec<&str> = primary.1.iter().map(String::as_str).collect();
        let primary = AgentBrowserClient::spawn_with_command(primary.0, &p_args)
            .await
            .map_err(|e| RouterError::Primary(e.to_string()))?;
        let cloak = match cloak {
            Some((cmd, args)) => {
                let c_args: Vec<&str> = args.iter().map(String::as_str).collect();
                Some(
                    CloakClient::spawn_with_command(cmd, &c_args)
                        .await
                        .map_err(|e| RouterError::Fallback(e.to_string()))?,
                )
            }
            None => None,
        };
        Ok(Self {
            primary,
            cloak,
            state: HashMap::new(),
        })
    }

    /// Test-only introspection: did this session become sticky on Cloak?
    #[must_use]
    pub fn sticky_cloak(&self, session: &str) -> bool {
        self.state.get(session).is_some_and(|s| s.sticky)
    }

    /// Run a verb through the routing policy.
    ///
    /// # Errors
    /// Returns [`RouterError`] if both backends fail.
    pub async fn run(&mut self, verb: &Verb) -> Result<SnapshotResp, RouterError> {
        let session = session_of(verb).to_string();
        let st = self.state.entry(session.clone()).or_default();

        // `sticky` is only ever set when a Cloak client exists.
        if let (true, Some(cloak)) = (st.sticky, self.cloak.as_mut()) {
            return cloak
                .send(verb)
                .await
                .map_err(|e| RouterError::Fallback(e.to_string()));
        }

        let primary = self
            .primary
            .send(verb)
            .await
            .map_err(|e| RouterError::Primary(e.to_string()))?;
        match classify(&primary) {
            Verdict::Clean => {
                st.cloak_streak = 0;
                Ok(primary)
            }
            Verdict::BotDetected(_reason) => {
                let Some(cloak) = self.cloak.as_mut() else {
                    return Ok(primary);
                };
                let cloak_resp = cloak
                    .send(verb)
                    .await
                    .map_err(|e| RouterError::Fallback(e.to_string()))?;
                if cloak_resp.ok {
                    st.cloak_streak = st.cloak_streak.saturating_add(1);
                    if st.cloak_streak >= 2 {
                        st.sticky = true;
                    }
                }
                Ok(cloak_resp)
            }
        }
    }
}

fn session_of(v: &Verb) -> &str {
    match v {
        Verb::Open { session, .. }
        | Verb::Click { session, .. }
        | Verb::Fill { session, .. }
        | Verb::Extract { session, .. }
        | Verb::Snapshot { session }
        | Verb::Screenshot { session, .. }
        | Verb::Close { session } => session,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stealth_fallback_is_opt_in() {
        // Only test in this crate's lib that touches the variable.
        std::env::remove_var("ORIGIN_STEALTH_FALLBACK");
        assert!(!super::stealth_fallback_enabled(), "unset ⇒ off");
        std::env::set_var("ORIGIN_STEALTH_FALLBACK", "true");
        assert!(!super::stealth_fallback_enabled(), "only `1` enables it");
        std::env::set_var("ORIGIN_STEALTH_FALLBACK", "1");
        assert!(super::stealth_fallback_enabled());
        std::env::remove_var("ORIGIN_STEALTH_FALLBACK");
    }
}
