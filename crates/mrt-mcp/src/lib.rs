pub mod bridge;
pub mod client;
pub mod protocol;
pub mod server;

pub use bridge::{
    clear_bridge_port_file, read_bridge_endpoint, start_gui_bridge_listener, GuiBridgeEndpoint,
    try_forward_jsonrpc_to_gui,
};
pub use client::{McpHostManager, McpServerConfig, McpServerStatus};
pub use protocol::{JsonRpcRequest, JsonRpcResponse, McpResource, McpTool, McpToolCallResult};
pub use server::{
    handle_mcp_jsonrpc, run_stdio_mcp_server, GuiUiCommand, McpTabSession, MrtMcpServerState,
};
