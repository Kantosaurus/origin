//! Flag parser for `/goal --max-iter=N --budget=200k <cond>`. Implemented in Task 3.

use thiserror::Error;

#[derive(Debug)]
pub struct GoalArgs;

#[derive(Debug, Error)]
pub enum FlagParseError {
    #[error("placeholder")]
    Placeholder,
}

pub fn parse_goal_args(_raw: &str) -> Result<GoalArgs, FlagParseError> {
    Err(FlagParseError::Placeholder)
}
