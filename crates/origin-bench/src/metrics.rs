use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub contestant: String,
    pub task_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub wall_ms: u64,
    pub tool_calls: u32,
    pub passed: bool,
}
