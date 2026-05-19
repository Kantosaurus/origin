//! Permission engine: tier-based check with a pluggable `Prompter`.
//!
//! `AutoAllowed` tools bypass the prompter; `RequiresPermission` tools ask.
//! Later phases add user-configured wildcard rules (P10) and a bloom-filter
//! pre-check (spec N9.2).

pub mod prompt;

use origin_tools::{Tier, ToolMeta};
use prompt::Prompter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Deny,
}

#[derive(Debug)]
pub struct Decision {
    pub outcome: Outcome,
    pub reason: String,
}

/// Decide whether `meta`'s invocation with `args_preview` is allowed.
///
/// `args_preview` is a short human-readable summary of the tool's input
/// (e.g., `"/path/to/file"` for Read or `"git status"` for Bash).
pub async fn check(meta: &ToolMeta, args_preview: &str, prompter: &dyn Prompter) -> Decision {
    match meta.tier {
        Tier::AutoAllowed => Decision {
            outcome: Outcome::Allow,
            reason: "tier=AutoAllowed".into(),
        },
        Tier::RequiresPermission => {
            let allowed = prompter.ask(meta, args_preview).await;
            Decision {
                outcome: if allowed { Outcome::Allow } else { Outcome::Deny },
                reason: if allowed {
                    "user-approved".into()
                } else {
                    "user-denied".into()
                },
            }
        }
    }
}
