//! `mrt server`: a headless session server.
//!
//! It owns tabs, command blocks and interactive PTYs (`mrt_pty_spawn`) in one
//! long-running process and serves them on the same token-authenticated localhost
//! bridge as the GUI (`mcp-bridge.json`). `mrt list|send|capture`, `mrt --mcp-server`
//! and agents talk to it exactly as they talk to a window, and PTY sessions keep
//! running while clients come and go: `mrt_pty_read` with `drain` returns the output
//! a client missed, once. This is the first part of Milestone 4's session server;
//! the GUI does not attach to it yet (see `docs/SESSION_SERVER.md`).

use anyhow::{bail, Result};

/// Starts serving `state` on a localhost port; with `discovery` the endpoint is
/// written to `mcp-bridge.json` so clients find it.
pub async fn serve(state: mrt_mcp::MrtMcpServerState, discovery: bool) -> Result<mrt_mcp::GuiBridgeEndpoint> {
    mrt_mcp::start_gui_bridge_listener(state, 0, discovery).await
}

/// Whether an MRT window or server already answers on the discovery file's endpoint.
async fn already_running() -> bool {
    let Some((port, token)) = mrt_mcp::read_bridge_endpoint() else { return false };
    let ping = mrt_mcp::JsonRpcRequest { jsonrpc: "2.0".into(), id: Some(serde_json::json!(1)), method: "ping".into(), params: None };
    matches!(mrt_mcp::try_forward_jsonrpc_to_gui(port, &token, &ping).await, Ok(Some(_)))
}

/// Runs the server until Ctrl+C.
pub async fn run() -> Result<()> {
    if already_running().await {
        bail!("an MRT window or server is already running; use `mrt list` to talk to it");
    }
    let endpoint = serve(mrt_mcp::MrtMcpServerState::default(), true).await?;
    eprintln!(
        "MRT session server listening on {} (pid {}). `mrt list`, `mrt send` and MCP agents use it; Ctrl+C stops it.",
        endpoint.addr,
        std::process::id()
    );
    tokio::signal::ctrl_c().await?;
    mrt_mcp::clear_bridge_port_file();
    eprintln!("MRT session server stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    async fn call(port: u16, token: &str, name: &str, arguments: Value) -> Value {
        let request = mrt_mcp::JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": arguments })),
        };
        // A new connection per call, like separate CLI invocations.
        let response = mrt_mcp::try_forward_jsonrpc_to_gui(port, token, &request).await.unwrap().unwrap();
        let text = response.result.unwrap()["content"][0]["text"].as_str().unwrap_or_default().to_string();
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    #[test]
    fn sessions_outlive_the_clients_that_started_them() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let endpoint = serve(mrt_mcp::MrtMcpServerState::default(), false).await.unwrap();
            let (port, token) = (endpoint.addr.port(), endpoint.token.clone());

            // Client 1 runs a command; client 2 sees its block.
            let ran = call(port, &token, "mrt_run_command", json!({ "command": "echo from-client-one" })).await;
            assert_eq!(ran["exit_code"], 0, "{ran}");
            let blocks = call(port, &token, "mrt_list_blocks", json!({ "tab_id": "", "limit": 5 })).await;
            assert!(blocks.to_string().contains("from-client-one"), "{blocks}");

            // Client 1 starts a PTY and leaves; client 2 writes to it and reads the output.
            let spawned = call(port, &token, "mrt_pty_spawn", json!({ "wait_ms": 300 })).await;
            let pty_id = spawned["pty_id"].as_str().or_else(|| spawned["id"].as_str()).expect("pty id").to_string();
            let _ = call(port, &token, "mrt_pty_write", json!({ "pty_id": pty_id, "input": "echo still-here", "wait_ms": 800 })).await;
            let mut seen = String::new();
            for _ in 0..20 {
                let read = call(port, &token, "mrt_pty_read", json!({ "pty_id": pty_id, "drain": false, "wait_ms": 200 })).await;
                seen = read.to_string();
                if seen.contains("still-here") {
                    break;
                }
            }
            assert!(seen.contains("still-here"), "{seen}");
            let _ = call(port, &token, "mrt_pty_kill", json!({ "pty_id": pty_id })).await;
        });
    }
}
