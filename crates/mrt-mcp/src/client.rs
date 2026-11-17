use crate::protocol::{JsonRpcRequest, McpResource, McpTool, McpToolCallResult};
use crate::server::{handle_mcp_jsonrpc, MrtMcpServerState};
use crate::transport::{
    HttpTransport, McpHttpHeader, McpTransportKind, ServerEventLog, SessionExpired,
    StdioTransport, Transport, LATEST_PROTOCOL_VERSION,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Longest MRT waits for one JSON-RPC response from an external MCP server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Upper bound on `nextCursor` pages followed by `tools/list` / `resources/list`.
const MAX_LIST_PAGES: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// stdio: the program to launch.
    #[serde(default)]
    pub command: String,
    /// stdio: its arguments.
    #[serde(default)]
    pub args: Vec<String>,
    pub builtin_loopback: bool,
    #[serde(default)]
    pub transport: McpTransportKind,
    /// HTTP: the server's single MCP endpoint (for example `https://example.com/mcp`).
    #[serde(default)]
    pub url: String,
    /// HTTP: extra headers sent with every request (for example `Authorization`).
    /// `Debug` output redacts their values.
    #[serde(default)]
    pub headers: Vec<McpHttpHeader>,
}

impl McpServerConfig {
    pub fn stdio(name: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args,
            builtin_loopback: false,
            transport: McpTransportKind::Stdio,
            url: String::new(),
            headers: Vec::new(),
        }
    }

    pub fn http(name: impl Into<String>, url: impl Into<String>, headers: Vec<McpHttpHeader>) -> Self {
        Self {
            name: name.into(),
            command: String::new(),
            args: Vec::new(),
            builtin_loopback: false,
            transport: McpTransportKind::Http,
            url: url.into(),
            headers,
        }
    }

    /// One line describing where the server lives. Header names are listed, values never.
    pub fn target_display(&self) -> String {
        if self.builtin_loopback {
            return self.command.clone();
        }
        match self.transport {
            McpTransportKind::Stdio => {
                let mut s = self.command.clone();
                for a in &self.args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            McpTransportKind::Http if self.headers.is_empty() => self.url.clone(),
            McpTransportKind::Http => {
                let names: Vec<&str> = self.headers.iter().map(|h| h.name.as_str()).collect();
                format!("{} (headers: {})", self.url, names.join(", "))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerStatus {
    pub config: McpServerConfig,
    pub connected: bool,
    pub server_version: String,
    pub tools: Vec<McpTool>,
    pub resources: Vec<McpResource>,
    pub last_error: Option<String>,
}

impl McpServerStatus {
    /// A status for a server MRT could not reach (or whose settings were invalid).
    pub fn failed(config: McpServerConfig, error: impl Into<String>) -> Self {
        Self {
            config,
            connected: false,
            server_version: "unreachable".to_string(),
            tools: Vec::new(),
            resources: Vec::new(),
            last_error: Some(error.into()),
        }
    }
}

/// An initialized MCP client session over any transport. Holds the request id counter and the
/// negotiated protocol version; the transport does the I/O.
pub(crate) struct McpClientSession {
    transport: Transport,
    next_id: u64,
    server_version: String,
}

impl McpClientSession {
    /// Opens the transport and runs the `initialize` / `notifications/initialized` handshake.
    async fn open(config: &McpServerConfig) -> Result<Self> {
        let transport = match config.transport {
            McpTransportKind::Stdio => Transport::Stdio(StdioTransport::spawn(&config.command, &config.args)?),
            McpTransportKind::Http => Transport::Http(HttpTransport::new(&config.url, config.headers.clone())?),
        };
        let mut session = Self {
            transport,
            next_id: 0,
            server_version: String::new(),
        };
        session.initialize().await?;
        Ok(session)
    }

    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "ModernRustTerminal", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        self.server_version = result
            .pointer("/serverInfo/version")
            .and_then(|v| v.as_str())
            .unwrap_or("1.0")
            .to_string();
        let protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(LATEST_PROTOCOL_VERSION)
            .to_string();
        self.transport.set_protocol_version(&protocol_version);
        self.transport
            .notify(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await?;
        self.transport.on_initialized();
        Ok(())
    }

    /// Sends one request and returns its `result`, turning a JSON-RPC error into `Err`.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params });
        let resp = tokio::time::timeout(REQUEST_TIMEOUT, self.transport.request(&msg))
            .await
            .map_err(|_| anyhow!("MCP request `{method}` timed out after {}s", REQUEST_TIMEOUT.as_secs()))??;
        if let Some(err) = resp.error {
            anyhow::bail!("MCP Error {}: {}", err.code, err.message);
        }
        Ok(resp.result.unwrap_or_else(|| json!({})))
    }

    /// Runs a paginated `*/list` request and collects `key` from every page.
    async fn list_all<T: serde::de::DeserializeOwned>(&mut self, method: &str, key: &str) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = self.request(method, params).await?;
            if let Some(list) = page.get(key) {
                items.extend(
                    serde_json::from_value::<Vec<T>>(list.clone())
                        .with_context(|| format!("Malformed `{key}` in {method} reply"))?,
                );
            }
            cursor = page.get("nextCursor").and_then(|c| c.as_str()).map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(items)
    }

    async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<McpToolCallResult> {
        let result = self
            .request("tools/call", json!({ "name": tool_name, "arguments": arguments }))
            .await?;
        Ok(serde_json::from_value(result)?)
    }

    fn events(&self) -> ServerEventLog {
        self.transport.events()
    }

    async fn close(&mut self) {
        self.transport.close().await;
    }
}

type SharedSession = Arc<tokio::sync::Mutex<McpClientSession>>;

/// Manages MCP servers connected to Modern Rust Terminal (MRT as MCP Host).
///
/// Cloning is cheap and clones share the live sessions with external servers, so a clone moved
/// into a background task reuses the same `stdio` child process or HTTP `Mcp-Session-Id`.
#[derive(Clone)]
pub struct McpHostManager {
    pub local_server_state: MrtMcpServerState,
    pub servers: Vec<McpServerStatus>,
    sessions: Arc<tokio::sync::Mutex<HashMap<String, SharedSession>>>,
}

impl McpHostManager {
    pub fn new(local_server_state: MrtMcpServerState) -> Self {
        let builtin_tools = MrtMcpServerState::built_in_tools();
        let builtin_resources = MrtMcpServerState::built_in_resources();

        let builtin_status = McpServerStatus {
            config: McpServerConfig {
                builtin_loopback: true,
                ..McpServerConfig::stdio(
                    "mrt-native-mcp",
                    "mrt --mcp-server (in-process)",
                    vec!["--mcp-server".to_string()],
                )
            },
            connected: true,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            tools: builtin_tools,
            resources: builtin_resources,
            last_error: None,
        };

        Self {
            local_server_state,
            servers: vec![builtin_status],
            sessions: Arc::default(),
        }
    }

    pub fn all_tools(&self) -> Vec<(String, McpTool)> {
        let mut list = Vec::new();
        for srv in &self.servers {
            if srv.connected {
                for tool in &srv.tools {
                    list.push((srv.config.name.clone(), tool.clone()));
                }
            }
        }
        list
    }

    /// Invokes a tool on the built-in MRT MCP server or on an external stdio / HTTP MCP server.
    /// External calls reuse the server's live session; if the session ended (stdio child exited,
    /// HTTP 404 for the session id) MRT reconnects once and retries.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<McpToolCallResult> {
        let srv = self
            .servers
            .iter()
            .find(|s| s.config.name == server_name)
            .or_else(|| self.servers.first())
            .context("No MCP server available")?;

        if srv.config.builtin_loopback {
            let req = JsonRpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(1)),
                method: "tools/call".to_string(),
                params: Some(json!({
                    "name": tool_name,
                    "arguments": arguments
                })),
            };
            let resp = handle_mcp_jsonrpc(&self.local_server_state, req)
                .await
                .context("No response from local MCP server")?;
            if let Some(err) = resp.error {
                anyhow::bail!("MCP Error {}: {}", err.code, err.message);
            }
            let result_val = resp.result.unwrap_or_else(|| json!({}));
            let parsed: McpToolCallResult = serde_json::from_value(result_val)?;
            return Ok(parsed);
        }

        let session = self.session_for(&srv.config).await?;
        let first = session.lock().await.call_tool(tool_name, arguments.clone()).await;
        match first {
            Err(e) if e.downcast_ref::<SessionExpired>().is_some() => {
                self.drop_session(&srv.config.name).await;
                let session = self.session_for(&srv.config).await?;
                let retry = session.lock().await.call_tool(tool_name, arguments).await;
                retry
            }
            other => other,
        }
    }

    /// Notifications and requests the server sent outside a response (for HTTP servers this
    /// includes the optional GET event stream), oldest first. Draining empties the buffer.
    pub async fn drain_server_events(&self, server_name: &str) -> Vec<Value> {
        let session = self.sessions.lock().await.get(server_name).cloned();
        match session {
            Some(s) => s.lock().await.events().drain(),
            None => Vec::new(),
        }
    }

    /// Connects to an external stdio MCP server, runs `initialize` + `tools/list`, and registers its tools.
    pub async fn connect_external_stdio_server(
        &mut self,
        name: String,
        command: String,
        args: Vec<String>,
    ) -> Result<McpServerStatus> {
        self.connect_external_server(McpServerConfig::stdio(name, command, args))
            .await
    }

    /// Connects to a remote MCP server over Streamable HTTP. `headers` (for example
    /// `Authorization: Bearer ...`) are sent with every request and never logged.
    pub async fn connect_external_http_server(
        &mut self,
        name: String,
        url: String,
        headers: Vec<McpHttpHeader>,
    ) -> Result<McpServerStatus> {
        self.connect_external_server(McpServerConfig::http(name, url, headers))
            .await
    }

    /// Connects to an external server over the transport in `config`, replacing any earlier
    /// server with the same name. Connection failures are reported in the returned status.
    pub async fn connect_external_server(&mut self, config: McpServerConfig) -> Result<McpServerStatus> {
        self.drop_session(&config.name).await;

        let status = match self.open_session(&config).await {
            Ok((session, tools, resources)) => {
                let status = McpServerStatus {
                    config: config.clone(),
                    connected: true,
                    server_version: session.server_version.clone(),
                    tools,
                    resources,
                    last_error: None,
                };
                self.sessions
                    .lock()
                    .await
                    .insert(config.name.clone(), Arc::new(tokio::sync::Mutex::new(session)));
                status
            }
            Err(e) => McpServerStatus::failed(config, format!("{e:#}")),
        };

        self.servers.retain(|s| s.config.name != status.config.name);
        self.servers.push(status.clone());
        Ok(status)
    }

    async fn open_session(
        &self,
        config: &McpServerConfig,
    ) -> Result<(McpClientSession, Vec<McpTool>, Vec<McpResource>)> {
        let mut session = McpClientSession::open(config).await?;
        let listed = session.list_all::<McpTool>("tools/list", "tools").await;
        let tools = match listed {
            Ok(t) => t,
            Err(e) => {
                session.close().await;
                return Err(e);
            }
        };
        // Resources are optional; servers without them answer "method not found".
        let resources = session
            .list_all::<McpResource>("resources/list", "resources")
            .await
            .unwrap_or_default();
        Ok((session, tools, resources))
    }

    /// The live session for `config`, opening (and initializing) one if there is none.
    async fn session_for(&self, config: &McpServerConfig) -> Result<SharedSession> {
        let mut sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get(&config.name) {
            return Ok(s.clone());
        }
        let session = Arc::new(tokio::sync::Mutex::new(McpClientSession::open(config).await?));
        sessions.insert(config.name.clone(), session.clone());
        Ok(session)
    }

    async fn drop_session(&self, name: &str) {
        let old = self.sessions.lock().await.remove(name);
        if let Some(old) = old {
            old.lock().await.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_server_initialize_and_tools() {
        let state = MrtMcpServerState::default();

        let init_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = handle_mcp_jsonrpc(&state, init_req).await.unwrap();
        assert!(resp.error.is_none());

        let list_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "tools/list".into(),
            params: None,
        };
        let list_resp = handle_mcp_jsonrpc(&state, list_req).await.unwrap();
        let tools = list_resp
            .result
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap()
            .len();
        assert!(tools >= 20);
    }

    #[tokio::test]
    async fn test_mcp_safety_gate_blocks_critical_command() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);
        let res = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_run_command",
                json!({ "command": "rm -rf /important_data" }),
            )
            .await
            .unwrap();
        assert!(res.is_error);
        assert!(res.content[0].text.contains("BLOCKED BY MRT SAFETY GATE"));
    }

    #[tokio::test]
    async fn test_mcp_tab_management_and_block_inspection() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);

        // 1. Create a second tab
        let created = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_create_tab",
                json!({ "title": "Agent Build Tab", "activate": true }),
            )
            .await
            .unwrap();
        assert!(!created.is_error);
        let created_json: Value = serde_json::from_str(&created.content[0].text).unwrap();
        let new_tab_id = created_json.get("tab_id").unwrap().as_str().unwrap();

        // 2. Run two commands in the new tab so we can compare them
        let run1 = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_run_command",
                json!({ "command": "echo alpha_line", "tab_id": new_tab_id }),
            )
            .await
            .unwrap();
        assert!(!run1.is_error);
        let run1_json: Value = serde_json::from_str(&run1.content[0].text).unwrap();
        let blk1_id = run1_json.get("block_id").unwrap().as_str().unwrap();

        let run2 = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_run_command",
                json!({ "command": "echo beta_line", "tab_id": new_tab_id }),
            )
            .await
            .unwrap();
        assert!(!run2.is_error);
        let run2_json: Value = serde_json::from_str(&run2.content[0].text).unwrap();
        let blk2_id = run2_json.get("block_id").unwrap().as_str().unwrap();

        // 3. Get block by ID
        let blk_detail = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_get_block",
                json!({ "block_id": blk1_id }),
            )
            .await
            .unwrap();
        assert!(!blk_detail.is_error);
        assert!(blk_detail.content[0].text.contains("alpha_line"));

        // 4. Search blocks
        let search = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_search_blocks",
                json!({ "query": "beta_line", "tab_id": new_tab_id }),
            )
            .await
            .unwrap();
        assert!(!search.is_error);
        let search_arr: Vec<Value> = serde_json::from_str(&search.content[0].text).unwrap();
        assert_eq!(search_arr.len(), 1);

        // 5. Compare blocks
        let cmp = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_compare_blocks",
                json!({ "previous_block_id": blk1_id, "current_block_id": blk2_id }),
            )
            .await
            .unwrap();
        assert!(!cmp.is_error);
        let cmp_json: Value = serde_json::from_str(&cmp.content[0].text).unwrap();
        assert_eq!(cmp_json.get("identical").and_then(|v| v.as_bool()), Some(false));
        let diff_str = cmp_json.get("diff").and_then(|v| v.as_str()).unwrap_or("");
        assert!(diff_str.contains("-alpha_line"));
        assert!(diff_str.contains("+beta_line"));

        // 6. Diagnose block
        let diag = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_diagnose_block",
                json!({ "block_id": blk1_id }),
            )
            .await
            .unwrap();
        assert!(!diag.is_error);
        assert!(diag.content[0].text.contains("explanation"));

        // 7. Close tab
        let close = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_close_tab",
                json!({ "tab_id": new_tab_id }),
            )
            .await
            .unwrap();
        assert!(!close.is_error);
    }

    #[tokio::test]
    async fn test_mcp_interactive_pty_lifecycle() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);

        // Spawn an interactive PTY session with a shell every host has
        let shell = if cfg!(target_os = "windows") { "cmd" } else { "sh" };
        let spawned = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_pty_spawn",
                json!({ "shell": shell, "cols": 100, "rows": 30, "wait_ms": 150 }),
            )
            .await
            .unwrap();
        assert!(!spawned.is_error, "{}", spawned.content[0].text);
        let spawned_json: Value = serde_json::from_str(&spawned.content[0].text).unwrap();
        let pty_id = spawned_json.get("pty_id").unwrap().as_str().unwrap();

        // Write a command to the PTY
        let written = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_pty_write",
                json!({
                    "pty_id": pty_id,
                    "input": "echo MRT_PTY_STREAM_OK",
                    "append_newline": true,
                    "wait_ms": 300
                }),
            )
            .await
            .unwrap();
        assert!(!written.is_error);

        // Poll up to a few times in case the OS shell takes a moment to flush
        let mut transcript = String::new();
        for _ in 0..10 {
            let read_res = host
                .call_tool(
                    "mrt-native-mcp",
                    "mrt_pty_read",
                    json!({ "pty_id": pty_id, "drain": false, "wait_ms": 100 }),
                )
                .await
                .unwrap();
            assert!(!read_res.is_error);
            transcript = read_res.content[0].text.clone();
            if transcript.contains("MRT_PTY_STREAM_OK") {
                break;
            }
        }
        assert!(
            transcript.contains("MRT_PTY_STREAM_OK"),
            "Expected PTY transcript to contain MRT_PTY_STREAM_OK, got: {transcript}"
        );

        // Resize PTY
        let resized = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_pty_resize",
                json!({ "pty_id": pty_id, "cols": 132, "rows": 40 }),
            )
            .await
            .unwrap();
        assert!(!resized.is_error);

        // Kill PTY
        let killed = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_pty_kill",
                json!({ "pty_id": pty_id, "remove": true }),
            )
            .await
            .unwrap();
        assert!(!killed.is_error);
    }

    #[tokio::test]
    async fn test_live_gui_bridge_tcp_forwarding() {
        let state = MrtMcpServerState::default();
        let endpoint = crate::bridge::start_gui_bridge_listener(state, 0, false)
            .await
            .unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(99)),
            method: "tools/call".into(),
            params: Some(json!({
                "name": "mrt_create_tab",
                "arguments": { "title": "Bridged TCP Tab" }
            })),
        };
        let resp = crate::bridge::try_forward_jsonrpc_to_gui(endpoint.addr.port(), &endpoint.token, &req)
            .await
            .unwrap()
            .unwrap();
        assert!(resp.error.is_none());
        let result_str = serde_json::to_string(&resp.result).unwrap();
        assert!(result_str.contains("Bridged TCP Tab"));
    }

    #[tokio::test]
    async fn test_mcp_inspect_dns_tool() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);
        let res = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_inspect_dns",
                json!({ "host": "127.0.0.1:9" }),
            )
            .await
            .unwrap();
        assert!(!res.is_error);
        assert!(res.content[0].text.contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn test_mcp_generate_bug_report_tool() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);

        // Run a failing command
        let _ = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_run_command",
                json!({ "command": "exit 42" }),
            )
            .await
            .unwrap();

        let res = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_generate_bug_report",
                json!({}),
            )
            .await
            .unwrap();
        assert!(!res.is_error);
        let text = &res.content[0].text;
        assert!(text.contains("## Bug Report:"));
        assert!(text.contains("exit 42") || text.contains("exit"));
    }

    #[tokio::test]
    async fn test_mcp_inspect_ws_tool_validation() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);

        let res = host
            .call_tool(
                "mrt-native-mcp",
                "mrt_inspect_ws",
                json!({}),
            )
            .await
            .unwrap();
        assert!(res.is_error);
    }

    #[tokio::test]
    async fn test_mcp_process_tools() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state);
        let me = std::process::id();

        let list = host
            .call_tool("mrt-native-mcp", "mrt_list_processes", json!({ "filter": me.to_string(), "limit": 5 }))
            .await
            .unwrap();
        assert!(!list.is_error, "{}", list.content[0].text);
        let parsed: Value = serde_json::from_str(&list.content[0].text).unwrap();
        let processes = parsed["processes"].as_array().unwrap();
        assert!(processes.iter().any(|p| p["pid"] == json!(me)));
        assert!(processes.iter().any(|p| p["mrt_owner"] == json!("MRT (this app)")));

        let details = host
            .call_tool("mrt-native-mcp", "mrt_inspect_process", json!({ "pid": me }))
            .await
            .unwrap();
        assert!(!details.is_error, "{}", details.content[0].text);
        assert!(details.content[0].text.contains("\"ports\""));

        // Stopping MRT itself is Critical and always blocked.
        let kill_self = host
            .call_tool("mrt-native-mcp", "mrt_kill_process", json!({ "pid": me, "force": true }))
            .await
            .unwrap();
        assert!(kill_self.is_error);
        assert!(kill_self.content[0].text.contains("BLOCKED BY MRT SAFETY GATE"));

        let missing = host
            .call_tool("mrt-native-mcp", "mrt_kill_process", json!({}))
            .await
            .unwrap();
        assert!(missing.is_error);

        let baseline = host
            .call_tool("mrt-native-mcp", "mrt_compare_process_snapshots", json!({}))
            .await
            .unwrap();
        assert!(baseline.content[0].text.contains("Saved a baseline"));
        let delta = host
            .call_tool("mrt-native-mcp", "mrt_compare_process_snapshots", json!({}))
            .await
            .unwrap();
        assert!(delta.content[0].text.contains("Process Snapshot Delta"));
    }

    #[tokio::test]
    async fn test_mcp_kill_tool_stops_owned_pty_but_blocks_foreign_processes_headless() {
        let state = MrtMcpServerState::default();
        let host = McpHostManager::new(state.clone());
        let Some(shell) = state.shells.first().cloned() else {
            return;
        };
        let cwd = std::env::current_dir().unwrap();
        let pty = mrt_core::InteractivePty::spawn(&shell, &cwd, 80, 24).unwrap();
        let Some(pid) = pty.process_id() else {
            return;
        };
        state.owned_processes.lock().unwrap().insert(
            pid,
            mrt_tools::OwnedProcess {
                pid,
                label: "test pty".into(),
                tab_id: None,
                kind: mrt_tools::OwnedProcessKind::Pty,
            },
        );
        let res = host
            .call_tool("mrt-native-mcp", "mrt_kill_process", json!({ "pid": pid, "force": true, "tree": true }))
            .await
            .unwrap();
        assert!(!res.is_error, "{}", res.content[0].text);
        assert!(res.content[0].text.contains("Stopped") || res.content[0].text.contains("NotFound"));
        let _ = pty.kill();
    }
}
