// SPDX-License-Identifier: Apache-2.0
//! Minimal JSON-RPC 2.0 framing for MCP.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JsonRpcId {
    Num(i64),
    Str(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: JsonRpcId,
    pub method: String,
    pub params: Value,
}

impl JsonRpcRequest {
    #[must_use]
    pub fn new(id: JsonRpcId, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)] // jsonrpc field accepted from the wire but not surfaced.
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Deserialize, Error)]
#[error("jsonrpc error {code}: {message}")]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    /// Collapse `(result, error)` into a `Result<Value, JsonRpcError>`.
    ///
    /// # Errors
    /// Returns the embedded [`JsonRpcError`] when `error` is set.
    pub fn into_result(self) -> Result<Value, JsonRpcError> {
        match (self.result, self.error) {
            (Some(v), _) => Ok(v),
            (None, Some(e)) => Err(e),
            (None, None) => Err(JsonRpcError {
                code: -32603,
                message: "neither result nor error present".into(),
            }),
        }
    }
}
