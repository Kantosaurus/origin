//! End-to-end dispatch: emit a [`LifecycleEvent`] JSON line to a hook script
//! via [`ShellPool::dispatch`], then parse stdout back into a [`HookOverride`].

use crate::event::{parse_hook_stdout, HookOverride, HookParseError, LifecycleEvent};
use crate::shellpool::{PoolError, ShellPool};
use thiserror::Error;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("pool: {0}")]
    Pool(#[from] PoolError),
    #[error("serialize: {0}")]
    Ser(#[from] serde_json::Error),
    #[error("parse: {0}")]
    Parse(#[from] HookParseError),
}

/// Send `event` to `pool` and return the parsed override.
///
/// The hook script is expected to read **one JSON line** from stdin and write
/// **one JSON object followed by a NUL byte** to stdout. Empty stdout means
/// passthrough.
///
/// # Errors
/// Forwards [`DispatchError`].
#[allow(clippy::module_name_repetitions)]
pub async fn dispatch_event(pool: &ShellPool, event: &LifecycleEvent) -> Result<HookOverride, DispatchError> {
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let bytes = pool.dispatch(&line).await?;
    Ok(parse_hook_stdout(&bytes)?)
}
