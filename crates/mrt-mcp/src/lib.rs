pub mod client;
pub mod protocol;
pub mod server;

pub use client::{McpHostManager, McpServerConfig, McpServerStatus};
pub use protocol::{JsonRpcRequest, JsonRpcResponse, McpResource, McpTool, McpToolCallResult};
pub use server::{handle_mcp_jsonrpc, run_stdio_mcp_server, MrtMcpServerState};
