use crate::widgets::block_view::{render_command_block, BlockAction};
use crate::widgets::side_panels::{
    render_internet_side_panel, render_mcp_side_panel, SidePanelEvent, SidePanelState,
};
use egui::{Color32, Key, Modifiers, RichText};
use mrt_ai::{diagnose_command_error, translate_nl_to_command, AiCommandProposal};
use mrt_core::{
    assess_command, discover_shells, execute_command_in_shell, CommandBlock, RiskLevel,
    SafetyAssessment, ShellProfile, StructuredOutput,
};
use mrt_mcp::{McpHostManager, MrtMcpServerState};
use mrt_tools::{
    execute_http_request, fetch_url_as_markdown, inspect_listening_ports, parse_http_command,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use uuid::Uuid;

pub struct TerminalTab {
    pub id: Uuid,
    pub title: String,
    pub active_shell_idx: usize,
    pub cwd: PathBuf,
    pub blocks: Vec<CommandBlock>,
    pub input_line: String,
    pub ai_proposal: Option<AiCommandProposal>,
    pub command_history: Vec<String>,
    pub history_cursor: Option<usize>,
    pub comparison: Option<(Uuid, Uuid)>,
    pub table_filters: HashMap<Uuid, String>,
}

impl TerminalTab {
    pub fn new(title: String, shell_idx: usize, cwd: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            title,
            active_shell_idx: shell_idx,
            cwd,
            blocks: Vec::new(),
            input_line: String::new(),
            ai_proposal: None,
            command_history: Vec::new(),
            history_cursor: None,
            comparison: None,
            table_filters: HashMap::new(),
        }
    }
}

pub enum AsyncWorkerEvent {
    BlockCompleted {
        tab_id: Uuid,
        block: CommandBlock,
        updated_cwd: Option<PathBuf>,
    },
    AiCommandReady {
        tab_id: Uuid,
        proposal: AiCommandProposal,
    },
    ExternalMcpConnected(mrt_mcp::McpServerStatus),
}

pub struct PendingSafetyApproval {
    pub tab_id: Uuid,
    pub assessment: SafetyAssessment,
    pub shell: ShellProfile,
    pub cwd: PathBuf,
}

pub struct MrtGuiApp {
    pub runtime: tokio::runtime::Runtime,
    pub shells: Vec<ShellProfile>,
    pub tabs: Vec<TerminalTab>,
    pub active_tab: usize,
    pub side_state: SidePanelState,
    pub mcp_host: McpHostManager,
    pub tx: Sender<AsyncWorkerEvent>,
    pub rx: Receiver<AsyncWorkerEvent>,
    pub pending_safety: Option<PendingSafetyApproval>,
    pub show_command_palette: bool,
    pub palette_query: String,
    pub busy_count: usize,
    pub focused_on_start: bool,
    history: Option<crate::history::HistoryWorker>,
    history_error: Option<String>,
    history_pending: usize,
    show_history: bool,
    history_query: String,
    history_results: Vec<CommandBlock>,
}

impl MrtGuiApp {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime initialization");
        let shells = discover_shells();
        let initial_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let mcp_state = MrtMcpServerState::default();
        let mcp_host = McpHostManager::new(mcp_state);

        let first_shell = &shells[0];
        let first_tab = TerminalTab::new(
            format!("Tab 1 ({})", first_shell.kind.short_label()),
            0,
            initial_cwd,
        );

        let (tx, rx) = channel();
        // Unit tests use isolated stores and never read or write the user's history.
        let history_result = if cfg!(test) {
            Ok(None)
        } else {
            directories::ProjectDirs::from("", "", "MRT")
                .ok_or_else(|| anyhow::anyhow!("Unable to locate the application data directory"))
                .and_then(|dirs| Ok(Some(crate::history::HistoryWorker::start(dirs.data_local_dir().join("history.sqlite")))))
        };
        let (history, history_error) = match history_result {
            Ok(store) => (store, None),
            Err(error) => (None, Some(error.to_string())),
        };

        Self {
            runtime,
            shells,
            tabs: vec![first_tab],
            active_tab: 0,
            side_state: SidePanelState::default(),
            mcp_host,
            tx,
            rx,
            pending_safety: None,
            show_command_palette: false,
            palette_query: String::new(),
            busy_count: 0,
            focused_on_start: false,
            history,
            history_error,
            history_pending: 0,
            show_history: false,
            history_query: String::new(),
            history_results: Vec::new(),
        }
    }

    fn refresh_history(&mut self) {
        if let Some(worker) = &self.history {
            if worker.send(crate::history::HistoryRequest::Search(self.history_query.clone())) {
                self.history_pending += 1;
            }
        }
    }

    fn process_history_events(&mut self) {
        let mut refresh = false;
        if let Some(worker) = &self.history {
            while let Ok(response) = worker.responses.try_recv() {
                self.history_pending = self.history_pending.saturating_sub(1);
                match response {
                    crate::history::HistoryResponse::Saved(Ok(())) => refresh |= self.show_history,
                    crate::history::HistoryResponse::Saved(Err(error)) => self.history_error = Some(error),
                    crate::history::HistoryResponse::Loaded(query, result) => {
                        if query == self.history_query {
                            match result {
                                Ok(blocks) => self.history_results = blocks,
                                Err(error) => self.history_error = Some(error),
                            }
                        }
                    }
                }
            }
        }
        if refresh { self.refresh_history(); }
    }

    fn process_async_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.busy_count = self.busy_count.saturating_sub(1);
            match ev {
                AsyncWorkerEvent::BlockCompleted {
                    tab_id,
                    block,
                    updated_cwd,
                } => {
                    if let Some(worker) = &self.history {
                        if worker.send(crate::history::HistoryRequest::Save(block.clone())) {
                            self.history_pending += 1;
                        }
                    }
                    // Also mirror block to local MCP server state so external MCP clients see live history
                    if let Ok(mut guard) = self.mcp_host.local_server_state.blocks.lock() {
                        guard.push(block.clone());
                    }
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(new_cwd) = updated_cwd {
                            tab.cwd = new_cwd.clone();
                            if let Ok(mut guard) = self.mcp_host.local_server_state.cwd.lock() {
                                *guard = new_cwd;
                            }
                        }
                        tab.blocks.push(block);
                    }
                }
                AsyncWorkerEvent::AiCommandReady { tab_id, proposal } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if tab.input_line.is_empty() {
                            tab.input_line = proposal.proposed_command.clone();
                        }
                        tab.ai_proposal = Some(proposal);
                    }
                }
                AsyncWorkerEvent::ExternalMcpConnected(status) => {
                    self.mcp_host
                        .servers
                        .retain(|s| s.config.name != status.config.name);
                    self.mcp_host.servers.push(status);
                }
            }
        }
    }

    fn submit_input(&mut self, command_override: Option<String>, bypass_safety: bool) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let raw_input = command_override
            .unwrap_or_else(|| std::mem::take(&mut tab.input_line))
            .trim()
            .to_string();

        if raw_input.is_empty() {
            return;
        }

        tab.command_history.push(raw_input.clone());
        tab.history_cursor = None;

        let tab_id = tab.id;
        let cwd = tab.cwd.clone();
        let shell = self
            .shells
            .get(tab.active_shell_idx)
            .cloned()
            .unwrap_or_else(|| self.shells[0].clone());

        // 1. Check built-in AI translation prefix (`#` or `?`)
        if raw_input.starts_with('#') || raw_input.starts_with('?') {
            let tx = self.tx.clone();
            let cwd_str = cwd.display().to_string();
            let tool_names: Vec<String> = self
                .mcp_host
                .all_tools()
                .into_iter()
                .map(|(_, t)| t.name)
                .collect();
            self.busy_count += 1;
            self.runtime.spawn(async move {
                if let Ok(prop) =
                    translate_nl_to_command(&raw_input, &shell, &cwd_str, &tool_names).await
                {
                    let _ = tx.send(AsyncWorkerEvent::AiCommandReady {
                        tab_id,
                        proposal: prop,
                    });
                }
            });
            return;
        }

        // 2. Check built-in Internet & Network tools (`:http`, `:curl`, `:web`, `:ports`, `:clear`)
        if raw_input.eq_ignore_ascii_case(":clear") || raw_input.eq_ignore_ascii_case("cls") {
            tab.blocks.clear();
            tab.comparison = None;
            return;
        }

        if raw_input.starts_with(":ports") {
            self.spawn_port_scanner(tab_id, cwd);
            return;
        }

        if raw_input.starts_with(":http") || raw_input.starts_with(":curl") {
            self.spawn_http_command(tab_id, cwd, raw_input);
            return;
        }

        if raw_input.starts_with(":web") {
            self.spawn_web_command(tab_id, cwd, raw_input);
            return;
        }

        // 3. Check Safety Gate before running shell commands
        if !bypass_safety {
            let assessment = assess_command(&raw_input);
            if assessment.requires_approval || assessment.risk == RiskLevel::Critical {
                self.pending_safety = Some(PendingSafetyApproval {
                    tab_id,
                    assessment,
                    shell,
                    cwd,
                });
                return;
            }
        }

        self.spawn_shell_command(tab_id, shell, cwd, raw_input);
    }

    fn approve_pending_command(&mut self) {
        if let Some(pending) = self.pending_safety.take() {
            if self.tabs.iter().any(|tab| tab.id == pending.tab_id) {
                self.spawn_shell_command(pending.tab_id, pending.shell, pending.cwd, pending.assessment.command);
            }
        }
    }

    fn spawn_shell_command(&mut self, tab_id: Uuid, shell: ShellProfile, cwd: PathBuf, raw_input: String) {
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let cwd_str = cwd.display().to_string();
            match execute_command_in_shell(&shell, &raw_input, &cwd).await {
                Ok(res) => {
                    let block = CommandBlock::new_completed(
                        &raw_input,
                        shell.kind.short_label(),
                        cwd_str,
                        res.combined_output,
                        res.exit_code,
                        res.duration_ms,
                    );
                    let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                        tab_id,
                        block,
                        updated_cwd: res.updated_cwd,
                    });
                }
                Err(e) => {
                    let block = CommandBlock::new_completed(
                        &raw_input,
                        shell.kind.short_label(),
                        cwd_str,
                        format!("Error: {e}"),
                        1,
                        0,
                    );
                    let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                        tab_id,
                        block,
                        updated_cwd: None,
                    });
                }
            }
        });
    }

    fn spawn_port_scanner(&mut self, tab_id: Uuid, cwd: PathBuf) {
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = std::time::Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match inspect_listening_ports().await {
                Ok((structured, csv_raw)) => CommandBlock::new_structured(
                    ":ports",
                    "mrt-net",
                    cwd_str,
                    csv_raw,
                    structured,
                    0,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => CommandBlock::new_completed(
                    ":ports",
                    "mrt-net",
                    cwd_str,
                    format!("Port scan failed: {e}"),
                    1,
                    start.elapsed().as_millis() as u64,
                ),
            };
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                tab_id,
                block,
                updated_cwd: None,
            });
        });
    }

    fn spawn_http_command(&mut self, tab_id: Uuid, cwd: PathBuf, cmd_str: String) {
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let cwd_str = cwd.display().to_string();
            let block = match parse_http_command(&cmd_str) {
                Ok(spec) => match execute_http_request(&spec).await {
                    Ok(summary) => {
                        let code = if summary.status_code < 400 { 0 } else { 1 };
                        let dur = summary.duration_ms;
                        let raw = summary.body_raw.clone();
                        CommandBlock::new_structured(
                            &cmd_str,
                            "mrt-http",
                            cwd_str,
                            raw,
                            StructuredOutput::HttpResult(summary),
                            code,
                            dur,
                        )
                    }
                    Err(e) => CommandBlock::new_completed(
                        &cmd_str,
                        "mrt-http",
                        cwd_str,
                        format!("HTTP Request Error: {e}"),
                        1,
                        0,
                    ),
                },
                Err(e) => CommandBlock::new_completed(
                    &cmd_str,
                    "mrt-http",
                    cwd_str,
                    format!("{e}"),
                    1,
                    0,
                ),
            };
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                tab_id,
                block,
                updated_cwd: None,
            });
        });
    }

    fn spawn_web_command(&mut self, tab_id: Uuid, cwd: PathBuf, cmd_str: String) {
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = std::time::Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match fetch_url_as_markdown(&cmd_str).await {
                Ok(md) => CommandBlock::new_structured(
                    &cmd_str,
                    "mrt-web",
                    cwd_str,
                    md.clone(),
                    StructuredOutput::Markdown(md),
                    0,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => CommandBlock::new_completed(
                    &cmd_str,
                    "mrt-web",
                    cwd_str,
                    format!("Web fetch failed: {e}"),
                    1,
                    start.elapsed().as_millis() as u64,
                ),
            };
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                tab_id,
                block,
                updated_cwd: None,
            });
        });
    }

    fn handle_side_panel_event(&mut self, event: SidePanelEvent) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let tab_id = tab.id;
        let cwd = tab.cwd.clone();

        match event {
            SidePanelEvent::InvokeMcpTool {
                server_name,
                tool_name,
                args_json,
            } => {
                let tx = self.tx.clone();
                let host = self.mcp_host.clone();
                let custom_input = self.side_state.mcp_tool_args_input.trim().to_string();
                let effective_args_str = if custom_input != "{}" && !custom_input.is_empty() {
                    custom_input
                } else {
                    args_json
                };
                let parsed_args: serde_json::Value =
                    serde_json::from_str(&effective_args_str).unwrap_or_else(|_| json!({}));

                self.busy_count += 1;
                self.runtime.spawn(async move {
                    let start = std::time::Instant::now();
                    let cmd_display = format!(":mcp {server_name}/{tool_name} {effective_args_str}");
                    let cwd_str = cwd.display().to_string();
                    let block = match host.call_tool(&server_name, &tool_name, parsed_args).await {
                        Ok(res) => {
                            let text = res
                                .content
                                .first()
                                .map(|c| c.text.clone())
                                .unwrap_or_default();
                            CommandBlock::new_completed(
                                cmd_display,
                                "mcp-tool",
                                cwd_str,
                                text,
                                if res.is_error { 1 } else { 0 },
                                start.elapsed().as_millis() as u64,
                            )
                        }
                        Err(e) => CommandBlock::new_completed(
                            cmd_display,
                            "mcp-tool",
                            cwd_str,
                            format!("MCP Tool Error: {e}"),
                            1,
                            start.elapsed().as_millis() as u64,
                        ),
                    };
                    let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                        tab_id,
                        block,
                        updated_cwd: None,
                    });
                });
            }
            SidePanelEvent::ConnectExternalMcp {
                name,
                command,
                args,
            } => {
                let tx = self.tx.clone();
                let mut host = self.mcp_host.clone();
                let arg_list: Vec<String> =
                    args.split_whitespace().map(|s| s.to_string()).collect();
                self.busy_count += 1;
                self.runtime.spawn(async move {
                    if let Ok(status) = host
                        .connect_external_stdio_server(name, command, arg_list)
                        .await
                    {
                        let _ = tx.send(AsyncWorkerEvent::ExternalMcpConnected(status));
                    }
                });
            }
            SidePanelEvent::RunHttpInspector { method, url, body } => {
                let cmd = if body.trim().is_empty() || method == "GET" {
                    format!(":http {method} {url}")
                } else {
                    format!(":http {method} {url} {}", body.trim())
                };
                self.spawn_http_command(tab_id, cwd, cmd);
            }
            SidePanelEvent::RunWebReader { url } => {
                self.spawn_web_command(tab_id, cwd, format!(":web {url}"));
            }
            SidePanelEvent::RunPortScanner => {
                self.spawn_port_scanner(tab_id, cwd);
            }
        }
    }
}

impl eframe::App for MrtGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let first_frame = !self.focused_on_start;
        let mut focus_prompt = first_frame;
        let palette_was_open = self.show_command_palette;
        let previous_tab = self.tabs[self.active_tab].id;
        if first_frame {
            self.focused_on_start = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::Normal,
            ));
        }

        self.process_async_events();
        self.process_history_events();

        if self.busy_count > 0 || self.history_pending > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        // Keyboard shortcut for Command Palette (Ctrl+K or Ctrl+P)
        if ctx.input_mut(|i| {
            i.consume_key(Modifiers::CTRL, Key::K) || i.consume_key(Modifiers::CTRL, Key::P)
        }) {
            self.show_command_palette = !self.show_command_palette;
        }

        // Top Navigation Bar: Tabs, Shell Switcher, and Side Panel Toggles
        egui::TopBottomPanel::top("mrt_top_bar")
            .frame(egui::Frame::none().fill(Color32::from_rgb(27, 33, 40)).inner_margin(egui::Margin::symmetric(14.0, 9.0)))
            .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("MRT")
                        .size(16.0)
                        .strong()
                        .color(Color32::from_rgb(120, 200, 255)),
                );
                ui.separator();

                // Render Tabs
                let mut tab_to_close = None;
                for idx in 0..self.tabs.len() {
                    let is_active = idx == self.active_tab;
                    let title = self.tabs[idx].title.clone();
                    let btn = egui::Button::new(
                        RichText::new(&title).color(if is_active {
                            Color32::from_rgb(140, 220, 255)
                        } else {
                            Color32::GRAY
                        }),
                    )
                    .fill(if is_active {
                        Color32::from_rgb(35, 45, 62)
                    } else {
                        Color32::TRANSPARENT
                    });

                    if ui.add(btn).clicked() {
                        self.active_tab = idx;
                    }
                    if self.tabs.len() > 1 && ui.small_button("×").clicked() {
                        tab_to_close = Some(idx);
                    }
                }
                if let Some(close_idx) = tab_to_close {
                    let closed_id = self.tabs.remove(close_idx).id;
                    if self.pending_safety.as_ref().is_some_and(|pending| pending.tab_id == closed_id) {
                        self.pending_safety = None;
                    }
                    if self.active_tab >= self.tabs.len() {
                        self.active_tab = self.tabs.len().saturating_sub(1);
                    }
                }

                // New Tab dropdown per discovered shell
                egui::menu::menu_button(ui, "+ New tab", |ui| {
                    for (s_idx, shell) in self.shells.iter().enumerate() {
                        if ui
                            .button(format!("{} ({})", shell.name, shell.program))
                            .clicked()
                        {
                            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                            let n = self.tabs.len() + 1;
                            self.tabs.push(TerminalTab::new(
                                format!("Tab {n} ({})", shell.kind.short_label()),
                                s_idx,
                                cwd,
                            ));
                            self.active_tab = self.tabs.len() - 1;
                            ui.close_menu();
                        }
                    }
                });

                ui.separator();

                // Active Tab Shell Profile Switcher (PowerShell, CMD/Batch, Bash/Git Bash, WSL, etc.)
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    let current_shell_name = self
                        .shells
                        .get(tab.active_shell_idx)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| "Shell".into());

                    ui.label(RichText::new("Shell").size(12.0).color(Color32::GRAY));
                    egui::ComboBox::from_id_salt("active_shell_selector")
                        .selected_text(RichText::new(current_shell_name).strong())
                        .show_ui(ui, |ui| {
                            for (idx, s) in self.shells.iter().enumerate() {
                                if ui
                                    .selectable_value(
                                        &mut tab.active_shell_idx,
                                        idx,
                                        format!("{} — {}", s.name, s.program),
                                    )
                                    .changed()
                                {
                                    tab.title = format!(
                                        "Tab {} ({})",
                                        self.active_tab + 1,
                                        s.kind.short_label()
                                    );
                                }
                            }
                        });
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.toggle_value(&mut self.show_history, "History").changed() && self.show_history {
                        self.refresh_history();
                    }
                    ui.toggle_value(&mut self.side_state.show_mcp_panel, "MCP Hub");
                    ui.toggle_value(&mut self.side_state.show_internet_panel, "Internet Studio");
                    if ui.button("Palette  Ctrl+K").clicked() {
                        self.show_command_palette = !self.show_command_palette;
                    }
                });
            });
        });

        if self.show_history {
            let mut restore = None;
            egui::SidePanel::right("history_panel").default_width(360.0).resizable(true).show(ctx, |ui| {
                ui.heading("Command history");
                if ui.add(egui::TextEdit::singleline(&mut self.history_query).hint_text("Search commands and output")).on_hover_text("Search commands and output").changed() {
                    self.refresh_history();
                }
                ui.label("Latest 200 matching commands");
                if self.history_pending > 0 { ui.spinner(); }
                if self.history_results.is_empty() && self.history_pending == 0 {
                    ui.label("No saved commands match.");
                }
                if let Some(error) = &self.history_error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for block in &self.history_results {
                        ui.group(|ui| {
                            ui.monospace(&block.command);
                            ui.small(format!("{} · {} · {}", block.timestamp.format("%Y-%m-%d %H:%M"), block.shell_name, block.cwd));
                            if ui.button("View output").clicked() { restore = Some(block.clone()); }
                        });
                    }
                });
            });
            if let Some(block) = restore {
                let shell_idx = self.shells.iter().position(|shell| shell.kind.short_label() == block.shell_name).unwrap_or(0);
                let mut tab = TerminalTab::new("History".into(), shell_idx, PathBuf::from(&block.cwd));
                tab.command_history.push(block.command.clone());
                tab.blocks.push(block);
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }

        // Right-hand Side Panels (MCP Hub & Internet Studio)
        let mut panel_event = None;
        if self.side_state.show_mcp_panel {
            egui::SidePanel::right("mrt_mcp_panel")
                .default_width(330.0)
                .resizable(true)
                .show(ctx, |ui| {
                    if let Some(ev) =
                        render_mcp_side_panel(ui, &mut self.side_state, &self.mcp_host)
                    {
                        panel_event = Some(ev);
                    }
                });
        }
        if self.side_state.show_internet_panel {
            egui::SidePanel::right("mrt_internet_panel")
                .default_width(330.0)
                .resizable(true)
                .show(ctx, |ui| {
                    if let Some(ev) = render_internet_side_panel(ui, &mut self.side_state) {
                        panel_event = Some(ev);
                    }
                });
        }
        if let Some(ev) = panel_event {
            self.handle_side_panel_event(ev);
        }

        focus_prompt |= previous_tab != self.tabs[self.active_tab].id || (palette_was_open && !self.show_command_palette);

        // Bottom Smart Prompt Bar
        egui::TopBottomPanel::bottom("mrt_bottom_prompt")
            .frame(egui::Frame::none().fill(Color32::from_rgb(29, 37, 45)).inner_margin(egui::Margin::symmetric(18.0, 12.0)))
            .show(ctx, |ui| {
            if !self.show_history {
                if let Some(error) = &self.history_error {
                    ui.colored_label(Color32::LIGHT_RED, error);
                }
            }
            // Optional AI Command Proposal Banner
            let mut run_ai_cmd = None;
            let mut dismiss_ai = false;
            let tab = &mut self.tabs[self.active_tab];
            if let Some(proposal) = &tab.ai_proposal {
                let cmd = &proposal.proposed_command;
                let explanation = &proposal.explanation;
                let provider = &proposal.provider_used;
                egui::Frame::none()
                    .fill(Color32::from_rgb(30, 46, 68))
                    .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(95, 165, 245)))
                    .rounding(egui::Rounding::same(5.0))
                    .inner_margin(egui::Margin::same(8.0))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("✨ AI ({provider}):"))
                                    .strong()
                                    .color(Color32::from_rgb(135, 210, 255)),
                            );
                            ui.label(
                                RichText::new(format!("`{cmd}`"))
                                    .monospace()
                                    .strong()
                                    .color(Color32::WHITE),
                            );
                            ui.label(
                                RichText::new(format!("— {explanation}"))
                                    .color(Color32::from_rgb(195, 210, 230)),
                            );
                            if ui.small_button("▶ Execute Now").clicked() {
                                run_ai_cmd = Some(cmd.clone());
                                dismiss_ai = true;
                            }
                            if ui.small_button("Dismiss").clicked() {
                                dismiss_ai = true;
                            }
                        });
                    });
                ui.add_space(4.0);
            }
            if dismiss_ai {
                tab.ai_proposal = None;
            }
            if let Some(c) = run_ai_cmd {
                self.submit_input(Some(c), false);
            }

            let mut should_submit = false;
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                ui.add(egui::Label::new(RichText::new(tab.cwd.display().to_string())
                    .monospace().color(Color32::from_rgb(175, 189, 202))).truncate())
                    .on_hover_text(tab.cwd.display().to_string());
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(">").size(21.0).color(Color32::from_rgb(132, 224, 188)));
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut tab.input_line)
                            .id(egui::Id::new(("command_input", tab.id)))
                            .font(egui::FontId::monospace(18.0))
                            .margin(egui::Margin::symmetric(10.0, 8.0))
                            .hint_text(if tab.blocks.is_empty() {
                                "Type a command. Press Ctrl+K for tools."
                            } else {
                                "Type a command"
                            })
                            .desired_width((ui.available_width() - 84.0).max(32.0)),
                    );
                    if focus_prompt && !self.show_command_palette && self.pending_safety.is_none() {
                        response.request_focus();
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        should_submit = true;
                        response.request_focus();
                    }

                    // Up/Down history navigation
                    if response.has_focus() {
                        if ui.input(|i| i.key_pressed(Key::ArrowUp)) && !tab.command_history.is_empty()
                        {
                            let next_idx = match tab.history_cursor {
                                Some(c) => c.saturating_sub(1),
                                None => tab.command_history.len() - 1,
                            };
                            tab.history_cursor = Some(next_idx);
                            tab.input_line = tab.command_history[next_idx].clone();
                        } else if ui.input(|i| i.key_pressed(Key::ArrowDown))
                            && !tab.command_history.is_empty()
                        {
                            if let Some(c) = tab.history_cursor {
                                if c + 1 < tab.command_history.len() {
                                    tab.history_cursor = Some(c + 1);
                                    tab.input_line = tab.command_history[c + 1].clone();
                                } else {
                                    tab.history_cursor = None;
                                    tab.input_line.clear();
                                }
                            }
                        }
                    }

                    if ui.add_enabled(!tab.input_line.trim().is_empty(), egui::Button::new("Run").min_size(egui::vec2(52.0, 36.0))).clicked() {
                        response.request_focus();
                        should_submit = true;
                    }
                });
            }

            if should_submit {
                self.submit_input(None, false);
            }
        });

        // Central Terminal Block Stream
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::from_rgb(19, 24, 30)).inner_margin(egui::Margin::same(14.0))).show(ctx, |ui| {
            let mut pending_block_action = None;

            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let mut previous_runs = HashMap::new();
                        for block in &mut tab.blocks {
                            let key = (block.command.clone(), block.shell_name.clone(), block.cwd.clone());
                            let previous_id = previous_runs.insert(key, block.id);
                            let filter = tab.table_filters.entry(block.id).or_default();
                            if let Some(action) = render_command_block(ui, block, filter, previous_id) {
                                pending_block_action = Some(action);
                            }
                            ui.add_space(8.0);
                        }
                        if let Some((previous_id, current_id)) = tab.comparison {
                            let previous = tab.blocks.iter().find(|block| block.id == previous_id);
                            let current = tab.blocks.iter().find(|block| block.id == current_id);
                            if let (Some(previous), Some(current)) = (previous, current) {
                                if crate::widgets::block_view::render_comparison(ui, previous, current) {
                                    tab.comparison = None;
                                }
                            }
                        }
                    });
            }

            if let Some(action) = pending_block_action {
                match action {
                    BlockAction::Compare(previous, current) => {
                        self.tabs[self.active_tab].comparison = Some((previous, current));
                    }
                    BlockAction::Rerun(cmd) => {
                        self.submit_input(Some(cmd), false);
                    }
                    BlockAction::ApplySuggestedFix(fix_cmd) => {
                        self.submit_input(Some(fix_cmd), false);
                    }
                    BlockAction::DiagnoseError(block_id) => {
                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                            let shell = self
                                .shells
                                .get(tab.active_shell_idx)
                                .cloned()
                                .unwrap_or_else(|| self.shells[0].clone());
                            if let Some(blk) = tab.blocks.iter_mut().find(|b| b.id == block_id) {
                                let triage = diagnose_command_error(
                                    &blk.command,
                                    &blk.plain_output,
                                    blk.exit_code.unwrap_or(1),
                                    &shell,
                                );
                                blk.ai_explanation = Some(triage.explanation);
                                blk.ai_suggested_fix = triage.suggested_fix;
                            }
                        }
                    }
                }
            }
        });

        // Safety Gate Modal Window
        if self.pending_safety.is_some() {
            let mut approve = false;
            let mut cancel = false;

            if let Some(ref pending) = self.pending_safety {
                egui::Window::new("🛡 MRT Safety Approval Gate")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new("Destructive Command Intercepted")
                                .size(15.5)
                                .strong()
                                .color(Color32::from_rgb(255, 130, 130)),
                        );
                        ui.add_space(4.0);
                        ui.label(format!(
                            "Risk Classification: {}",
                            pending.assessment.risk.label()
                        ));
                        ui.label(format!("Reason: {}", pending.assessment.reason));
                        ui.label(format!(
                            "Target Shell: {} (Tab {}, in {})",
                            pending.shell.name,
                            pending.tab_id,
                            pending.cwd.display()
                        ));
                        ui.add_space(6.0);
                        egui::Frame::none()
                            .fill(Color32::from_rgb(35, 20, 24))
                            .inner_margin(egui::Margin::same(8.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&pending.assessment.command)
                                        .monospace()
                                        .strong()
                                        .color(Color32::from_rgb(255, 195, 195)),
                                );
                            });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button(
                                    RichText::new("✓ Approve & Execute")
                                        .color(Color32::from_rgb(255, 150, 150)),
                                )
                                .clicked()
                            {
                                approve = true;
                            }
                            if ui.button("✗ Cancel Command").clicked() {
                                cancel = true;
                            }
                        });
                    });
            }

            if approve {
                self.approve_pending_command();
            } else if cancel {
                self.pending_safety = None;
            }
        }

        // Universal Command Palette Modal (Ctrl+K / Ctrl+P)
        if self.show_command_palette {
            let mut close_palette = false;
            let mut cmd_to_run = None;
            let mut switch_shell_idx = None;

            egui::Window::new("Command palette")
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
                .show(ctx, |ui| {
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.palette_query)
                            .hint_text("Search commands and shells")
                            .desired_width(f32::INFINITY),
                    );
                    ui.separator();

                    if !palette_was_open { search.request_focus(); }
                    let q = self.palette_query.trim().to_lowercase();
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        ui.label(RichText::new("Switch Active Shell Profile").size(11.5).color(Color32::GRAY));
                        for (idx, s) in self.shells.iter().enumerate() {
                            let label = format!("🐚 Switch Shell: {} ({})", s.name, s.program);
                            if q.is_empty() || label.to_lowercase().contains(&q) {
                                if ui.button(label).clicked() {
                                    switch_shell_idx = Some(idx);
                                    close_palette = true;
                                }
                            }
                        }

                        ui.add_space(4.0);
                        ui.label(RichText::new("Built-in Internet & AI Tools").size(11.5).color(Color32::GRAY));
                        for (title, cmd) in [
                            ("🔍 Scan Active Listening TCP/UDP Ports", ":ports"),
                            ("🌐 HTTP GET Sample JSON API", ":http GET https://httpbin.org/json"),
                            ("📖 Fetch Rust Homepage as Markdown", ":web https://www.rust-lang.org"),
                            ("🤖 AI: Show System Info in Active Shell", "# show system info"),
                            ("Read example.com", ":web https://example.com"),
                            ("AI: List files", "# list all files in directory"),
                            ("AI: Show running processes", "# show running processes"),
                            ("Test Safety Gate", "rm -rf ./important_production_data"),
                            ("Clear terminal blocks", ":clear"),
                        ] {
                            if q.is_empty() || title.to_lowercase().contains(&q) || cmd.to_lowercase().contains(&q) {
                                if ui.button(title).clicked() {
                                    cmd_to_run = Some(cmd.to_string());
                                    close_palette = true;
                                }
                            }
                        }
                    });

                    ui.separator();
                    if ui.small_button("Close (Esc)").clicked() {
                        close_palette = true;
                    }
                });

            if let Some(s_idx) = switch_shell_idx {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.active_shell_idx = s_idx;
                    tab.title = format!(
                        "Tab {} ({})",
                        self.active_tab + 1,
                        self.shells[s_idx].kind.short_label()
                    );
                }
            }
            if let Some(cmd) = cmd_to_run {
                self.submit_input(Some(cmd), false);
            }
            if close_palette || ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_command_palette = false;
                if self.pending_safety.is_none() {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new(("command_input", self.tabs[self.active_tab].id))));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_entry_creates_comparable_output_blocks() {
        let mut app = MrtGuiApp::new();
        for value in ["first", "second"] {
            let command = format!("echo {value}");
            app.tabs[0].input_line = command;
            app.submit_input(None, false);
            let event = app.rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
            app.tx.send(event).unwrap();
            app.process_async_events();
        }
        assert_eq!(app.tabs[0].blocks.len(), 2);
        assert!(app.tabs[0].blocks[0].plain_output.contains("first"));
        assert!(app.tabs[0].blocks[1].plain_output.contains("second"));
        let context = egui::Context::default();
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let blocks = &app.tabs[0].blocks;
                assert!(!crate::widgets::block_view::render_comparison(ui, &blocks[0], &blocks[1]));
                assert!(!crate::widgets::block_view::render_comparison(ui, &blocks[0], &blocks[0]));
            });
        });
    }

    #[test]
    fn approval_preserves_captured_context_after_switching_tabs() {
        let mut app = MrtGuiApp::new();
        let tab_id = app.tabs[0].id;
        let cwd = app.tabs[0].cwd.clone();
        let shell = app.shells[0].clone();
        app.pending_safety = Some(PendingSafetyApproval {
            tab_id,
            assessment: assess_command("echo approval-test"),
            shell: shell.clone(),
            cwd: cwd.clone(),
        });
        app.tabs.push(TerminalTab::new("Other".into(), 0, cwd.join("missing-directory")));
        app.active_tab = 1;
        app.tabs[0].cwd = cwd.join("changed-directory");
        app.approve_pending_command();
        let event = app.rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
        match event {
            AsyncWorkerEvent::BlockCompleted { tab_id: result_id, block, .. } => {
                assert_eq!(result_id, tab_id);
                assert_eq!(block.cwd, cwd.display().to_string());
                assert_eq!(block.shell_name, shell.kind.short_label());
                assert_eq!(block.exit_code, Some(0));
                assert!(block.plain_output.contains("approval-test"));
            }
            _ => panic!("Expected command completion"),
        }
    }

    #[test]
    fn approval_for_a_closed_tab_does_not_execute() {
        let mut app = MrtGuiApp::new();
        app.pending_safety = Some(PendingSafetyApproval {
            tab_id: Uuid::new_v4(),
            assessment: assess_command("echo approval-test"),
            shell: app.shells[0].clone(),
            cwd: app.tabs[0].cwd.clone(),
        });
        app.approve_pending_command();
        assert_eq!(app.busy_count, 0);
        assert!(app.pending_safety.is_none());
        assert!(app.rx.try_recv().is_err());
    }

    fn deliver_proposal(app: &mut MrtGuiApp, tab_id: Uuid, command: &str) {
        let proposal = AiCommandProposal {
            query: String::new(),
            target_shell: app.shells[0].name.clone(),
            proposed_command: command.into(),
            explanation: String::new(),
            provider_used: "test".into(),
        };
        app.tx
            .send(AsyncWorkerEvent::AiCommandReady { tab_id, proposal })
            .unwrap();
        app.process_async_events();
    }

    #[test]
    fn delayed_ai_replies_follow_the_requesting_tab() {
        let mut app = MrtGuiApp::new();
        let first_id = app.tabs[0].id;
        app.tabs[0].input_line = "echo my draft".into();
        deliver_proposal(&mut app, first_id, "echo suggestion");
        assert_eq!(app.tabs[0].input_line, "echo my draft");
        assert_eq!(app.tabs[0].ai_proposal.as_ref().unwrap().proposed_command, "echo suggestion");
        app.tabs[0].input_line.clear();
        let second_dir = app.tabs[0].cwd.join("second-project");
        app.tabs.push(TerminalTab::new(
            "Second".into(),
            0,
            second_dir,
        ));
        app.active_tab = 1;
        let second_id = app.tabs[1].id;
        deliver_proposal(&mut app, second_id, "echo second");
        deliver_proposal(&mut app, first_id, "echo first");

        let active = &app.tabs[app.active_tab];
        assert_eq!(active.input_line, "echo second");
        assert_eq!(
            active.ai_proposal.as_ref().unwrap().proposed_command,
            "echo second"
        );
        assert_eq!(app.tabs[0].input_line, "echo first");
        assert_eq!(
            app.tabs[0].ai_proposal.as_ref().unwrap().proposed_command,
            "echo first"
        );

        app.tabs.remove(0);
        app.active_tab = 0;
        deliver_proposal(&mut app, first_id, "echo late");
        let remaining = &app.tabs[app.active_tab];
        assert_eq!(remaining.input_line, "echo second");
        assert_eq!(
            remaining.ai_proposal.as_ref().unwrap().proposed_command,
            "echo second"
        );
    }
}
