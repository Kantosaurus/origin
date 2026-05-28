//! Wire-shape types shared with protocol + resume-token. Implemented in Task 5.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSnapshot;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagOutcomeWire;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearReasonWire;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalStatusWire;
