use crate::protocol::{JsonRpcRequest, JsonRpcResponse, McpResource, McpTool, McpToolCallResult};
use crate::server::{handle_mcp_jsonrpc, MrtMcpServerState};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub builtin_loopback: bool,
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

/// Manages MCP servers connected to Modern Rust Terminal (MRT as MCP Host).
#[derive(Clone)]
pub struct McpHostManager {
    pub local_server_state: MrtMcpServerState,
    pub servers: Vec<McpServerStatus>,
}

impl McpHostManager {
    pub fn new(local_server_state: MrtMcpServerState) -> Self {
        let builtin_tools = MrtMcpServerState::built_in_tools();
        let builtin_resources = MrtMcpServerState::built_in_resources();

        let builtin_status = McpServerStatus {
            config: McpServerConfig {
                name: "mrt-native-mcp".to_string(),
                command: "mrt --mcp-server (in-process)".to_string(),
                args: vec!["--mcp-server".to_string()],
                builtin_loopback: true,
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

    /// Invokes a tool on either the built-in MRT MCP server or an external stdio MCP server.
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

        call_external_stdio_tool(&srv.config, tool_name, arguments).await
    }

    /// Connects to an external stdio MCP server, runs `initialize` + `tools/list`, and registers its tools.
    pub async fn connect_external_stdio_server(
        &mut self,
        name: String,
        command: String,
        args: Vec<String>,
    ) -> Result<McpServerStatus> {
        let config = McpServerConfig {
            name,
            command,
            args,
            builtin_loopback: false,
        };

        let status = match probe_external_stdio_server(&config).await {
            Ok((version, tools, resources)) => McpServerStatus {
                config,
                connected: true,
                server_version: version,
                tools,
                resources,
                last_error: None,
            },
            Err(e) => McpServerStatus {
                config,
                connected: false,
                server_version: "unreachable".to_string(),
                tools: Vec::new(),
                resources: Vec::new(),
                last_error: Some(e.to_string()),
            },
        };

        self.servers.retain(|s| s.config.name != status.config.name);
        self.servers.push(status.clone());
        Ok(status)
    }
}

async fn probe_external_stdio_server(
    config: &McpServerConfig,
) -> Result<(String, Vec<McpTool>, Vec<McpResource>)> {
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to launch MCP server '{}'", config.command))?;

    let mut stdin = child.stdin.take().context("Failed to open MCP stdin")?;
    let stdout = child.stdout.take().context("Failed to open MCP stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    // 1. Send initialize
    let init_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "initialize".into(),
        params: Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "ModernRustTerminal", "version": env!("CARGO_PKG_VERSION") }
        })),
    };
    let mut line = serde_json::to_string(&init_req)? + "\n";
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;

    let init_resp_str = reader
        .next_line()
        .await?
        .context("MCP server closed stream during initialize")?;
    let init_resp: JsonRpcResponse = serde_json::from_str(&init_resp_str)?;
    let version = init_resp
        .result
        .as_ref()
        .and_then(|r| r.get("serverInfo"))
        .and_then(|s| s.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("1.0")
        .to_string();

    // 2. Send initialized notification & tools/list
    let notif = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    line = serde_json::to_string(&notif)? + "\n";
    stdin.write_all(line.as_bytes()).await?;

    let tools_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "tools/list".into(),
        params: Some(json!({})),
    };
    line = serde_json::to_string(&tools_req)? + "\n";
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;

    let tools_resp_str = reader
        .next_line()
        .await?
        .context("MCP server closed stream during tools/list")?;
    let tools_resp: JsonRpcResponse = serde_json::from_str(&tools_resp_str)?;
    let tools: Vec<McpTool> = tools_resp
        .result
        .and_then(|r| r.get("tools").cloned())
        .and_then(|t| serde_json::from_value(t).ok())
        .unwrap_or_default();

    let _ = child.kill().await;
    Ok((version, tools, Vec::new()))
}

async fn call_external_stdio_tool(
    config: &McpServerConfig,
    tool_name: &str,
    arguments: Value,
) -> Result<McpToolCallResult> {
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;

    let mut stdin = child.stdin.take().context("Failed to open MCP stdin")?;
    let stdout = child.stdout.take().context("Failed to open MCP stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "ModernRustTerminal", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    stdin
        .write_all(format!("{}\n", serde_json::to_string(&init_req)?).as_bytes())
        .await?;
    let _ = reader.next_line().await?;

    let call_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    });
    stdin
        .write_all(format!("{}\n", serde_json::to_string(&call_req)?).as_bytes())
        .await?;
    stdin.flush().await?;

    let resp_line = reader
        .next_line()
        .await?
        .context("MCP server closed stream before returning tool result")?;
    let resp: JsonRpcResponse = serde_json::from_str(&resp_line)?;
    let _ = child.kill().await;

    if let Some(err) = resp.error {
        anyhow::bail!("MCP tool error ({}): {}", err.code, err.message);
    }
    let val = resp.result.unwrap_or_else(|| json!({}));
    Ok(serde_json::from_value(val)?)
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
