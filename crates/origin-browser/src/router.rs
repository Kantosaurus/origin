//! Primary/fallback browser router.
//!
//! Policy: try `agent-browser` first. If the classifier flags the response as
//! bot-detected, replay the same verb against `CloakBrowser` and emit that
//! response instead. After two consecutive Cloak fallbacks in a session, mark
//! the session sticky so future verbs skip primary entirely.

use crate::agent_browser::AgentBrowserClient;
use crate::cloak::CloakClient;
use crate::detectors::{classify, Verdict};
use crate::protocol::{SnapshotResp, Verb};
use std::collections::HashMap;
use thiserror::Error;

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

pub struct BrowserRouter {
    primary: AgentBrowserClient,
    cloak: CloakClient,
    state: HashMap<String, SessionState>,
}

impl BrowserRouter {
    /// Production constructor: spawn real CLIs.
    ///
    /// # Errors
    /// Forwards spawn errors from either backend.
    pub async fn new() -> Result<Self, RouterError> {
        let primary = AgentBrowserClient::spawn().await.map_err(|e| RouterError::Primary(e.to_string()))?;
        let cloak = CloakClient::spawn().await.map_err(|e| RouterError::Fallback(e.to_string()))?;
        Ok(Self { primary, cloak, state: HashMap::new() })
    }

    /// Test constructor: spawn both backends with explicit commands.
    ///
    /// # Errors
    /// Forwards spawn errors from either backend.
    pub async fn with_commands(
        primary: (&str, Vec<String>),
        cloak: (&str, Vec<String>),
    ) -> Result<Self, RouterError> {
        let p_args: Vec<&str> = primary.1.iter().map(String::as_str).collect();
        let c_args: Vec<&str> = cloak.1.iter().map(String::as_str).collect();
        let primary = AgentBrowserClient::spawn_with_command(primary.0, &p_args).await
            .map_err(|e| RouterError::Primary(e.to_string()))?;
        let cloak = CloakClient::spawn_with_command(cloak.0, &c_args).await
            .map_err(|e| RouterError::Fallback(e.to_string()))?;
        Ok(Self { primary, cloak, state: HashMap::new() })
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

        if st.sticky {
            return self.cloak.send(verb).await.map_err(|e| RouterError::Fallback(e.to_string()));
        }

        let primary = self.primary.send(verb).await.map_err(|e| RouterError::Primary(e.to_string()))?;
        match classify(&primary) {
            Verdict::Clean => {
                st.cloak_streak = 0;
                Ok(primary)
            }
            Verdict::BotDetected(_reason) => {
                let cloak_resp = self.cloak.send(verb).await.map_err(|e| RouterError::Fallback(e.to_string()))?;
                if cloak_resp.ok {
                    st.cloak_streak = st.cloak_streak.saturating_add(1);
                    if st.cloak_streak >= 2 { st.sticky = true; }
                }
                Ok(cloak_resp)
            }
        }
    }
}

fn session_of(v: &Verb) -> &str {
    match v {
        Verb::Open { session, .. } | Verb::Click { session, .. } | Verb::Fill { session, .. }
        | Verb::Extract { session, .. } | Verb::Snapshot { session } | Verb::Screenshot { session, .. }
        | Verb::Close { session } => session,
    }
}
