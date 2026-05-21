#![allow(dead_code)]
// Task B7 fills this in.
//
// Temporary stub so lib.rs re-exports compile.
use crate::protocol::SnapshotResp;

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("stub: router not yet implemented")]
    Stub,
}

pub struct BrowserRouter;

impl BrowserRouter {
    /// # Errors
    /// Always returns `RouterError::Stub` until Task B7 lands.
    pub async fn run(&mut self, _verb: &crate::protocol::Verb) -> Result<SnapshotResp, RouterError> {
        Err(RouterError::Stub)
    }
}
