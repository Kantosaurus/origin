//! `origin-mcp` — Model Context Protocol client. Phase 10 ships JSON-RPC +
//! stdio + HTTP/SSE transports + tool registry integration + OAuth.

pub mod client;
pub mod jsonrpc;
pub mod proxy;
pub mod transport;
pub mod transport_http;
pub mod transport_stdio;

pub use client::{ClientError, ListToolsResult, McpClient, McpTool, ToolCallResult};
pub use jsonrpc::{JsonRpcError, JsonRpcId, JsonRpcRequest, JsonRpcResponse};
pub use proxy::McpToolProxy;
pub use transport::{Transport, TransportError};
pub use transport_http::HttpTransport;
pub use transport_stdio::StdioTransport;
