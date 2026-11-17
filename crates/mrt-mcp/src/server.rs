use crate::bridge::{read_bridge_endpoint, try_forward_jsonrpc_to_gui};
use crate::protocol::{
    JsonRpcRequest, JsonRpcResponse, McpResource, McpTool, McpToolCallResult,
};
use anyhow::Result;
use mrt_ai::{diagnose_command_error, translate_nl_to_command};
use mrt_core::{
    assess_command, ActivitySnapshot, discover_shells, execute_command_in_shell, parse_ansi_text, CommandBlock,
    InteractivePty, RiskLevel, ShellProfile,
};
use mrt_tools::{
    compare_blocks_structured, compare_process_snapshots, execute_http_request,
    fetch_url_as_markdown, generate_markdown_bug_report, inspect_dns, inspect_listening_ports,
    inspect_process, inspect_websocket, owner_label, plan_kill, query_processes, ChangeType,
    HttpRequestSpec, KillRequest, OwnedProcess, OwnedProcessKind, OwnedProcessMap,
    ProcessQuery, ProcessSelector, ProcessSnapshot, ProcessSort, SharedProcessSampler,
    StructuredDiffKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTabSession {
    pub id: Uuid,
    pub title: String,
    pub shell_id: String,
    pub shell_name: String,
    pub cwd: PathBuf,
    pub blocks: Vec<CommandBlock>,
    /// Live activity from the GUI's tracker. `None` for tabs owned by a headless server,
    /// whose state is derived from their blocks instead.
    #[serde(skip)]
    pub activity: Option<ActivitySnapshot>,
    /// Set for an agent sandbox tab (`:sandbox`): commands always start in this
    /// folder, and `mrt_run_command` runs only commands the Safety Gate rates Safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_root: Option<PathBuf>,
}

/// JSON for a tab's activity (`state`, `progress`, `tone`, `agent`, `detail`, `command`).
pub fn tab_activity_json(tab: &McpTabSession) -> Value {
    match &tab.activity {
        Some(snapshot) => snapshot.to_json(),
        None => ActivitySnapshot::from_blocks(&tab.blocks).to_json(),
    }
}

#[derive(Debug, Clone)]
pub enum GuiUiCommand {
    SyncFromMcp,
    SetInputDraft {
        tab_id: Option<Uuid>,
        text: String,
        submit: bool,
    },
    SetMosaic {
        enabled: bool,
        columns: Option<usize>,
        /// Optional arrangement: "grid", "columns", "rows" or "focus".
        layout: Option<String>,
    },
    SetExplorer {
        visible: bool,
    },
    SetComparison {
        tab_id: Option<Uuid>,
        previous_block_id: Uuid,
        current_block_id: Uuid,
    },
    /// Shows or hides the Toolbox side panel, optionally on a tab ("network", "task_manager").
    SetToolbox {
        visible: bool,
        tab: Option<String>,
    },
    /// Asks the GUI to stop processes. The GUI re-plans the request against a fresh
    /// snapshot and opens the Safety Approval Gate; nothing is stopped without approval.
    RequestProcessKill {
        pids: Vec<u32>,
        force: bool,
        tree: bool,
    },
}

#[derive(Clone)]
pub struct MrtMcpServerState {
    pub cwd: Arc<Mutex<PathBuf>>,
    pub blocks: Arc<Mutex<Vec<CommandBlock>>>,
    pub tabs: Arc<Mutex<Vec<McpTabSession>>>,
    pub active_tab_id: Arc<Mutex<Uuid>>,
    pub ptys: Arc<Mutex<HashMap<Uuid, Arc<InteractivePty>>>>,
    pub shells: Vec<ShellProfile>,
    pub gui_tx: Option<Sender<GuiUiCommand>>,
    /// Shared process sampler for the Task Manager tools.
    pub process_sampler: SharedProcessSampler,
    /// Processes started by MRT (GUI command blocks, TUI overlays, MCP PTYs), by PID.
    pub owned_processes: Arc<Mutex<OwnedProcessMap>>,
    /// Baseline for `mrt_compare_process_snapshots` when no block ids are given.
    pub process_baseline: Arc<Mutex<Option<ProcessSnapshot>>>,
}

impl Default for MrtMcpServerState {
    fn default() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let shells = discover_shells();
        let default_shell = shells.first();
        let shell_id = default_shell
            .map(|s| s.id.clone())
            .unwrap_or_else(|| "sh".into());
        let shell_name = default_shell
            .map(|s| s.kind.short_label().to_string())
            .unwrap_or_else(|| "sh".into());
        let tab_id = Uuid::new_v4();
        let first_tab = McpTabSession {
            id: tab_id,
            title: format!("Tab 1 ({shell_name})"),
            shell_id,
            shell_name,
            cwd: cwd.clone(),
            blocks: Vec::new(),
            activity: None,
            sandbox_root: None,
        };
        Self {
            cwd: Arc::new(Mutex::new(cwd)),
            blocks: Arc::new(Mutex::new(Vec::new())),
            tabs: Arc::new(Mutex::new(vec![first_tab])),
            active_tab_id: Arc::new(Mutex::new(tab_id)),
            ptys: Arc::new(Mutex::new(HashMap::new())),
            shells,
            gui_tx: None,
            process_sampler: SharedProcessSampler::new(),
            owned_processes: Arc::new(Mutex::new(HashMap::new())),
            process_baseline: Arc::new(Mutex::new(None)),
        }
    }
}

impl MrtMcpServerState {
    fn notify_gui(&self, cmd: GuiUiCommand) {
        if let Some(ref tx) = self.gui_tx {
            let _ = tx.send(cmd);
        }
    }

    fn resolve_tab_id(&self, requested: Option<&str>) -> Option<Uuid> {
        let tabs = self.tabs.lock().ok()?;
        if let Some(req) = requested {
            let trimmed = req.trim();
            if !trimmed.is_empty() {
                if let Ok(uuid) = Uuid::parse_str(trimmed) {
                    if tabs.iter().any(|t| t.id == uuid) {
                        return Some(uuid);
                    }
                }
                // Match by index ("0", "1") or case-insensitive title substring
                if let Ok(idx) = trimmed.parse::<usize>() {
                    if let Some(t) = tabs.get(idx) {
                        return Some(t.id);
                    }
                }
                let lower = trimmed.to_lowercase();
                if let Some(t) = tabs.iter().find(|t| t.title.to_lowercase().contains(&lower)) {
                    return Some(t.id);
                }
                return None;
            }
        }
        let active = *self.active_tab_id.lock().ok()?;
        if tabs.iter().any(|t| t.id == active) {
            Some(active)
        } else {
            tabs.first().map(|t| t.id)
        }
    }

    fn resolve_shell(&self, requested: Option<&str>) -> Option<ShellProfile> {
        if let Some(req) = requested {
            let trimmed = req.trim();
            if !trimmed.is_empty() {
                return self
                    .shells
                    .iter()
                    .find(|s| {
                        s.id.eq_ignore_ascii_case(trimmed)
                            || s.kind.short_label().eq_ignore_ascii_case(trimmed)
                            || s.name.to_lowercase().contains(&trimmed.to_lowercase())
                    })
                    .cloned();
            }
        }
        self.shells.first().cloned()
    }

    pub fn built_in_tools() -> Vec<McpTool> {
        vec![
            McpTool {
                name: "mrt_get_session_state".into(),
                description: "Get the current Modern Rust Terminal session state (OS, CWD, active tab, all open tabs with their activity state, active PTY sessions, available shells, and block count).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                }),
            },
            McpTool {
                name: "mrt_list_tabs".into(),
                description: "List all terminal tabs with their IDs, titles, active shells, CWDs, block counts, active status, and activity (idle, running, progress with percent, waiting_for_input, succeeded, failed; plus any detected agent CLI).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                }),
            },
            McpTool {
                name: "mrt_create_tab".into(),
                description: "Create a new terminal tab with an optional title, shell profile, Working Directory (cwd), and focus flag.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Optional custom title for the new tab" },
                        "shell": { "type": "string", "description": "Optional shell id (e.g. 'pwsh', 'powershell', 'cmd', 'bash', 'git-bash', 'wsl', 'zsh', 'sh')" },
                        "cwd": { "type": "string", "description": "Optional starting directory path" },
                        "activate": { "type": "boolean", "description": "Switch focus to the newly created tab (default true)" }
                    }
                }),
            },
            McpTool {
                name: "mrt_switch_tab".into(),
                description: "Switch the active terminal tab by tab UUID, 0-based index, or title substring.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string", "description": "Tab UUID, 0-based index, or title substring" }
                    },
                    "required": ["tab_id"]
                }),
            },
            McpTool {
                name: "mrt_close_tab".into(),
                description: "Close a terminal tab by UUID, 0-based index, or title substring (at least one tab is always kept open).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string", "description": "Tab UUID, 0-based index, or title substring" }
                    },
                    "required": ["tab_id"]
                }),
            },
            McpTool {
                name: "mrt_configure_tab".into(),
                description: "Configure a terminal tab's shell profile, working directory (cwd), or title.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string", "description": "Optional tab UUID, index, or title (defaults to active tab)" },
                        "shell": { "type": "string", "description": "Optional new shell id (e.g. 'pwsh', 'cmd', 'bash', 'wsl')" },
                        "cwd": { "type": "string", "description": "Optional new working directory path" },
                        "title": { "type": "string", "description": "Optional new tab title" }
                    }
                }),
            },
            McpTool {
                name: "mrt_run_command".into(),
                description: "Execute a command in a specified tab and shell (pwsh, powershell, cmd, bash, wsl, zsh, sh) with MRT Safety Gate protection, recording a structured CommandBlock.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The command string to run"
                        },
                        "shell": {
                            "type": "string",
                            "description": "Optional shell id override (e.g. 'pwsh', 'cmd', 'bash', 'wsl')"
                        },
                        "tab_id": {
                            "type": "string",
                            "description": "Optional target tab UUID, index, or title (defaults to active tab)"
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional working directory override for this command"
                        }
                    },
                    "required": ["command"]
                }),
            },
            McpTool {
                name: "mrt_list_blocks".into(),
                description: "List recent terminal command blocks (optionally scoped to a tab), including block ID, command, shell, exit code, duration, and output.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tab_id": {
                            "type": "string",
                            "description": "Optional tab UUID, index, or title to filter blocks"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of recent blocks to return (default 10)"
                        }
                    }
                }),
            },
            McpTool {
                name: "mrt_get_block".into(),
                description: "Retrieve full details of a specific CommandBlock by its UUID, including raw ANSI output, plain text, and structured payload (JSON tree, Table headers/rows, Markdown, or HTTP exchange).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "block_id": { "type": "string", "description": "UUID of the CommandBlock" }
                    },
                    "required": ["block_id"]
                }),
            },
            McpTool {
                name: "mrt_search_blocks".into(),
                description: "Search and filter terminal blocks across tabs by command/output substring, shell, or exit code status.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Substring to match in command or output" },
                        "tab_id": { "type": "string", "description": "Optional tab UUID, index, or title" },
                        "failed_only": { "type": "boolean", "description": "If true, only return blocks with non-zero exit codes" },
                        "limit": { "type": "integer", "description": "Maximum matches to return (default 20)" }
                    }
                }),
            },
            McpTool {
                name: "mrt_compare_blocks".into(),
                description: "Compute a unified line-by-line diff between two CommandBlocks (and optionally open the visual comparison view in the MRT GUI).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "previous_block_id": { "type": "string", "description": "UUID of the baseline/previous CommandBlock" },
                        "current_block_id": { "type": "string", "description": "UUID of the current CommandBlock" },
                        "show_in_gui": { "type": "boolean", "description": "Also activate the visual comparison view in the MRT GUI (default true)" }
                    },
                    "required": ["previous_block_id", "current_block_id"]
                }),
            },
            McpTool {
                name: "mrt_diagnose_block".into(),
                description: "Run MRT's shell-aware AI Error Triage on a CommandBlock (or arbitrary command/output) and attach the explanation and suggested fix to the block.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "block_id": { "type": "string", "description": "UUID of the failed CommandBlock to diagnose" }
                    },
                    "required": ["block_id"]
                }),
            },
            McpTool {
                name: "mrt_clear_blocks".into(),
                description: "Clear command blocks in the specified terminal tab (or all tabs if all=true).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string", "description": "Optional tab UUID, index, or title (defaults to active tab)" },
                        "all": { "type": "boolean", "description": "If true, clear blocks across all tabs" }
                    }
                }),
            },
            McpTool {
                name: "mrt_pty_spawn".into(),
                description: "Spawn a persistent interactive PTY session (for REPLs, dev servers, SSH, watchers, or interactive CLI prompts) and return its pty_id.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "shell": { "type": "string", "description": "Optional shell id (e.g. 'pwsh', 'cmd', 'bash', 'wsl')" },
                        "command": { "type": "string", "description": "Optional initial command to run inside the PTY" },
                        "cwd": { "type": "string", "description": "Optional working directory" },
                        "cols": { "type": "integer", "description": "Terminal width in columns (default 120)" },
                        "rows": { "type": "integer", "description": "Terminal height in rows (default 36)" },
                        "wait_ms": { "type": "integer", "description": "Milliseconds to wait for initial startup output before returning (default 150)" }
                    }
                }),
            },
            McpTool {
                name: "mrt_pty_write".into(),
                description: "Send text, commands, or control characters (e.g. '\\u0003' for Ctrl+C, '\\u0004' for Ctrl+D) to a running interactive PTY session.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pty_id": { "type": "string", "description": "UUID of the PTY session" },
                        "input": { "type": "string", "description": "Input string to write to the PTY stdin" },
                        "append_newline": { "type": "boolean", "description": "Whether to append a newline after input (default true)" },
                        "wait_ms": { "type": "integer", "description": "Optional milliseconds to wait before reading newly produced output (default 150)" }
                    },
                    "required": ["pty_id", "input"]
                }),
            },
            McpTool {
                name: "mrt_pty_read".into(),
                description: "Read from an interactive PTY session: its output (new unread output, or the full transcript), or with view='screen' what the terminal shows right now (screen rows, cursor, alternate screen, title), which is what you need to drive full-screen programs such as vim, htop or less.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pty_id": { "type": "string", "description": "UUID of the PTY session" },
                        "view": { "type": "string", "enum": ["output", "screen"], "description": "'output' (default): the program's output as text. 'screen': the visible terminal screen as rows of text plus cursor position, alternate-screen flag and title, rebuilt from the whole session. 'drain' and 'strip_ansi' apply to 'output' only." },
                        "drain": { "type": "boolean", "description": "If true (default), return and clear unread output; if false, return full transcript" },
                        "strip_ansi": { "type": "boolean", "description": "If true (default), strip ANSI escape codes for clean agent reading" },
                        "wait_ms": { "type": "integer", "description": "Optional milliseconds to wait before reading output (default 0)" }
                    },
                    "required": ["pty_id"]
                }),
            },
            McpTool {
                name: "mrt_pty_resize".into(),
                description: "Resize an active interactive PTY session's terminal dimensions (cols and rows).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pty_id": { "type": "string", "description": "UUID of the PTY session" },
                        "cols": { "type": "integer", "description": "New width in columns" },
                        "rows": { "type": "integer", "description": "New height in rows" }
                    },
                    "required": ["pty_id", "cols", "rows"]
                }),
            },
            McpTool {
                name: "mrt_pty_list".into(),
                description: "List all active and exited interactive PTY sessions managed by MRT.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            McpTool {
                name: "mrt_pty_kill".into(),
                description: "Terminate a running interactive PTY session and optionally remove it from the session table.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pty_id": { "type": "string", "description": "UUID of the PTY session to kill" },
                        "remove": { "type": "boolean", "description": "Remove the PTY session entry after killing (default true)" }
                    },
                    "required": ["pty_id"]
                }),
            },
            McpTool {
                name: "mrt_control_gui".into(),
                description: "Control the live MRT GUI layout and prompt bar (populate or submit the command input bar, toggle Mosaic multi-terminal grid view, or toggle the folder Explorer panel).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string", "description": "Optional target tab UUID, index, or title" },
                        "input_text": { "type": "string", "description": "Optional text to type into the tab's command prompt bar" },
                        "submit_input": { "type": "boolean", "description": "If true, immediately submit the input_text in the GUI (honoring the GUI Safety Gate modal for Critical commands)" },
                        "mosaic": { "type": "boolean", "description": "Optional toggle for Mosaic multi-terminal grid mode" },
                        "mosaic_columns": { "type": "integer", "description": "Optional column count (1..=6) for Mosaic grid mode" },
                        "mosaic_layout": { "type": "string", "enum": ["grid", "columns", "rows", "focus"], "description": "Optional Mosaic arrangement: even grid, side-by-side columns, stacked rows, or focus (active terminal large, others beside it)" },
                        "explorer": { "type": "boolean", "description": "Optional toggle for the left Folder Explorer side panel" },
                        "toolbox": { "type": ["boolean", "string"], "description": "Optional: true/false toggles the Toolbox side panel; \"network\" or \"task_manager\" opens it on that tab; \"hidden\" closes it" }
                    }
                }),
            },
            McpTool {
                name: "mrt_ai_translate".into(),
                description: "Translate a natural-language task description into an idiomatic shell command for the target shell using MRT's multi-provider AI / polyglot engine.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural language description of what to do" },
                        "shell": { "type": "string", "description": "Optional target shell id (defaults to active tab's shell)" }
                    },
                    "required": ["query"]
                }),
            },
            McpTool {
                name: "mrt_http_request".into(),
                description: "Perform an HTTP request using MRT's built-in HTTP Inspector and return status, latency, headers, and formatted body.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "method": { "type": "string", "description": "HTTP method (GET, POST, PUT, DELETE, PATCH)" },
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
            McpTool {
                name: "mrt_inspect_dns".into(),
                description: "Inspect DNS resolution for a hostname and measure TCP handshake latency to all resolved IPv4 and IPv6 addresses.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "host": { "type": "string", "description": "Hostname, domain, or host:port to resolve and trace (e.g. 'google.com', 'api.github.com:443')" }
                    },
                    "required": ["host"]
                }),
            },
            McpTool {
                name: "mrt_generate_bug_report".into(),
                description: "Generate a redacted GitHub-flavored Markdown bug report from a failed command block and its preceding history.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "block_id": { "type": "string", "description": "Optional UUID of the failed block (defaults to the most recent failed block)" },
                        "tab_id": { "type": "string", "description": "Optional tab UUID or index" }
                    }
                }),
            },
            McpTool {
                name: "mrt_inspect_ws".into(),
                description: "Connect to a WebSocket server (ws:// or wss://), optionally send an initial message, and collect logged frames and handshake latency.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "WebSocket endpoint URL (e.g. 'wss://echo.websocket.events')" },
                        "message": { "type": "string", "description": "Optional message string to send immediately after connecting" },
                        "timeout_ms": { "type": "integer", "description": "Milliseconds to listen for frames before closing (default 2500)" }
                    },
                    "required": ["url"]
                }),
            },
            McpTool {
                name: "mrt_list_processes".into(),
                description: "List running processes (PID, parent, name, CPU %, memory, user, status, command line) with the MRT tab/PTY that started each one, if any. Supports filtering, sorting, a process tree, and MRT-owned-only views.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "filter": { "type": "string", "description": "Optional case-insensitive match on PID, name or command line" },
                        "sort": { "type": "string", "enum": ["cpu", "mem", "pid", "name"], "description": "Sort order (default cpu)" },
                        "limit": { "type": "integer", "description": "Maximum rows (default 50)" },
                        "tree": { "type": "boolean", "description": "Arrange results as a parent/child tree (adds a depth field)" },
                        "only_mrt": { "type": "boolean", "description": "Only processes started by MRT (tabs, TUI overlays, PTYs) and their children" }
                    }
                }),
            },
            McpTool {
                name: "mrt_inspect_process".into(),
                description: "Inspect one process: command line, executable, working directory, user, CPU/memory, ancestors, child tree, listening ports, and whether MRT started it.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pid": { "type": "integer", "description": "Process id" }
                    },
                    "required": ["pid"]
                }),
            },
            McpTool {
                name: "mrt_kill_process".into(),
                description: "Request stopping processes through the MRT Safety Approval Gate. Critical targets (OS processes, MRT itself, other users' processes, names matching several PIDs) are always blocked. With the GUI attached, the request opens the approval modal and nothing stops until the user approves. Headless, only processes started by MRT can be stopped.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pid": { "type": "integer", "description": "Process id to stop" },
                        "name": { "type": "string", "description": "Process name to stop (alternative to pid)" },
                        "force": { "type": "boolean", "description": "Force kill (SIGKILL / TerminateProcess) instead of a graceful stop" },
                        "tree": { "type": "boolean", "description": "Also stop all child processes" }
                    }
                }),
            },
            McpTool {
                name: "mrt_compare_process_snapshots".into(),
                description: "Compare process snapshots: processes started, exited, or whose memory grew. Pass two ':ps' block ids, or omit them to compare the current processes with the baseline saved by the previous call (the first call only saves a baseline).".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "previous_block_id": { "type": "string", "description": "Optional UUID of an earlier :ps block" },
                        "current_block_id": { "type": "string", "description": "Optional UUID of a later :ps block" },
                        "reset_baseline": { "type": "boolean", "description": "Save a new baseline without comparing" }
                    }
                }),
            },
        ]
    }

    /// Owned-process map including live MCP PTYs.
    pub fn owned_process_map(&self) -> OwnedProcessMap {
        self.owned_processes.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn built_in_resources() -> Vec<McpResource> {
        vec![
            McpResource {
                uri: "mrt://session/state".into(),
                name: "Active Terminal Session State".into(),
                description: "Live JSON snapshot of CWD, active tab, available shells, and environment.".into(),
                mime_type: "application/json".into(),
            },
            McpResource {
                uri: "mrt://session/tabs".into(),
                name: "Open Terminal Tabs".into(),
                description: "List of open terminal tabs, active shell per tab, CWD, and block counts.".into(),
                mime_type: "application/json".into(),
            },
            McpResource {
                uri: "mrt://session/blocks".into(),
                name: "Command Block History".into(),
                description: "Structured list of executed terminal blocks in this session.".into(),
                mime_type: "application/json".into(),
            },
            McpResource {
                uri: "mrt://session/ptys".into(),
                name: "Interactive PTY Sessions".into(),
                description: "Active and exited interactive background PTY sessions.".into(),
                mime_type: "application/json".into(),
            },
            McpResource {
                uri: "mrt://processes".into(),
                name: "Running Processes".into(),
                description: "Top 100 running processes by CPU with memory, user, command line and MRT ownership.".into(),
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
        "ping" => Some(JsonRpcResponse::success(req.id, json!({}))),
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
                    "version": env!("CARGO_PKG_VERSION")
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

            let text = match uri {
                "mrt://session/blocks" => {
                    let guard = state.blocks.lock().unwrap();
                    serde_json::to_string_pretty(&*guard).unwrap_or_else(|_| "[]".into())
                }
                "mrt://session/tabs" => {
                    let active_id = *state.active_tab_id.lock().unwrap();
                    let tabs = state.tabs.lock().unwrap();
                    let summaries: Vec<Value> = tabs
                        .iter()
                        .enumerate()
                        .map(|(idx, t)| {
                            json!({
                                "index": idx,
                                "id": t.id,
                                "title": t.title,
                                "active": t.id == active_id,
                                "shell_id": t.shell_id,
                                "shell_name": t.shell_name,
                                "cwd": t.cwd.display().to_string(),
                                "block_count": t.blocks.len(),
                                "activity": tab_activity_json(t),
                            })
                        })
                        .collect();
                    serde_json::to_string_pretty(&summaries).unwrap_or_else(|_| "[]".into())
                }
                "mrt://session/ptys" => {
                    let guard = state.ptys.lock().unwrap();
                    let list: Vec<Value> = guard
                        .values()
                        .map(|pty| {
                            let exit_code = pty.try_wait_exit().ok().flatten();
                            json!({
                                "pty_id": pty.id,
                                "shell_id": pty.shell_id,
                                "shell_name": pty.shell_name,
                                "cwd": pty.cwd.display().to_string(),
                                "cols": pty.size().0,
                                "rows": pty.size().1,
                                "running": exit_code.is_none(),
                                "exit_code": exit_code,
                            })
                        })
                        .collect();
                    serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into())
                }
                "mrt://processes" => {
                    let query = ProcessQuery {
                        limit: Some(100),
                        ..Default::default()
                    };
                    match list_processes_value(state, &query).await {
                        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| "[]".into()),
                        Err(e) => serde_json::to_string(&json!({ "error": e.to_string() })).unwrap_or_default(),
                    }
                }
                _ => {
                    let cwd = state.cwd.lock().unwrap().display().to_string();
                    let active_tab_id = *state.active_tab_id.lock().unwrap();
                    let tab_count = state.tabs.lock().unwrap().len();
                    let pty_count = state.ptys.lock().unwrap().len();
                    let shells: Vec<&str> = state.shells.iter().map(|s| s.id.as_str()).collect();
                    serde_json::to_string_pretty(&json!({
                        "os": std::env::consts::OS,
                        "arch": std::env::consts::ARCH,
                        "cwd": cwd,
                        "active_tab_id": active_tab_id,
                        "tab_count": tab_count,
                        "pty_count": pty_count,
                        "gui_attached": state.gui_tx.is_some(),
                        "available_shells": shells,
                    }))
                    .unwrap_or_default()
                }
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
            let active_tab_id = *state.active_tab_id.lock().unwrap();
            let tabs: Vec<Value> = state
                .tabs
                .lock()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(idx, t)| {
                    json!({
                        "index": idx,
                        "id": t.id,
                        "title": t.title,
                        "active": t.id == active_tab_id,
                        "shell_id": t.shell_id,
                        "shell_name": t.shell_name,
                        "cwd": t.cwd.display().to_string(),
                        "block_count": t.blocks.len(),
                        "activity": tab_activity_json(t),
                        "sandbox": t.sandbox_root.as_ref().map(|r| r.display().to_string()),
                    })
                })
                .collect();
            let pty_count = state.ptys.lock().unwrap().len();
            let shells: Vec<Value> = state
                .shells
                .iter()
                .map(|s| json!({ "id": s.id, "name": s.name, "program": s.program }))
                .collect();
            let payload = json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "cwd": cwd,
                "active_tab_id": active_tab_id,
                "tab_count": tabs.len(),
                "tabs": tabs,
                "pty_count": pty_count,
                "block_count": block_count,
                "gui_attached": state.gui_tx.is_some(),
                "available_shells": shells
            });
            McpToolCallResult::text(
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
                false,
            )
        }
        "mrt_list_tabs" => {
            let active_tab_id = *state.active_tab_id.lock().unwrap();
            let tabs: Vec<Value> = state
                .tabs
                .lock()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(idx, t)| {
                    json!({
                        "index": idx,
                        "id": t.id,
                        "title": t.title,
                        "active": t.id == active_tab_id,
                        "shell_id": t.shell_id,
                        "shell_name": t.shell_name,
                        "cwd": t.cwd.display().to_string(),
                        "block_count": t.blocks.len(),
                        "activity": tab_activity_json(t),
                        "sandbox": t.sandbox_root.as_ref().map(|r| r.display().to_string()),
                    })
                })
                .collect();
            McpToolCallResult::text(
                serde_json::to_string_pretty(&tabs).unwrap_or_default(),
                false,
            )
        }
        "mrt_create_tab" => {
            let requested_shell = args.get("shell").and_then(|v| v.as_str());
            let Some(shell) = state.resolve_shell(requested_shell) else {
                return McpToolCallResult::text("No matching shell available on host", true);
            };
            let cwd = args
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| state.cwd.lock().unwrap().clone());
            let activate = args
                .get("activate")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let mut tabs = state.tabs.lock().unwrap();
            let n = tabs.len() + 1;
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from)
                .unwrap_or_else(|| format!("Tab {n} ({})", shell.kind.short_label()));
            let new_id = Uuid::new_v4();
            let session = McpTabSession {
                id: new_id,
                title: title.clone(),
                shell_id: shell.id.clone(),
                shell_name: shell.kind.short_label().to_string(),
                cwd: cwd.clone(),
                blocks: Vec::new(),
                activity: None,
                sandbox_root: None,
            };
            tabs.push(session);
            drop(tabs);

            if activate {
                *state.active_tab_id.lock().unwrap() = new_id;
                *state.cwd.lock().unwrap() = cwd.clone();
            }
            state.notify_gui(GuiUiCommand::SyncFromMcp);

            let resp = json!({
                "tab_id": new_id,
                "title": title,
                "shell_id": shell.id,
                "cwd": cwd.display().to_string(),
                "activated": activate,
            });
            McpToolCallResult::text(
                serde_json::to_string_pretty(&resp).unwrap_or_default(),
                false,
            )
        }
        "mrt_switch_tab" => {
            let req_id = args.get("tab_id").and_then(|v| v.as_str());
            let Some(target_id) = state.resolve_tab_id(req_id) else {
                return McpToolCallResult::text(
                    format!("Tab not found: {:?}", req_id.unwrap_or("")),
                    true,
                );
            };
            *state.active_tab_id.lock().unwrap() = target_id;
            let tab_info = {
                let tabs = state.tabs.lock().unwrap();
                tabs.iter().find(|t| t.id == target_id).cloned()
            };
            if let Some(ref t) = tab_info {
                *state.cwd.lock().unwrap() = t.cwd.clone();
            }
            state.notify_gui(GuiUiCommand::SyncFromMcp);
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "active_tab_id": target_id,
                    "title": tab_info.as_ref().map(|t| &t.title),
                    "shell_id": tab_info.as_ref().map(|t| &t.shell_id),
                    "cwd": tab_info.as_ref().map(|t| t.cwd.display().to_string()),
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_close_tab" => {
            let req_id = args.get("tab_id").and_then(|v| v.as_str());
            let Some(target_id) = state.resolve_tab_id(req_id) else {
                return McpToolCallResult::text(
                    format!("Tab not found: {:?}", req_id.unwrap_or("")),
                    true,
                );
            };
            let mut tabs = state.tabs.lock().unwrap();
            if tabs.len() <= 1 {
                return McpToolCallResult::text(
                    "Cannot close the last remaining terminal tab",
                    true,
                );
            }
            tabs.retain(|t| t.id != target_id);
            let mut active = state.active_tab_id.lock().unwrap();
            if *active == target_id {
                if let Some(first) = tabs.first() {
                    *active = first.id;
                    *state.cwd.lock().unwrap() = first.cwd.clone();
                }
            }
            let remaining = tabs.len();
            let active_now = *active;
            drop(active);
            drop(tabs);
            state.notify_gui(GuiUiCommand::SyncFromMcp);

            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "closed_tab_id": target_id,
                    "active_tab_id": active_now,
                    "remaining_tabs": remaining,
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_configure_tab" => {
            let req_id = args.get("tab_id").and_then(|v| v.as_str());
            let Some(target_id) = state.resolve_tab_id(req_id) else {
                return McpToolCallResult::text("Target tab not found", true);
            };
            let new_shell = if let Some(s_str) = args.get("shell").and_then(|v| v.as_str()) {
                let Some(s) = state.resolve_shell(Some(s_str)) else {
                    return McpToolCallResult::text(format!("Unknown shell: {s_str}"), true);
                };
                Some(s)
            } else {
                None
            };
            let new_cwd = if let Some(c_str) = args.get("cwd").and_then(|v| v.as_str()) {
                let candidate = PathBuf::from(c_str);
                let resolved = if candidate.is_absolute() {
                    candidate
                } else {
                    let base = state
                        .tabs
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|t| t.id == target_id)
                        .map(|t| t.cwd.clone())
                        .unwrap_or_else(|| state.cwd.lock().unwrap().clone());
                    base.join(candidate)
                };
                if !resolved.is_dir() {
                    return McpToolCallResult::text(
                        format!("Directory does not exist: {}", resolved.display()),
                        true,
                    );
                }
                Some(resolved)
            } else {
                None
            };
            let new_title = args
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(String::from);

            let mut tabs = state.tabs.lock().unwrap();
            let Some(tab) = tabs.iter_mut().find(|t| t.id == target_id) else {
                return McpToolCallResult::text("Target tab not found", true);
            };
            if let Some(s) = new_shell {
                tab.shell_id = s.id;
                tab.shell_name = s.kind.short_label().to_string();
            }
            if let Some(ref c) = new_cwd {
                tab.cwd = c.clone();
            }
            if let Some(t) = new_title {
                tab.title = t;
            }
            let updated = tab.clone();
            drop(tabs);

            if *state.active_tab_id.lock().unwrap() == target_id {
                *state.cwd.lock().unwrap() = updated.cwd.clone();
            }
            state.notify_gui(GuiUiCommand::SyncFromMcp);

            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "tab_id": updated.id,
                    "title": updated.title,
                    "shell_id": updated.shell_id,
                    "shell_name": updated.shell_name,
                    "cwd": updated.cwd.display().to_string(),
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_list_blocks" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let requested_tab = args.get("tab_id").and_then(|v| v.as_str());
            let recent: Vec<Value> = if let Some(tab_str) = requested_tab {
                let Some(tab_id) = state.resolve_tab_id(Some(tab_str)) else {
                    return McpToolCallResult::text(format!("Tab not found: {tab_str}"), true);
                };
                let tabs = state.tabs.lock().unwrap();
                tabs.iter()
                    .find(|t| t.id == tab_id)
                    .map(|t| {
                        t.blocks
                            .iter()
                            .rev()
                            .take(limit)
                            .map(|b| format_block_summary(b, Some(t.id)))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                let guard = state.blocks.lock().unwrap();
                guard
                    .iter()
                    .rev()
                    .take(limit)
                    .map(|b| format_block_summary(b, None))
                    .collect()
            };
            McpToolCallResult::text(
                serde_json::to_string_pretty(&recent).unwrap_or_default(),
                false,
            )
        }
        "mrt_get_block" => {
            let Some(id_str) = args.get("block_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'block_id' parameter", true);
            };
            let Ok(block_id) = Uuid::parse_str(id_str.trim()) else {
                return McpToolCallResult::text(format!("Invalid block UUID: {id_str}"), true);
            };
            let found = {
                let tabs = state.tabs.lock().unwrap();
                tabs.iter()
                    .find_map(|t| t.blocks.iter().find(|b| b.id == block_id).cloned().map(|b| (Some(t.id), b)))
                    .or_else(|| {
                        let global = state.blocks.lock().unwrap();
                        global.iter().find(|b| b.id == block_id).cloned().map(|b| (None, b))
                    })
            };
            let Some((tab_id, block)) = found else {
                return McpToolCallResult::text(format!("Block not found: {block_id}"), true);
            };
            let detail = json!({
                "id": block.id,
                "tab_id": tab_id,
                "command": block.command,
                "shell": block.shell_name,
                "cwd": block.cwd,
                "timestamp": block.timestamp,
                "duration_ms": block.duration_ms,
                "exit_code": block.exit_code,
                "structured_type": block.structured.badge_label(),
                "structured_data": block.structured,
                "plain_output": block.plain_output,
                "raw_output": block.raw_output,
                "ai_explanation": block.ai_explanation,
                "ai_suggested_fix": block.ai_suggested_fix,
            });
            McpToolCallResult::text(
                serde_json::to_string_pretty(&detail).unwrap_or_default(),
                false,
            )
        }
        "mrt_search_blocks" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let failed_only = args
                .get("failed_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(20) as usize;
            let requested_tab = args
                .get("tab_id")
                .and_then(|v| v.as_str())
                .and_then(|s| state.resolve_tab_id(Some(s)));

            let tabs = state.tabs.lock().unwrap();
            let mut results = Vec::new();
            for t in tabs.iter() {
                if let Some(tid) = requested_tab {
                    if t.id != tid {
                        continue;
                    }
                }
                for b in t.blocks.iter().rev() {
                    if failed_only && b.exit_code.unwrap_or(0) == 0 {
                        continue;
                    }
                    if !query.is_empty()
                        && !b.command.to_lowercase().contains(&query)
                        && !b.plain_output.to_lowercase().contains(&query)
                    {
                        continue;
                    }
                    results.push(format_block_summary(b, Some(t.id)));
                    if results.len() >= limit {
                        break;
                    }
                }
                if results.len() >= limit {
                    break;
                }
            }
            if results.is_empty() && requested_tab.is_none() {
                let global = state.blocks.lock().unwrap();
                for b in global.iter().rev() {
                    if failed_only && b.exit_code.unwrap_or(0) == 0 {
                        continue;
                    }
                    if !query.is_empty()
                        && !b.command.to_lowercase().contains(&query)
                        && !b.plain_output.to_lowercase().contains(&query)
                    {
                        continue;
                    }
                    results.push(format_block_summary(b, None));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
            McpToolCallResult::text(
                serde_json::to_string_pretty(&results).unwrap_or_default(),
                false,
            )
        }
        "mrt_compare_blocks" => {
            let Some(prev_str) = args.get("previous_block_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing 'previous_block_id'", true);
            };
            let Some(curr_str) = args.get("current_block_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing 'current_block_id'", true);
            };
            let (Ok(prev_id), Ok(curr_id)) = (Uuid::parse_str(prev_str.trim()), Uuid::parse_str(curr_str.trim())) else {
                return McpToolCallResult::text("Invalid block UUID", true);
            };
            let find_blk = |id: Uuid| -> Option<(Option<Uuid>, CommandBlock)> {
                let tabs = state.tabs.lock().unwrap();
                tabs.iter()
                    .find_map(|t| t.blocks.iter().find(|b| b.id == id).cloned().map(|b| (Some(t.id), b)))
                    .or_else(|| {
                        let global = state.blocks.lock().unwrap();
                        global.iter().find(|b| b.id == id).cloned().map(|b| (None, b))
                    })
            };
            let Some((tab_a, prev_blk)) = find_blk(prev_id) else {
                return McpToolCallResult::text(format!("Block not found: {prev_id}"), true);
            };
            let Some((tab_b, curr_blk)) = find_blk(curr_id) else {
                return McpToolCallResult::text(format!("Block not found: {curr_id}"), true);
            };

            let report = compare_blocks_structured(&prev_blk, &curr_blk);

            let show_in_gui = args
                .get("show_in_gui")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if show_in_gui {
                state.notify_gui(GuiUiCommand::SetComparison {
                    tab_id: tab_b.or(tab_a),
                    previous_block_id: prev_id,
                    current_block_id: curr_id,
                });
            }

            let (diff_text, is_identical) = match &report.kind {
                StructuredDiffKind::Identical { summary } => (summary.clone(), true),
                StructuredDiffKind::Json { added_paths, removed_paths, modified_paths, diff_lines } => {
                    let mut s = format!(
                        "JSON Diff (+{} keys, -{} keys, ~{} modified values):\n\n",
                        added_paths.len(),
                        removed_paths.len(),
                        modified_paths.len()
                    );
                    for (line, tag) in diff_lines {
                        let sign = match tag {
                            ChangeType::Remove => "-",
                            ChangeType::Add => "+",
                            ChangeType::Equal => " ",
                        };
                        s.push_str(&format!("{sign}{line}\n"));
                    }
                    (s, false)
                }
                StructuredDiffKind::PortsDelta { added_listeners, removed_listeners, unchanged_count } => {
                    let mut s = format!("Listening Ports Delta ({unchanged_count} unchanged):\n");
                    for a in added_listeners {
                        s.push_str(&format!("+ {a}\n"));
                    }
                    for r in removed_listeners {
                        s.push_str(&format!("- {r}\n"));
                    }
                    (s, false)
                }
                StructuredDiffKind::ProcessesDelta { started, exited, memory_grown, unchanged_count } => {
                    (format_processes_delta(started, exited, memory_grown, *unchanged_count), false)
                }
                StructuredDiffKind::Text { diff_lines } => {
                    let mut s = String::new();
                    for (line, tag) in diff_lines {
                        let sign = match tag {
                            ChangeType::Remove => "-",
                            ChangeType::Add => "+",
                            ChangeType::Equal => " ",
                        };
                        s.push_str(&format!("{sign}{line}\n"));
                    }
                    (s, false)
                }
            };

            let payload = json!({
                "previous_block_id": prev_id,
                "current_block_id": curr_id,
                "identical": is_identical,
                "structured_kind": match &report.kind {
                    StructuredDiffKind::Identical { .. } => "identical",
                    StructuredDiffKind::Json { .. } => "json",
                    StructuredDiffKind::PortsDelta { .. } => "ports_delta",
                    StructuredDiffKind::ProcessesDelta { .. } => "processes_delta",
                    StructuredDiffKind::Text { .. } => "text",
                },
                "diff": diff_text,
            });
            McpToolCallResult::text(
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
                false,
            )
        }
        "mrt_diagnose_block" => {
            let Some(id_str) = args.get("block_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'block_id' parameter", true);
            };
            let Ok(block_id) = Uuid::parse_str(id_str.trim()) else {
                return McpToolCallResult::text(format!("Invalid block UUID: {id_str}"), true);
            };

            let mut diagnosed = None;
            {
                let mut tabs = state.tabs.lock().unwrap();
                for t in tabs.iter_mut() {
                    if let Some(b) = t.blocks.iter_mut().find(|b| b.id == block_id) {
                        let shell = state
                            .resolve_shell(Some(&b.shell_name))
                            .unwrap_or_else(|| state.shells[0].clone());
                        let triage = diagnose_command_error(
                            &b.command,
                            &b.plain_output,
                            b.exit_code.unwrap_or(1),
                            &shell,
                        );
                        b.ai_explanation = Some(triage.explanation.clone());
                        b.ai_suggested_fix = triage.suggested_fix.clone();
                        diagnosed = Some((b.command.clone(), b.exit_code, triage));
                        break;
                    }
                }
            }
            {
                let mut global = state.blocks.lock().unwrap();
                if let Some(b) = global.iter_mut().find(|b| b.id == block_id) {
                    let shell = state
                        .resolve_shell(Some(&b.shell_name))
                        .unwrap_or_else(|| state.shells[0].clone());
                    let triage = diagnose_command_error(
                        &b.command,
                        &b.plain_output,
                        b.exit_code.unwrap_or(1),
                        &shell,
                    );
                    b.ai_explanation = Some(triage.explanation.clone());
                    b.ai_suggested_fix = triage.suggested_fix.clone();
                    if diagnosed.is_none() {
                        diagnosed = Some((b.command.clone(), b.exit_code, triage));
                    }
                }
            }

            let Some((cmd, code, triage)) = diagnosed else {
                return McpToolCallResult::text(format!("Block not found: {block_id}"), true);
            };
            state.notify_gui(GuiUiCommand::SyncFromMcp);
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "block_id": block_id,
                    "command": cmd,
                    "exit_code": code,
                    "explanation": triage.explanation,
                    "suggested_fix": triage.suggested_fix,
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_clear_blocks" => {
            let clear_all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
            if clear_all {
                for t in state.tabs.lock().unwrap().iter_mut() {
                    t.blocks.clear();
                }
                state.blocks.lock().unwrap().clear();
            } else {
                let req_tab = args.get("tab_id").and_then(|v| v.as_str());
                let Some(tab_id) = state.resolve_tab_id(req_tab) else {
                    return McpToolCallResult::text("Target tab not found", true);
                };
                if let Some(t) = state.tabs.lock().unwrap().iter_mut().find(|t| t.id == tab_id) {
                    t.blocks.clear();
                }
            }
            state.notify_gui(GuiUiCommand::SyncFromMcp);
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({ "cleared": true, "all": clear_all }))
                    .unwrap_or_default(),
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

            let req_tab = args.get("tab_id").and_then(|v| v.as_str());
            let target_tab_id = state.resolve_tab_id(req_tab);

            let tab_defaults = target_tab_id.and_then(|tid| {
                state
                    .tabs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|t| t.id == tid)
                    .map(|t| (t.shell_id.clone(), t.cwd.clone()))
            });
            let sandbox_root = target_tab_id.and_then(|tid| {
                state.tabs.lock().unwrap().iter().find(|t| t.id == tid).and_then(|t| t.sandbox_root.clone())
            });
            // Sandbox tabs: only Safe commands run unattended, always from the sandbox folder.
            if sandbox_root.is_some() && safety.risk != RiskLevel::Safe {
                return McpToolCallResult::text(
                    format!(
                        "Refused: the target is an agent sandbox tab, where only commands the Safety Gate rates Safe run without approval ({} risk: {}). Ask the user to run it, or send it with mrt_control_gui (input_text + submit_input) so they can approve it in the MRT window.",
                        safety.risk.label(),
                        safety.reason
                    ),
                    true,
                );
            }

            let requested_shell = args
                .get("shell")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| tab_defaults.as_ref().map(|(s, _)| s.clone()));

            let Some(shell_profile) = state.resolve_shell(requested_shell.as_deref()) else {
                return McpToolCallResult::text("No shell available on host", true);
            };

            let cwd = match &sandbox_root {
                Some(root) => root.clone(),
                None => args
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .or_else(|| tab_defaults.map(|(_, c)| c))
                    .unwrap_or_else(|| state.cwd.lock().unwrap().clone()),
            };

            match execute_command_in_shell(&shell_profile, cmd, &cwd).await {
                Ok(res) => {
                    // A sandbox tab's folder never moves.
                    let res = if sandbox_root.is_some() {
                        mrt_core::pty::CommandExecutionResult { updated_cwd: None, ..res }
                    } else {
                        res
                    };
                    let effective_cwd = res.updated_cwd.clone().unwrap_or_else(|| cwd.clone());
                    if res.updated_cwd.is_some() {
                        *state.cwd.lock().unwrap() = effective_cwd.clone();
                    }
                    let block = CommandBlock::new_completed(
                        cmd,
                        shell_profile.kind.short_label(),
                        cwd.display().to_string(),
                        &res.combined_output,
                        res.exit_code,
                        res.duration_ms,
                    );
                    let block_id = block.id;
                    let structured_badge = block.structured.badge_label();

                    state.blocks.lock().unwrap().push(block.clone());
                    if let Some(tid) = target_tab_id {
                        let mut tabs = state.tabs.lock().unwrap();
                        if let Some(t) = tabs.iter_mut().find(|t| t.id == tid) {
                            if res.updated_cwd.is_some() {
                                t.cwd = effective_cwd.clone();
                            }
                            t.blocks.push(block);
                        }
                    }
                    state.notify_gui(GuiUiCommand::SyncFromMcp);

                    let summary = json!({
                        "block_id": block_id,
                        "tab_id": target_tab_id,
                        "shell": shell_profile.id,
                        "cwd": effective_cwd.display().to_string(),
                        "exit_code": res.exit_code,
                        "duration_ms": res.duration_ms,
                        "structured_type": structured_badge,
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
        "mrt_pty_spawn" => {
            let requested_shell = args.get("shell").and_then(|v| v.as_str());
            let Some(shell) = state.resolve_shell(requested_shell) else {
                return McpToolCallResult::text("No shell available on host", true);
            };
            let initial_cmd = args.get("command").and_then(|v| v.as_str());
            if let Some(cmd) = initial_cmd {
                let safety = assess_command(cmd);
                if safety.risk == RiskLevel::Critical {
                    return McpToolCallResult::text(
                        format!(
                            "BLOCKED BY MRT SAFETY GATE: Command '{}' flagged as {} ({}).",
                            cmd,
                            safety.risk.label(),
                            safety.reason
                        ),
                        true,
                    );
                }
            }
            let cwd = args
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| state.cwd.lock().unwrap().clone());
            let cols = args
                .get("cols")
                .and_then(|v| v.as_u64())
                .unwrap_or(120)
                .clamp(20, 500) as u16;
            let rows = args
                .get("rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(36)
                .clamp(5, 200) as u16;
            let wait_ms = args
                .get("wait_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(150)
                .min(5000);

            match InteractivePty::spawn_with_command(&shell, &cwd, cols, rows, initial_cmd) {
                Ok(pty) => {
                    let pty_id = pty.id;
                    if let Some(pid) = pty.process_id() {
                        state.owned_processes.lock().unwrap().insert(
                            pid,
                            OwnedProcess {
                                pid,
                                label: format!("MCP PTY {}", &pty_id.to_string()[..8]),
                                tab_id: None,
                                kind: OwnedProcessKind::Pty,
                            },
                        );
                    }
                    let arc_pty = Arc::new(pty);
                    state.ptys.lock().unwrap().insert(pty_id, Arc::clone(&arc_pty));
                    if wait_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                    }
                    let raw = arc_pty.drain_output();
                    let (_, plain) = parse_ansi_text(&raw);
                    let exit_code = arc_pty.try_wait_exit().ok().flatten();
                    let payload = json!({
                        "pty_id": pty_id,
                        "shell": shell.id,
                        "cwd": cwd.display().to_string(),
                        "cols": cols,
                        "rows": rows,
                        "running": exit_code.is_none(),
                        "exit_code": exit_code,
                        "initial_output": plain,
                    });
                    McpToolCallResult::text(
                        serde_json::to_string_pretty(&payload).unwrap_or_default(),
                        false,
                    )
                }
                Err(e) => McpToolCallResult::text(format!("Failed to spawn PTY: {e}"), true),
            }
        }
        "mrt_pty_write" => {
            let Some(id_str) = args.get("pty_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'pty_id'", true);
            };
            let Some(input) = args.get("input").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'input'", true);
            };
            let safety = assess_command(input);
            if safety.risk == RiskLevel::Critical {
                return McpToolCallResult::text(
                    format!(
                        "BLOCKED BY MRT SAFETY GATE: Input '{}' flagged as {} ({}).",
                        input,
                        safety.risk.label(),
                        safety.reason
                    ),
                    true,
                );
            }
            let Ok(pty_id) = Uuid::parse_str(id_str.trim()) else {
                return McpToolCallResult::text(format!("Invalid pty_id UUID: {id_str}"), true);
            };
            let pty = state.ptys.lock().unwrap().get(&pty_id).cloned();
            let Some(pty) = pty else {
                return McpToolCallResult::text(format!("PTY session not found: {pty_id}"), true);
            };
            let append_newline = args
                .get("append_newline")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let wait_ms = args
                .get("wait_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(150)
                .min(5000);

            if let Err(e) = pty.send_input(input, append_newline) {
                return McpToolCallResult::text(format!("PTY write error: {e}"), true);
            }
            if wait_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            }
            let raw = pty.drain_output();
            let (_, plain) = parse_ansi_text(&raw);
            let exit_code = pty.try_wait_exit().ok().flatten();
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "pty_id": pty_id,
                    "running": exit_code.is_none(),
                    "exit_code": exit_code,
                    "output": plain,
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_pty_read" => {
            let Some(id_str) = args.get("pty_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'pty_id'", true);
            };
            let Ok(pty_id) = Uuid::parse_str(id_str.trim()) else {
                return McpToolCallResult::text(format!("Invalid pty_id UUID: {id_str}"), true);
            };
            let pty = state.ptys.lock().unwrap().get(&pty_id).cloned();
            let Some(pty) = pty else {
                return McpToolCallResult::text(format!("PTY session not found: {pty_id}"), true);
            };
            let drain = args.get("drain").and_then(|v| v.as_bool()).unwrap_or(true);
            let strip_ansi = args
                .get("strip_ansi")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let wait_ms = args
                .get("wait_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(5000);

            if wait_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            }
            if args.get("view").and_then(|v| v.as_str()) == Some("screen") {
                let exit_code = pty.try_wait_exit().ok().flatten();
                let mut payload = pty_screen_view(&pty);
                payload["pty_id"] = json!(pty_id);
                payload["running"] = json!(exit_code.is_none());
                payload["exit_code"] = json!(exit_code);
                return McpToolCallResult::text(serde_json::to_string_pretty(&payload).unwrap_or_default(), false);
            }
            let raw = if drain {
                pty.drain_output()
            } else {
                pty.read_transcript()
            };
            let output = if strip_ansi {
                parse_ansi_text(&raw).1
            } else {
                raw
            };
            let exit_code = pty.try_wait_exit().ok().flatten();
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "pty_id": pty_id,
                    "running": exit_code.is_none(),
                    "exit_code": exit_code,
                    "output": output,
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_pty_resize" => {
            let Some(id_str) = args.get("pty_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'pty_id'", true);
            };
            let Ok(pty_id) = Uuid::parse_str(id_str.trim()) else {
                return McpToolCallResult::text(format!("Invalid pty_id UUID: {id_str}"), true);
            };
            let cols = args
                .get("cols")
                .and_then(|v| v.as_u64())
                .unwrap_or(120)
                .clamp(20, 500) as u16;
            let rows = args
                .get("rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(36)
                .clamp(5, 200) as u16;
            let pty = state.ptys.lock().unwrap().get(&pty_id).cloned();
            let Some(pty) = pty else {
                return McpToolCallResult::text(format!("PTY session not found: {pty_id}"), true);
            };
            match pty.resize(cols, rows) {
                Ok(()) => McpToolCallResult::text(
                    serde_json::to_string_pretty(&json!({
                        "pty_id": pty_id,
                        "cols": cols,
                        "rows": rows,
                    }))
                    .unwrap_or_default(),
                    false,
                ),
                Err(e) => McpToolCallResult::text(format!("PTY resize failed: {e}"), true),
            }
        }
        "mrt_pty_list" => {
            let guard = state.ptys.lock().unwrap();
            let list: Vec<Value> = guard
                .values()
                .map(|pty| {
                    let exit_code = pty.try_wait_exit().ok().flatten();
                    json!({
                        "pty_id": pty.id,
                        "shell_id": pty.shell_id,
                        "shell_name": pty.shell_name,
                        "cwd": pty.cwd.display().to_string(),
                        "cols": pty.size().0,
                        "rows": pty.size().1,
                        "running": exit_code.is_none(),
                        "exit_code": exit_code,
                    })
                })
                .collect();
            McpToolCallResult::text(
                serde_json::to_string_pretty(&list).unwrap_or_default(),
                false,
            )
        }
        "mrt_pty_kill" => {
            let Some(id_str) = args.get("pty_id").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'pty_id'", true);
            };
            let Ok(pty_id) = Uuid::parse_str(id_str.trim()) else {
                return McpToolCallResult::text(format!("Invalid pty_id UUID: {id_str}"), true);
            };
            let remove = args.get("remove").and_then(|v| v.as_bool()).unwrap_or(true);
            let pty = if remove {
                state.ptys.lock().unwrap().remove(&pty_id)
            } else {
                state.ptys.lock().unwrap().get(&pty_id).cloned()
            };
            let Some(pty) = pty else {
                return McpToolCallResult::text(format!("PTY session not found: {pty_id}"), true);
            };
            if let Some(pid) = pty.process_id() {
                state.owned_processes.lock().unwrap().remove(&pid);
            }
            let _ = pty.kill();
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "pty_id": pty_id,
                    "killed": true,
                    "removed": remove,
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_control_gui" => {
            let tab_id = args
                .get("tab_id")
                .and_then(|v| v.as_str())
                .and_then(|s| state.resolve_tab_id(Some(s)));
            if let Some(text) = args.get("input_text").and_then(|v| v.as_str()) {
                let submit = args
                    .get("submit_input")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                state.notify_gui(GuiUiCommand::SetInputDraft {
                    tab_id,
                    text: text.to_string(),
                    submit,
                });
            }
            if let Some(mosaic) = args.get("mosaic").and_then(|v| v.as_bool()) {
                let columns = args
                    .get("mosaic_columns")
                    .and_then(|v| v.as_u64())
                    .map(|c| c.clamp(1, 6) as usize);
                let layout = args
                    .get("mosaic_layout")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                state.notify_gui(GuiUiCommand::SetMosaic {
                    enabled: mosaic,
                    columns,
                    layout,
                });
            }
            if let Some(explorer) = args.get("explorer").and_then(|v| v.as_bool()) {
                state.notify_gui(GuiUiCommand::SetExplorer { visible: explorer });
            }
            match args.get("toolbox") {
                Some(Value::Bool(visible)) => state.notify_gui(GuiUiCommand::SetToolbox {
                    visible: *visible,
                    tab: None,
                }),
                Some(Value::String(tab)) => state.notify_gui(GuiUiCommand::SetToolbox {
                    visible: !tab.eq_ignore_ascii_case("hidden"),
                    tab: Some(tab.clone()),
                }),
                _ => {}
            }
            McpToolCallResult::text(
                serde_json::to_string_pretty(&json!({
                    "gui_attached": state.gui_tx.is_some(),
                    "status": "dispatched"
                }))
                .unwrap_or_default(),
                false,
            )
        }
        "mrt_ai_translate" => {
            let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'query' parameter", true);
            };
            let req_shell = args.get("shell").and_then(|v| v.as_str());
            let Some(shell) = state.resolve_shell(req_shell) else {
                return McpToolCallResult::text("No shell available", true);
            };
            let cwd = state.cwd.lock().unwrap().display().to_string();
            let tool_names: Vec<String> = MrtMcpServerState::built_in_tools()
                .into_iter()
                .map(|t| t.name)
                .collect();
            match translate_nl_to_command(query, &shell, &cwd, &tool_names).await {
                Ok(prop) => McpToolCallResult::text(
                    serde_json::to_string_pretty(&prop).unwrap_or_default(),
                    false,
                ),
                Err(e) => McpToolCallResult::text(format!("AI translation error: {e}"), true),
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
        "mrt_inspect_dns" => {
            let Some(host) = args.get("host").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'host' parameter", true);
            };
            match inspect_dns(host).await {
                Ok((_, plain, _)) => McpToolCallResult::text(plain, false),
                Err(e) => McpToolCallResult::text(format!("DNS inspection error: {e}"), true),
            }
        }
        "mrt_generate_bug_report" => {
            let requested_id = args
                .get("block_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s.trim()).ok());
            let tabs = state.tabs.lock().unwrap();
            let mut target_blk = None;
            let mut preceding = Vec::new();

            if let Some(id) = requested_id {
                for tab in tabs.iter() {
                    if let Some(pos) = tab.blocks.iter().position(|b| b.id == id) {
                        target_blk = Some(tab.blocks[pos].clone());
                        preceding = tab.blocks[..pos].to_vec();
                        break;
                    }
                }
            } else {
                // Find most recent failed block across tabs
                for tab in tabs.iter() {
                    if let Some(pos) = tab
                        .blocks
                        .iter()
                        .rposition(|b| b.exit_code.map(|c| c != 0).unwrap_or(false))
                    {
                        target_blk = Some(tab.blocks[pos].clone());
                        preceding = tab.blocks[..pos].to_vec();
                        break;
                    }
                }
                // Fallback to last block if no failed block
                if target_blk.is_none() {
                    if let Some(tab) = tabs.first() {
                        if let Some(last) = tab.blocks.last() {
                            target_blk = Some(last.clone());
                            let count = tab.blocks.len().saturating_sub(1);
                            preceding = tab.blocks[..count].to_vec();
                        }
                    }
                }
            }

            let Some(target) = target_blk else {
                return McpToolCallResult::text("No block found to generate report for", true);
            };

            let os_info = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
            let report = generate_markdown_bug_report(&target, &preceding, &os_info, env!("CARGO_PKG_VERSION"));
            McpToolCallResult::text(report, false)
        }
        "mrt_inspect_ws" => {
            let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
                return McpToolCallResult::text("Missing required 'url' parameter", true);
            };
            let mut cmd = format!(":ws {url}");
            if let Some(msg) = args.get("message").and_then(|v| v.as_str()) {
                cmd.push_str(&format!(" --send {msg}"));
            }
            if let Some(t) = args.get("timeout_ms").and_then(|v| v.as_u64()) {
                cmd.push_str(&format!(" --timeout {t}"));
            }

            match inspect_websocket(&cmd).await {
                Ok((_, plain, _)) => McpToolCallResult::text(plain, false),
                Err(e) => McpToolCallResult::text(format!("WebSocket inspection error: {e}"), true),
            }
        }
        "mrt_list_processes" => {
            let mut query = ProcessQuery {
                filter: args
                    .get("filter")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|f| !f.trim().is_empty()),
                limit: Some(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).clamp(1, 2000) as usize),
                tree: args.get("tree").and_then(|v| v.as_bool()).unwrap_or(false),
                only_mrt: args.get("only_mrt").and_then(|v| v.as_bool()).unwrap_or(false),
                ..Default::default()
            };
            if let Some(sort) = args.get("sort").and_then(|v| v.as_str()) {
                match ProcessSort::parse(sort) {
                    Some(s) => query.sort = s,
                    None => {
                        return McpToolCallResult::text(
                            format!("Unknown sort '{sort}' (use cpu, mem, pid or name)"),
                            true,
                        )
                    }
                }
            }
            match list_processes_value(state, &query).await {
                Ok(v) => McpToolCallResult::text(serde_json::to_string_pretty(&v).unwrap_or_default(), false),
                Err(e) => McpToolCallResult::text(format!("Process listing error: {e}"), true),
            }
        }
        "mrt_inspect_process" => {
            let Some(pid) = args.get("pid").and_then(|v| v.as_u64()) else {
                return McpToolCallResult::text("Missing required integer 'pid'", true);
            };
            let owned = state.owned_process_map();
            match inspect_process(&state.process_sampler, pid as u32, &owned).await {
                Ok(details) => McpToolCallResult::text(
                    serde_json::to_string_pretty(&details).unwrap_or_default(),
                    false,
                ),
                Err(e) => McpToolCallResult::text(format!("Process inspection error: {e}"), true),
            }
        }
        "mrt_kill_process" => {
            let mut selectors = Vec::new();
            if let Some(pid) = args.get("pid").and_then(|v| v.as_u64()) {
                selectors.push(ProcessSelector::Pid(pid as u32));
            }
            if let Some(name) = args.get("name").and_then(|v| v.as_str()).filter(|n| !n.trim().is_empty()) {
                selectors.push(ProcessSelector::Name(name.trim().to_string()));
            }
            if selectors.is_empty() {
                return McpToolCallResult::text("Provide 'pid' or 'name'", true);
            }
            let request = KillRequest {
                selectors,
                force: args.get("force").and_then(|v| v.as_bool()).unwrap_or(false),
                tree: args.get("tree").and_then(|v| v.as_bool()).unwrap_or(false),
            };
            let snapshot = match state.process_sampler.snapshot().await {
                Ok(s) => s,
                Err(e) => return McpToolCallResult::text(format!("Process snapshot error: {e}"), true),
            };
            let owned = state.owned_process_map();
            let plan = match plan_kill(&snapshot, &request, &owned) {
                Ok(p) => p,
                Err(e) => return McpToolCallResult::text(e.to_string(), true),
            };
            if plan.assessment.risk == RiskLevel::Critical {
                return McpToolCallResult::text(
                    format!(
                        "BLOCKED BY MRT SAFETY GATE: '{}' flagged as {}.\n{}",
                        plan.assessment.command,
                        plan.assessment.risk.label(),
                        plan.assessment.reason
                    ),
                    true,
                );
            }
            if state.gui_tx.is_some() {
                state.notify_gui(GuiUiCommand::RequestProcessKill {
                    pids: plan.pids(),
                    force: request.force,
                    tree: request.tree,
                });
                return McpToolCallResult::text(
                    serde_json::to_string_pretty(&json!({
                        "status": "pending_approval",
                        "message": "The MRT Safety Approval Gate is asking the user to approve this stop request in the GUI. Check the result later with mrt_list_processes or mrt_inspect_process.",
                        "command": plan.assessment.command,
                        "risk": plan.assessment.risk.label(),
                        "reason": plan.assessment.reason,
                    }))
                    .unwrap_or_default(),
                    false,
                );
            }
            let all_owned = plan
                .targets
                .iter()
                .all(|t| owner_label(&snapshot, t.pid, &owned).is_some());
            if !all_owned {
                return McpToolCallResult::text(
                    format!(
                        "BLOCKED BY MRT SAFETY GATE: '{}' targets processes MRT did not start. Stopping them requires interactive approval inside the MRT GUI.\n{}",
                        plan.assessment.command, plan.assessment.reason
                    ),
                    true,
                );
            }
            match state
                .process_sampler
                .terminate(plan.pids(), request.force, request.tree)
                .await
            {
                Ok(outcomes) => {
                    let failed = outcomes.iter().any(|o| !o.succeeded());
                    McpToolCallResult::text(serde_json::to_string_pretty(&outcomes).unwrap_or_default(), failed)
                }
                Err(e) => McpToolCallResult::text(format!("Process termination error: {e}"), true),
            }
        }
        "mrt_compare_process_snapshots" => {
            let prev_id = args
                .get("previous_block_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s.trim()).ok());
            let curr_id = args
                .get("current_block_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s.trim()).ok());
            if let (Some(prev_id), Some(curr_id)) = (prev_id, curr_id) {
                let find = |id: Uuid| -> Option<CommandBlock> {
                    let tabs = state.tabs.lock().unwrap();
                    tabs.iter()
                        .flat_map(|t| t.blocks.iter())
                        .chain(state.blocks.lock().unwrap().iter())
                        .find(|b| b.id == id)
                        .cloned()
                };
                let (Some(prev), Some(curr)) = (find(prev_id), find(curr_id)) else {
                    return McpToolCallResult::text("One or both block ids were not found", true);
                };
                let report = compare_blocks_structured(&prev, &curr);
                let text = match &report.kind {
                    StructuredDiffKind::ProcessesDelta { started, exited, memory_grown, unchanged_count } => {
                        format_processes_delta(started, exited, memory_grown, *unchanged_count)
                    }
                    StructuredDiffKind::Identical { summary } => summary.clone(),
                    _ => {
                        return McpToolCallResult::text(
                            "Both blocks must be :ps or :top process snapshots",
                            true,
                        )
                    }
                };
                return McpToolCallResult::text(text, false);
            }

            let snapshot = match state.process_sampler.snapshot().await {
                Ok(s) => s,
                Err(e) => return McpToolCallResult::text(format!("Process snapshot error: {e}"), true),
            };
            let reset = args.get("reset_baseline").and_then(|v| v.as_bool()).unwrap_or(false);
            let previous = state.process_baseline.lock().unwrap().replace(snapshot.clone());
            match previous {
                Some(before) if !reset => {
                    let report = compare_process_snapshots(&before.entries, &snapshot.entries);
                    let mut text = format_processes_delta(
                        &report.started_lines(),
                        &report.exited_lines(),
                        &report.memory_lines(),
                        report.unchanged_count,
                    );
                    text.push_str("\nThe current snapshot is now the baseline for the next comparison.\n");
                    McpToolCallResult::text(text, false)
                }
                _ => McpToolCallResult::text(
                    format!(
                        "Saved a baseline of {} processes. Call mrt_compare_process_snapshots again to see what changed.",
                        snapshot.entries.len()
                    ),
                    false,
                ),
            }
        }
        other => McpToolCallResult::text(format!("Unknown MCP tool: {other}"), true),
    }
}

async fn list_processes_value(state: &MrtMcpServerState, query: &ProcessQuery) -> Result<Value> {
    let snapshot = state.process_sampler.snapshot().await?;
    let owned = state.owned_process_map();
    let rows = query_processes(&snapshot, query, &owned);
    let processes: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "pid": r.entry.pid,
                "parent_pid": r.entry.parent_pid,
                "name": r.entry.name,
                "cpu_percent": (r.entry.cpu_percent * 10.0).round() / 10.0,
                "memory_bytes": r.entry.memory_bytes,
                "memory": mrt_tools::format_bytes(r.entry.memory_bytes),
                "user": r.entry.user,
                "status": r.entry.status,
                "command_line": r.entry.command_line,
                "run_time_secs": r.entry.run_time_secs,
                "mrt_owner": r.owner,
                "depth": r.depth,
            })
        })
        .collect();
    Ok(json!({
        "summary": mrt_tools::snapshot_summary(&snapshot, rows.len()),
        "total_processes": snapshot.entries.len(),
        "processes": processes,
    }))
}

fn format_processes_delta(
    started: &[String],
    exited: &[String],
    memory_grown: &[String],
    unchanged_count: usize,
) -> String {
    let mut s = format!("Process Snapshot Delta ({unchanged_count} unchanged):\n");
    for line in started {
        s.push_str(&format!("+ started: {line}\n"));
    }
    for line in exited {
        s.push_str(&format!("- exited: {line}\n"));
    }
    for line in memory_grown {
        s.push_str(&format!("▲ memory: {line}\n"));
    }
    s
}

/// What a PTY session's terminal shows now, rebuilt by replaying its whole transcript
/// into a `TerminalEmulator` at the session's current size. Replaying (rather than
/// keeping an emulator fed with live output) leaves `drain_output` to the other tools.
/// A session resized midway is replayed at its final size; full-screen programs
/// redraw after a resize, so their screen still comes out right.
fn pty_screen_view(pty: &InteractivePty) -> Value {
    let (cols, rows) = pty.size();
    let mut term = mrt_core::TerminalEmulator::new(cols, rows, 0);
    term.feed(pty.read_transcript().as_bytes());
    let cursor = term.cursor();
    let screen: Vec<String> = term.screen_text().split('\n').map(str::to_string).collect();
    json!({
        "cols": cols,
        "rows": rows,
        "screen": screen,
        "cursor": { "row": cursor.row, "col": cursor.col, "visible": cursor.visible },
        "alt_screen": term.is_alt_screen(),
        "title": term.title(),
    })
}

fn format_block_summary(b: &CommandBlock, tab_id: Option<Uuid>) -> Value {
    json!({
        "id": b.id,
        "tab_id": tab_id,
        "command": b.command,
        "shell": b.shell_name,
        "cwd": b.cwd,
        "exit_code": b.exit_code,
        "duration_ms": b.duration_ms,
        "structured_type": b.structured.badge_label(),
        "output": b.plain_output,
        "ai_explanation": b.ai_explanation,
        "ai_suggested_fix": b.ai_suggested_fix,
    })
}

/// Runs MRT in headless stdio MCP Server mode (`mrt --mcp-server`), reading JSON-RPC lines
/// from stdin and writing JSON-RPC responses to stdout.
/// If a live MRT GUI window is running on the host (discovered via `mcp-bridge.json`),
/// requests are transparently proxied to the live GUI so external agents control the open terminal!
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
            Ok(req) => {
                if let Some((port, token)) = read_bridge_endpoint() {
                    if let Ok(forwarded) = try_forward_jsonrpc_to_gui(port, &token, &req).await {
                        forwarded
                    } else {
                        handle_mcp_jsonrpc(&state, req).await
                    }
                } else {
                    handle_mcp_jsonrpc(&state, req).await
                }
            }
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

#[cfg(test)]
mod sandbox_tests {
    use super::*;

    async fn call(state: &MrtMcpServerState, name: &str, arguments: Value) -> (String, bool) {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": arguments })),
        };
        let resp = handle_mcp_jsonrpc(state, req).await.unwrap();
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap_or("").to_string();
        (text, result["isError"].as_bool().unwrap_or(false))
    }

    #[tokio::test]
    async fn sandbox_tabs_run_only_safe_commands_in_their_folder() {
        let state = MrtMcpServerState::default();
        let root = std::env::temp_dir().join(format!("mrt-sandbox-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        {
            let mut tabs = state.tabs.lock().unwrap();
            tabs[0].sandbox_root = Some(root.clone());
            tabs[0].cwd = root.clone();
        }
        let moderate = "del notes.txt";
        assert_eq!(assess_command(moderate).risk, RiskLevel::Moderate);
        let (text, is_error) = call(&state, "mrt_run_command", json!({ "command": moderate, "tab_id": "0" })).await;
        assert!(is_error);
        assert!(text.contains("sandbox"), "{text}");

        let (text, _) = call(&state, "mrt_run_command", json!({ "command": "echo inside", "tab_id": "0", "cwd": "/" })).await;
        let summary: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(summary["exit_code"], 0, "{text}");
        assert_eq!(summary["cwd"].as_str().unwrap(), root.display().to_string(), "the cwd argument is ignored");

        let (text, _) = call(&state, "mrt_list_tabs", json!({})).await;
        assert!(text.contains("\"sandbox\""), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
