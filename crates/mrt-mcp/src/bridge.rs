use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::server::{handle_mcp_jsonrpc, MrtMcpServerState};
use anyhow::Result;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// JSON key of the first line a client must send on every bridge connection.
const AUTH_KEY: &str = "mrt_bridge_auth";
/// JSON-RPC error code returned when a bridge connection fails authentication.
const UNAUTHORIZED_CODE: i64 = -32001;
/// How long the listener waits for the authentication line before dropping the connection.
const AUTH_TIMEOUT: Duration = Duration::from_secs(2);

/// Address and per-session secret of a running GUI bridge listener.
#[derive(Debug, Clone)]
pub struct GuiBridgeEndpoint {
    pub addr: SocketAddr,
    pub token: String,
}

pub fn bridge_port_file_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "MRT")
        .map(|dirs| dirs.data_local_dir().join("mcp-bridge.json"))
}

/// Generates a random bridge token (two v4 UUIDs, 244 random bits, hex encoded).
fn generate_bridge_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Compares two tokens without short-circuiting on the first differing byte.
fn tokens_match(expected: &str, provided: &str) -> bool {
    let (a, b) = (expected.as_bytes(), provided.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Writes the discovery file (port, token, pid). On Unix the file is restricted to `0600`;
/// on Windows it lives in the per-user `%LOCALAPPDATA%` directory.
pub fn write_bridge_port_file(port: u16, token: &str) -> Result<()> {
    if let Some(path) = bridge_port_file_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let payload = serde_json::json!({
            "port": port,
            "token": token,
            "pid": std::process::id(),
        });
        let contents = serde_json::to_string_pretty(&payload)?;

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Tighten permissions even if the file already existed with a looser mode.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        std::io::Write::write_all(&mut file, contents.as_bytes())?;
    }
    Ok(())
}

/// Returns the `(port, token)` of a live GUI bridge, taken from the `MRT_MCP_BRIDGE_PORT` and
/// `MRT_MCP_BRIDGE_TOKEN` environment variables, or else from the discovery file.
pub fn read_bridge_endpoint() -> Option<(u16, String)> {
    if let Ok(env_port) = std::env::var("MRT_MCP_BRIDGE_PORT") {
        if let Ok(port) = env_port.trim().parse::<u16>() {
            let token = std::env::var("MRT_MCP_BRIDGE_TOKEN").ok()?;
            return Some((port, token.trim().to_string()));
        }
    }
    if cfg!(test) {
        return None;
    }
    let path = bridge_port_file_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    let port = val
        .get("port")?
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())?;
    let token = val.get("token")?.as_str()?.to_string();
    Some((port, token))
}

pub fn clear_bridge_port_file() {
    if let Some(path) = bridge_port_file_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Starts a loopback TCP listener inside the running MRT GUI process so external `mrt --mcp-server`
/// instances (or direct TCP MCP clients) can inspect and control the live terminal GUI.
///
/// Every connection must first send `{"mrt_bridge_auth":"<token>"}` on its own line, using the
/// random token generated here; connections that fail this check are closed.
pub async fn start_gui_bridge_listener(
    state: MrtMcpServerState,
    bind_port: u16,
    write_discovery_file: bool,
) -> Result<GuiBridgeEndpoint> {
    let listener = TcpListener::bind(("127.0.0.1", bind_port)).await?;
    let addr = listener.local_addr()?;
    let token = generate_bridge_token();

    if write_discovery_file {
        let _ = write_bridge_port_file(addr.port(), &token);
    }

    let listener_token = token.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            let state_clone = state.clone();
            let token_clone = listener_token.clone();
            tokio::spawn(async move {
                let _ = handle_bridge_connection(stream, state_clone, &token_clone).await;
            });
        }
    });

    Ok(GuiBridgeEndpoint { addr, token })
}

/// Returns true when `line` is a well-formed authentication line carrying the expected token.
fn is_valid_auth_line(line: &str, expected_token: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .and_then(|v| v.get(AUTH_KEY)?.as_str().map(|t| tokens_match(expected_token, t)))
        .unwrap_or(false)
}

async fn handle_bridge_connection(
    stream: TcpStream,
    state: MrtMcpServerState,
    expected_token: &str,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();

    let authorized = match tokio::time::timeout(AUTH_TIMEOUT, reader.next_line()).await {
        Ok(Ok(Some(line))) => is_valid_auth_line(&line, expected_token),
        _ => false,
    };
    if !authorized {
        let resp = JsonRpcResponse::error(
            None,
            UNAUTHORIZED_CODE,
            "Unauthorized MRT bridge connection",
        );
        let mut serialized = serde_json::to_string(&resp)?;
        serialized.push('\n');
        let _ = write_half.write_all(serialized.as_bytes()).await;
        let _ = write_half.shutdown().await;
        return Ok(());
    }

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
/// or `Err` if no live GUI bridge is reachable or it rejected the token.
pub async fn try_forward_jsonrpc_to_gui(
    port: u16,
    token: &str,
    req: &JsonRpcRequest,
) -> Result<Option<JsonRpcResponse>> {
    let stream = tokio::time::timeout(
        Duration::from_millis(250),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await??;

    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&serde_json::json!({ AUTH_KEY: token }))?;
    payload.push('\n');
    payload.push_str(&serde_json::to_string(req)?);
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;
    write_half.flush().await?;

    if req.method.starts_with("notifications/") {
        return Ok(None);
    }

    let mut reader = BufReader::new(read_half).lines();
    if let Some(line) = reader.next_line().await? {
        let resp: JsonRpcResponse = serde_json::from_str(&line)?;
        if resp.id.is_none()
            && resp
                .error
                .as_ref()
                .is_some_and(|e| e.code == UNAUTHORIZED_CODE)
        {
            anyhow::bail!("MRT GUI bridge rejected the token");
        }
        Ok(Some(resp))
    } else {
        anyhow::bail!("Bridge closed stream without response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_tokens_are_random_and_compared_exactly() {
        let a = generate_bridge_token();
        let b = generate_bridge_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(tokens_match(&a, &a.clone()));
        assert!(!tokens_match(&a, &b));
        assert!(!tokens_match(&a, &a[..63]));
        assert!(!tokens_match(&a, ""));
    }

    #[test]
    fn auth_line_requires_matching_token() {
        let token = generate_bridge_token();
        assert!(is_valid_auth_line(
            &format!(r#"{{"mrt_bridge_auth":"{token}"}}"#),
            &token
        ));
        assert!(!is_valid_auth_line(r#"{"mrt_bridge_auth":"wrong"}"#, &token));
        assert!(!is_valid_auth_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            &token
        ));
        assert!(!is_valid_auth_line("not json", &token));
    }

    #[tokio::test]
    async fn bridge_rejects_wrong_token() {
        let endpoint = start_gui_bridge_listener(MrtMcpServerState::default(), 0, false)
            .await
            .unwrap();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: "tools/list".into(),
            params: None,
        };
        let result = try_forward_jsonrpc_to_gui(endpoint.addr.port(), "wrong-token", &req).await;
        assert!(result.is_err());
    }
}
