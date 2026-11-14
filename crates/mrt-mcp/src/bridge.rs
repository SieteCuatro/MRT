use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::server::{handle_mcp_jsonrpc, MrtMcpServerState};
use anyhow::Result;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub fn bridge_port_file_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "MRT")
        .map(|dirs| dirs.data_local_dir().join("mcp-bridge.json"))
}

pub fn write_bridge_port_file(port: u16) -> Result<()> {
    if let Some(path) = bridge_port_file_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let payload = serde_json::json!({
            "port": port,
            "pid": std::process::id(),
        });
        std::fs::write(path, serde_json::to_string_pretty(&payload)?)?;
    }
    Ok(())
}

pub fn read_bridge_port_file() -> Option<u16> {
    if let Ok(env_port) = std::env::var("MRT_MCP_BRIDGE_PORT") {
        if let Ok(port) = env_port.trim().parse::<u16>() {
            return Some(port);
        }
    }
    if cfg!(test) {
        return None;
    }
    let path = bridge_port_file_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("port")?.as_u64().and_then(|p| u16::try_from(p).ok())
}

pub fn clear_bridge_port_file() {
    if let Some(path) = bridge_port_file_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Starts a loopback TCP listener inside the running MRT GUI process so external `mrt --mcp-server`
/// instances (or direct TCP MCP clients) can inspect and control the live terminal GUI.
pub async fn start_gui_bridge_listener(
    state: MrtMcpServerState,
    bind_port: u16,
    write_discovery_file: bool,
) -> Result<SocketAddr> {
    let listener = TcpListener::bind(("127.0.0.1", bind_port)).await?;
    let addr = listener.local_addr()?;

    if write_discovery_file {
        let _ = write_bridge_port_file(addr.port());
    }

    tokio::spawn(async move {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            let state_clone = state.clone();
            tokio::spawn(async move {
                let _ = handle_bridge_connection(stream, state_clone).await;
            });
        }
    });

    Ok(addr)
}

async fn handle_bridge_connection(stream: TcpStream, state: MrtMcpServerState) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => handle_mcp_jsonrpc(&state, req).await,
            Err(e) => Some(JsonRpcResponse::error(
                None,
                -32700,
                format!("Parse error: {e}"),
            )),
        };

        if let Some(resp) = response {
            let mut serialized = serde_json::to_string(&resp)?;
            serialized.push('\n');
            write_half.write_all(serialized.as_bytes()).await?;
            write_half.flush().await?;
        }
    }

    Ok(())
}

/// Attempts to forward a JSON-RPC request to a running MRT GUI bridge on localhost.
/// Returns `Ok(Some(resp))` if a response was returned, `Ok(None)` if it was a notification,
/// or `Err` if no live GUI bridge is reachable.
pub async fn try_forward_jsonrpc_to_gui(
    port: u16,
    req: &JsonRpcRequest,
) -> Result<Option<JsonRpcResponse>> {
    let stream = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await??;

    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(req)?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    write_half.flush().await?;

    if req.method.starts_with("notifications/") {
        return Ok(None);
    }

    let mut reader = BufReader::new(read_half).lines();
    if let Some(line) = reader.next_line().await? {
        let resp: JsonRpcResponse = serde_json::from_str(&line)?;
        Ok(Some(resp))
    } else {
        anyhow::bail!("Bridge closed stream without response")
    }
}
