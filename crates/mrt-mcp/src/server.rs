use crate::protocol::{
    JsonRpcRequest, JsonRpcResponse, McpResource, McpTool, McpToolCallResult,
};
use anyhow::Result;
use mrt_core::{
    assess_command, discover_shells, execute_command_in_shell, CommandBlock, RiskLevel,
    ShellProfile,
};
use mrt_tools::{
    execute_http_request, fetch_url_as_markdown, inspect_listening_ports, HttpRequestSpec,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Clone)]
pub struct MrtMcpServerState {
    pub cwd: Arc<Mutex<PathBuf>>,
    pub blocks: Arc<Mutex<Vec<CommandBlock>>>,
    pub shells: Vec<ShellProfile>,
}

impl Default for MrtMcpServerState {
    fn default() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            cwd: Arc::new(Mutex::new(cwd)),
            blocks: Arc::new(Mutex::new(Vec::new())),
            shells: discover_shells(),
        }
    }
}

impl MrtMcpServerState {
    pub fn built_in_tools() -> Vec<McpTool> {
        vec![
            McpTool {
                name: "mrt_get_session_state".into(),
                description: "Get the current Modern Rust Terminal session state (OS, CWD, available shells, and block count).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                }),
            },
            McpTool {
                name: "mrt_list_blocks".into(),
                description: "List recent terminal command blocks, including command, shell, exit code, duration, and output.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of recent blocks to return (default 10)"
                        }
                    }
                }),
            },
            McpTool {
                name: "mrt_run_command".into(),
                description: "Execute a command in a specified shell (pwsh, powershell, cmd, bash, wsl, zsh, sh) with MRT Safety Gate protection.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The command string to run"
                        },
                        "shell": {
                            "type": "string",
                            "description": "Optional shell id (e.g. 'pwsh', 'cmd', 'bash', 'wsl')"
                        }
                    },
                    "required": ["command"]
                }),
            },
            McpTool {
                name: "mrt_http_request".into(),
                description: "Perform an HTTP request using MRT's built-in HTTP Inspector and return status, latency, headers, and formatted body.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "method": { "type": "string", "description": "HTTP method (GET, POST, PUT, DELETE)" },
                        "url": { "type": "string", "description": "Target URL" },
                        "body": { "type": "string", "description": "Optional request body" }
                    },
                    "required": ["url"]
                }),
            },
            McpTool {
                name: "mrt_web_fetch".into(),
                description: "Fetch a web URL and convert its HTML into clean readable Markdown.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "Web page URL to fetch" }
                    },
                    "required": ["url"]
                }),
            },
            McpTool {
                name: "mrt_inspect_ports".into(),
                description: "List active local listening TCP/UDP ports and PIDs on the host system.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        ]
    }

    pub fn built_in_resources() -> Vec<McpResource> {
        vec![
            McpResource {
                uri: "mrt://session/state".into(),
                name: "Active Terminal Session State".into(),
                description: "Live JSON snapshot of CWD, available shells, and environment.".into(),
                mime_type: "application/json".into(),
            },
            McpResource {
                uri: "mrt://session/blocks".into(),
                name: "Command Block History".into(),
                description: "Structured list of executed terminal blocks in this session.".into(),
                mime_type: "application/json".into(),
            },
        ]
    }
}

/// Processes a single JSON-RPC 2.0 MCP request against the MRT Server state.
pub async fn handle_mcp_jsonrpc(
    state: &MrtMcpServerState,
    req: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "notifications/initialized" | "notifications/cancelled" => None,
        "initialize" => Some(JsonRpcResponse::success(
            req.id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "subscribe": false, "listChanged": false }
                },
                "serverInfo": {
                    "name": "mrt-mcp-server",
                    "version": "0.1.0"
                }
            }),
        )),
        "tools/list" => Some(JsonRpcResponse::success(
            req.id,
            json!({
                "tools": MrtMcpServerState::built_in_tools()
            }),
        )),
        "resources/list" => Some(JsonRpcResponse::success(
            req.id,
            json!({
                "resources": MrtMcpServerState::built_in_resources()
            }),
        )),
        "resources/read" => {
            let uri = req
                .params
                .as_ref()
                .and_then(|p| p.get("uri"))
                .and_then(|u| u.as_str())
                .unwrap_or("mrt://session/state");

            let text = if uri == "mrt://session/blocks" {
                let guard = state.blocks.lock().unwrap();
                serde_json::to_string_pretty(&*guard).unwrap_or_else(|_| "[]".into())
            } else {
                let cwd = state.cwd.lock().unwrap().display().to_string();
                let shells: Vec<&str> = state.shells.iter().map(|s| s.id.as_str()).collect();
                serde_json::to_string_pretty(&json!({
                    "os": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                    "cwd": cwd,
                    "available_shells": shells,
                }))
                .unwrap_or_default()
            };

            Some(JsonRpcResponse::success(
                req.id,
                json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": text
                    }]
                }),
            ))
        }
        "tools/call" => {
            let params = req.params.unwrap_or_else(|| json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

            let call_result = execute_mrt_mcp_tool(state, tool_name, args).await;
            Some(JsonRpcResponse::success(
                req.id,
                serde_json::to_value(call_result).unwrap_or_else(|_| json!({})),
            ))
        }
        other => Some(JsonRpcResponse::error(
            req.id,
            -32601,
            format!("Method not found: {other}"),
        )),
    }
}

pub async fn execute_mrt_mcp_tool(
    state: &MrtMcpServerState,
    tool_name: &str,
    args: Value,
) -> McpToolCallResult {
    match tool_name {
        "mrt_get_session_state" => {
            let cwd = state.cwd.lock().unwrap().display().to_string();
            let block_count = state.blocks.lock().unwrap().len();
            let shells: Vec<Value> = state
                .shells
                .iter()
                .map(|s| json!({ "id": s.id, "name": s.name, "program": s.program }))
                .collect();
            let payload = json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "cwd": cwd,
                "block_count": block_count,
                "available_shells": shells
            });
            McpToolCallResult::text(
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
                false,
            )
        }
        "mrt_list_blocks" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let guard = state.blocks.lock().unwrap();
            let recent: Vec<Value> = guard
                .iter()
                .rev()
                .take(limit)
                .map(|b| {
                    json!({
                        "id": b.id,
                        "command": b.command,
                        "shell": b.shell_name,
                        "cwd": b.cwd,
                        "exit_code": b.exit_code,
                        "duration_ms": b.duration_ms,
                        "structured_type": b.structured.badge_label(),
                        "output": b.plain_output,
                    })
                })
                .collect();
            McpToolCallResult::text(
                serde_json::to_string_pretty(&recent).unwrap_or_default(),
                false,
            )
        }
        "mrt_run_command" => {
            let Some(cmd) = args.get("command").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'command' parameter", true);
            };
            let safety = assess_command(cmd);
            if safety.risk == RiskLevel::Critical {
                return McpToolCallResult::text(
                    format!(
                        "BLOCKED BY MRT SAFETY GATE: Command '{}' flagged as {} ({}). Requires interactive approval inside MRT GUI.",
                        cmd,
                        safety.risk.label(),
                        safety.reason
                    ),
                    true,
                );
            }

            let requested_shell = args.get("shell").and_then(|v| v.as_str());
            let shell = requested_shell
                .and_then(|id| state.shells.iter().find(|s| s.id.eq_ignore_ascii_case(id)))
                .or_else(|| state.shells.first())
                .cloned();

            let Some(shell_profile) = shell else {
                return McpToolCallResult::text("No shell available on host", true);
            };

            let cwd = state.cwd.lock().unwrap().clone();
            match execute_command_in_shell(&shell_profile, cmd, &cwd).await {
                Ok(res) => {
                    if let Some(new_cwd) = &res.updated_cwd {
                        *state.cwd.lock().unwrap() = new_cwd.clone();
                    }
                    let block = CommandBlock::new_completed(
                        cmd,
                        &shell_profile.name,
                        cwd.display().to_string(),
                        &res.combined_output,
                        res.exit_code,
                        res.duration_ms,
                    );
                    state.blocks.lock().unwrap().push(block);

                    let summary = json!({
                        "shell": shell_profile.id,
                        "exit_code": res.exit_code,
                        "duration_ms": res.duration_ms,
                        "output": res.combined_output,
                    });
                    McpToolCallResult::text(
                        serde_json::to_string_pretty(&summary).unwrap_or_default(),
                        res.exit_code != 0,
                    )
                }
                Err(e) => McpToolCallResult::text(format!("Execution failed: {e}"), true),
            }
        }
        "mrt_http_request" => {
            let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'url' parameter", true);
            };
            let method = args
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("GET")
                .to_string();
            let body = args.get("body").and_then(|v| v.as_str()).map(String::from);

            let mut headers = BTreeMap::new();
            headers.insert("User-Agent".into(), "ModernRustTerminal-MCP/0.1".into());
            let spec = HttpRequestSpec {
                method,
                url: url.to_string(),
                headers,
                body,
            };
            match execute_http_request(&spec).await {
                Ok(summary) => McpToolCallResult::text(
                    serde_json::to_string_pretty(&summary).unwrap_or_default(),
                    summary.status_code >= 400,
                ),
                Err(e) => McpToolCallResult::text(format!("HTTP error: {e}"), true),
            }
        }
        "mrt_web_fetch" => {
            let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'url' parameter", true);
            };
            match fetch_url_as_markdown(url).await {
                Ok(md) => McpToolCallResult::text(md, false),
                Err(e) => McpToolCallResult::text(format!("Web fetch error: {e}"), true),
            }
        }
        "mrt_inspect_ports" => match inspect_listening_ports().await {
            Ok((_, csv)) => McpToolCallResult::text(csv, false),
            Err(e) => McpToolCallResult::text(format!("Port inspection error: {e}"), true),
        },
        other => McpToolCallResult::text(format!("Unknown MCP tool: {other}"), true),
    }
}

/// Runs MRT in headless stdio MCP Server mode (`mrt --mcp-server`), reading JSON-RPC lines
/// from stdin and writing JSON-RPC responses to stdout.
pub async fn run_stdio_mcp_server(state: MrtMcpServerState) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

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
            stdout.write_all(serialized.as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
