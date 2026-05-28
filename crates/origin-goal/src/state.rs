//! Goal state machine types. Fully populated in Task 4.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TagOutcome {
    Met,
    InProgress { what_remains: String },
    Blocked { why: String },
    Missing,
}

// Placeholder symbols so lib.rs compiles. Replaced in Task 4.
#[derive(Debug, Clone)]
pub struct GoalState;
#[derive(Debug, Clone)]
pub enum GoalStatus { Active }
#[derive(Debug, Clone)]
pub enum ClearReason { UserSlash }
