use crate::theme::{apply_mrt_theme, Palette};
use crate::widgets::block_view::{render_command_block, BlockAction};
use crate::widgets::runbooks::{
    render_runbooks_panel, ReplayStatusView, RunbookAction, RunbooksUiState,
};
use crate::widgets::side_panels::{
    render_internet_side_panel, render_mcp_side_panel, SidePanelEvent, SidePanelState,
};
use egui::{Color32, Key, Modifiers, RichText};
use mrt_ai::{diagnose_command_error, translate_nl_to_command, AiCommandProposal};
use mrt_core::{
    assess_command, discover_shells, execute_command_streaming, CommandBlock, RiskLevel,
    SafetyAssessment, ShellProfile, StructuredOutput,
};
use mrt_mcp::{
    start_gui_bridge_listener, GuiUiCommand, McpHostManager, McpTabSession, MrtMcpServerState,
};
use mrt_tools::{
    execute_http_request, fetch_url_as_markdown, inspect_dns, inspect_listening_ports,
    parse_http_command,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use uuid::Uuid;

#[derive(Clone)]
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
    ExplorerLoaded {
        path: PathBuf,
        entries: Result<Vec<(PathBuf, bool)>, String>,
    },
    ExternalMcpConnected(mrt_mcp::McpServerStatus),
}

pub struct PendingSafetyApproval {
    pub tab_id: Uuid,
    pub assessment: SafetyAssessment,
    pub shell: ShellProfile,
    pub cwd: PathBuf,
}

pub struct ActiveRunbookReplay {
    pub runbook_name: String,
    pub tab_id: Uuid,
    pub steps: Vec<crate::history::RunbookStep>,
    pub next_step_idx: usize,
    pub waiting_block_id: Option<Uuid>,
}

pub struct MrtGuiApp {
    pub runtime: tokio::runtime::Runtime,
    pub shells: Vec<ShellProfile>,
    pub tabs: Vec<TerminalTab>,
    pub active_tab: usize,
    pub side_state: SidePanelState,
    pub mcp_host: McpHostManager,
    pub gui_cmd_rx: Receiver<GuiUiCommand>,
    pub bridge_port: Option<u16>,
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
    runbooks_state: RunbooksUiState,
    active_replay: Option<ActiveRunbookReplay>,
    running_output: HashMap<Uuid, (Uuid, std::sync::Arc<std::sync::Mutex<String>>)>,
    mosaic: bool,
    mosaic_columns: usize,
    show_explorer: bool,
    explorer_path: Option<PathBuf>,
    explorer_entries: Result<Vec<(PathBuf, bool)>, String>,
}

impl MrtGuiApp {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime initialization");
        let shells = discover_shells();
        let initial_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let (gui_cmd_tx, gui_cmd_rx) = channel();
        let mut mcp_state = MrtMcpServerState::default();
        mcp_state.gui_tx = Some(gui_cmd_tx);

        let first_shell = &shells[0];
        let first_tab = TerminalTab::new(
            format!("Tab 1 ({})", first_shell.kind.short_label()),
            0,
            initial_cwd,
        );
        if let Ok(mut tabs_guard) = mcp_state.tabs.lock() {
            *tabs_guard = vec![McpTabSession {
                id: first_tab.id,
                title: first_tab.title.clone(),
                shell_id: first_shell.id.clone(),
                shell_name: first_shell.kind.short_label().to_string(),
                cwd: first_tab.cwd.clone(),
                blocks: Vec::new(),
            }];
        }
        if let Ok(mut active_guard) = mcp_state.active_tab_id.lock() {
            *active_guard = first_tab.id;
        }

        let bridge_state = mcp_state.clone();
        let write_discovery = !cfg!(test);
        let bridge_port = runtime
            .block_on(async move { start_gui_bridge_listener(bridge_state, 0, write_discovery).await })
            .ok()
            .map(|addr| addr.port());

        let mcp_host = McpHostManager::new(mcp_state);

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
            gui_cmd_rx,
            bridge_port,
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
            runbooks_state: RunbooksUiState::default(),
            active_replay: None,
            running_output: HashMap::new(),
            mosaic: false,
            mosaic_columns: 2,
            show_explorer: false,
            explorer_path: None,
            explorer_entries: Ok(Vec::new()),
        }
    }

    fn handle_block_action(&mut self, action: BlockAction) {
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

    fn move_terminal(&mut self, source: Uuid, target: Uuid) {
        let active_id = self.tabs[self.active_tab].id;
        let from = self.tabs.iter().position(|tab| tab.id == source);
        let to = self.tabs.iter().position(|tab| tab.id == target);
        if let (Some(from), Some(to)) = (from, to) {
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            self.active_tab = self
                .tabs
                .iter()
                .position(|tab| tab.id == active_id)
                .unwrap_or(0);
        }
    }

    fn close_terminal(&mut self, id: Uuid) {
        if self.tabs.len() <= 1 {
            return;
        }
        let active_id = self.tabs[self.active_tab].id;
        self.tabs.retain(|tab| tab.id != id);
        self.active_tab = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_id)
            .unwrap_or_else(|| self.active_tab.min(self.tabs.len() - 1));
        if self
            .pending_safety
            .as_ref()
            .is_some_and(|pending| pending.tab_id == id)
        {
            self.pending_safety = None;
        }
        if self
            .active_replay
            .as_ref()
            .is_some_and(|replay| replay.tab_id == id)
        {
            self.stop_runbook_replay(Some("Runbook stopped because its target tab was closed.".into()));
        }
    }

    fn render_mosaic(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Slider::new(&mut self.mosaic_columns, 1..=6).text("Columns"));
            if ui.button("+ Terminal").clicked() {
                let active = &self.tabs[self.active_tab];
                let tab = TerminalTab::new(
                    format!("Terminal {}", self.tabs.len() + 1),
                    active.active_shell_idx,
                    active.cwd.clone(),
                );
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
            ui.weak("Drag a title to reorder");
        });
        ui.add_space(8.0);
        let spacing = 10.0;
        let available = ui.available_width().max(1.0);
        let columns = self
            .mosaic_columns
            .min(((available + spacing) / 250.0).floor().max(1.0) as usize);
        let size = ((available - spacing * (columns - 1) as f32) / columns as f32).max(1.0);
        let mut reorder = None;
        let mut close = None;
        let mut submit = None;
        let mut action = None;
        let active_id = self.tabs[self.active_tab].id;
        egui::ScrollArea::vertical().id_salt("mosaic_grid").show(ui, |ui| {
            for row in (0..self.tabs.len()).step_by(columns) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = spacing;
                    for index in row..(row + columns).min(self.tabs.len()) {
                        let tab = &mut self.tabs[index];
                        let id = tab.id;
                        ui.push_id(id, |ui| {
                            ui.allocate_ui_with_layout(egui::vec2(size, size), egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                                let frame = egui::Frame::none().inner_margin(8.0)
                                    .stroke(egui::Stroke::new(1.0_f32, if id == active_id { Color32::LIGHT_BLUE } else { Color32::DARK_GRAY }))
                                    .rounding(5.0);
                                let (panel, dropped) = ui.dnd_drop_zone::<Uuid, _>(frame, |ui| {
                                    ui.set_width((size - 16.0).max(1.0));
                                    ui.horizontal(|ui| {
                                        ui.dnd_drag_source(egui::Id::new(("terminal_drag", id)), id, |ui| {
                                            let title = ui.add_sized(
                                                [((size - 120.0).max(24.0)), 20.0],
                                                egui::Label::new(RichText::new(&tab.title).strong())
                                                    .truncate().sense(egui::Sense::click()),
                                            );
                                            if title.clicked() { self.active_tab = index; }
                                        });
                                        if ui.small_button("Expand").clicked() {
                                            self.active_tab = index;
                                            self.mosaic = false;
                                        }
                                        if ui.small_button("×").clicked() { close = Some(id); }
                                    });
                                    egui::ComboBox::from_id_salt("shell").selected_text(&self.shells[tab.active_shell_idx].name)
                                        .show_ui(ui, |ui| {
                                            for (shell_index, shell) in self.shells.iter().enumerate() {
                                                ui.selectable_value(&mut tab.active_shell_idx, shell_index, &shell.name);
                                            }
                                        });
                                    ui.add(egui::Label::new(RichText::new(tab.cwd.display().to_string()).small()).truncate());
                                    let output_height = (size - 135.0).max(24.0);
                                    egui::ScrollArea::both().id_salt("output").auto_shrink([false, false])
                                        .max_height(output_height).min_scrolled_height(output_height)
                                        .stick_to_bottom(true).show(ui, |ui| {
                                            let mut previous_runs = HashMap::new();
                                            for block in &mut tab.blocks {
                                                let key = (block.command.clone(), block.shell_name.clone(), block.cwd.clone());
                                                let previous = previous_runs.insert(key, block.id);
                                                let filter = tab.table_filters.entry(block.id).or_default();
                                                if let Some(event) = render_command_block(ui, block, filter, previous) {
                                                    action = Some((index, event));
                                                }
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
                                    ui.horizontal(|ui| {
                                        let response = ui.add(egui::TextEdit::singleline(&mut tab.input_line)
                                            .id(egui::Id::new(("mosaic_input", id))).font(egui::TextStyle::Monospace)
                                            .desired_width((ui.available_width() - 48.0).max(1.0)).hint_text("Command"));
                                        if response.has_focus() || response.clicked() { self.active_tab = index; }
                                        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                            submit = Some(index);
                                            response.request_focus();
                                        }
                                        if ui.add_enabled(!tab.input_line.trim().is_empty(), egui::Button::new("Run")).clicked() {
                                            submit = Some(index);
                                        }
                                    });
                                });
                                if panel.response.clicked() { self.active_tab = index; }
                                if let Some(source) = dropped { reorder = Some((*source, id)); }
                            });
                        });
                    }
                });
                ui.add_space(spacing);
            }
        });
        if let Some(index) = submit {
            self.active_tab = index;
            self.submit_input(None, false);
        }
        if let Some((index, event)) = action {
            self.active_tab = index;
            self.handle_block_action(event);
        }
        if let Some((source, target)) = reorder {
            self.move_terminal(source, target);
        }
        if let Some(id) = close {
            self.close_terminal(id);
        }
    }

    fn refresh_explorer(&mut self, path: PathBuf) {
        self.explorer_path = Some(path.clone());
        self.explorer_entries = Ok(Vec::new());
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn_blocking(move || {
            let entries = read_directory(&path).map_err(|error| error.to_string());
            let _ = tx.send(AsyncWorkerEvent::ExplorerLoaded { path, entries });
        });
    }

    fn render_explorer(&mut self, ctx: &egui::Context) {
        let path = self.tabs[self.active_tab].cwd.clone();
        if self.explorer_path.as_ref() != Some(&path) {
            self.refresh_explorer(path.clone());
        }
        let mut navigate = None;
        let mut refresh = false;
        egui::SidePanel::left("folder_explorer")
            .default_width(240.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Explorer");
                    if ui.small_button("Refresh").clicked() {
                        refresh = true;
                    }
                });
                ui.add(egui::Label::new(path.display().to_string()).wrap());
                if let Some(parent) = path.parent() {
                    if ui.button("↑ Parent folder").clicked() {
                        navigate = Some(parent.to_path_buf());
                    }
                }
                ui.separator();
                match &self.explorer_entries {
                    Err(error) => {
                        ui.colored_label(Color32::LIGHT_RED, error);
                    }
                    Ok(entries) => {
                        egui::ScrollArea::both()
                            .id_salt("explorer_entries")
                            .show(ui, |ui| {
                                for (entry, directory) in entries {
                                    let name =
                                        entry.file_name().unwrap_or_default().to_string_lossy();
                                    if *directory {
                                        if ui.button(format!("▸ {name}/")).clicked() {
                                            navigate = Some(entry.clone());
                                        }
                                    } else {
                                        ui.label(name.as_ref());
                                    }
                                }
                            });
                    }
                }
            });
        if refresh {
            self.refresh_explorer(path);
        }
        if let Some(path) = navigate {
            self.tabs[self.active_tab].cwd = path.clone();
            if let Ok(mut cwd) = self.mcp_host.local_server_state.cwd.lock() {
                *cwd = path;
            }
        }
    }

    fn refresh_history(&mut self) {
        if let Some(worker) = &self.history {
            if worker.send(crate::history::HistoryRequest::Search(self.history_query.clone())) {
                self.history_pending += 1;
            }
        }
    }

    fn refresh_runbooks(&mut self) {
        if let Some(worker) = &self.history {
            if worker.send(crate::history::HistoryRequest::ListRunbooks) {
                self.history_pending += 1;
            }
        }
    }

    fn process_history_events(&mut self) {
        let mut refresh = false;
        let mut refresh_runbooks = false;
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
                    crate::history::HistoryResponse::RunbookSaved(Ok(()))
                    | crate::history::HistoryResponse::RunbookDeleted(Ok(())) => {
                        refresh_runbooks = true;
                    }
                    crate::history::HistoryResponse::RunbookSaved(Err(error))
                    | crate::history::HistoryResponse::RunbookDeleted(Err(error)) => {
                        self.history_error = Some(error);
                    }
                    crate::history::HistoryResponse::RunbooksLoaded(result) => match result {
                        Ok(runbooks) => self.runbooks_state.runbooks = runbooks,
                        Err(error) => self.history_error = Some(error),
                    },
                }
            }
        }
        if refresh {
            self.refresh_history();
        }
        if refresh_runbooks {
            self.refresh_runbooks();
        }
    }

    pub fn sync_mcp_from_gui(&self) {
        let state = &self.mcp_host.local_server_state;
        if let Some(active) = self.tabs.get(self.active_tab) {
            if let Ok(mut guard) = state.active_tab_id.lock() {
                *guard = active.id;
            }
            if let Ok(mut guard) = state.cwd.lock() {
                *guard = active.cwd.clone();
            }
        }
        if let Ok(mut tabs_guard) = state.tabs.lock() {
            *tabs_guard = self
                .tabs
                .iter()
                .map(|t| {
                    let shell = self
                        .shells
                        .get(t.active_shell_idx)
                        .unwrap_or(&self.shells[0]);
                    McpTabSession {
                        id: t.id,
                        title: t.title.clone(),
                        shell_id: shell.id.clone(),
                        shell_name: shell.kind.short_label().to_string(),
                        cwd: t.cwd.clone(),
                        blocks: t.blocks.clone(),
                    }
                })
                .collect();
        }
    }

    pub fn process_mcp_ui_commands(&mut self) {
        let mut pending_submit = None;
        while let Ok(cmd) = self.gui_cmd_rx.try_recv() {
            match cmd {
                GuiUiCommand::SyncFromMcp => {
                    let state = &self.mcp_host.local_server_state;
                    let mcp_tabs = state.tabs.lock().map(|g| g.clone()).unwrap_or_default();
                    let active_id = state.active_tab_id.lock().map(|g| *g).ok();
                    if !mcp_tabs.is_empty() {
                        let mut rebuilt = Vec::with_capacity(mcp_tabs.len());
                        for mt in mcp_tabs {
                            let shell_idx = self
                                .shells
                                .iter()
                                .position(|s| {
                                    s.id.eq_ignore_ascii_case(&mt.shell_id)
                                        || s.kind.short_label().eq_ignore_ascii_case(&mt.shell_name)
                                })
                                .unwrap_or(0);
                            if let Some(mut existing) =
                                self.tabs.iter().find(|t| t.id == mt.id).cloned()
                            {
                                existing.title = mt.title;
                                existing.active_shell_idx = shell_idx;
                                existing.cwd = mt.cwd;
                                existing.blocks = mt.blocks;
                                rebuilt.push(existing);
                            } else {
                                let mut new_tab = TerminalTab::new(mt.title, shell_idx, mt.cwd);
                                new_tab.id = mt.id;
                                new_tab.blocks = mt.blocks;
                                rebuilt.push(new_tab);
                            }
                        }
                        self.tabs = rebuilt;
                        if let Some(aid) = active_id {
                            if let Some(pos) = self.tabs.iter().position(|t| t.id == aid) {
                                self.active_tab = pos;
                            }
                        }
                        if self.active_tab >= self.tabs.len() {
                            self.active_tab = self.tabs.len().saturating_sub(1);
                        }
                    }
                }
                GuiUiCommand::SetInputDraft {
                    tab_id,
                    text,
                    submit,
                } => {
                    let target_idx = tab_id
                        .and_then(|id| self.tabs.iter().position(|t| t.id == id))
                        .unwrap_or(self.active_tab);
                    if let Some(tab) = self.tabs.get_mut(target_idx) {
                        tab.input_line = text.clone();
                        if submit {
                            self.active_tab = target_idx;
                            pending_submit = Some(text);
                        }
                    }
                }
                GuiUiCommand::SetMosaic { enabled, columns } => {
                    self.mosaic = enabled;
                    if let Some(c) = columns {
                        self.mosaic_columns = c.clamp(1, 6);
                    }
                }
                GuiUiCommand::SetExplorer { visible } => {
                    self.show_explorer = visible;
                }
                GuiUiCommand::SetComparison {
                    tab_id,
                    previous_block_id,
                    current_block_id,
                } => {
                    let target_idx = tab_id
                        .and_then(|id| self.tabs.iter().position(|t| t.id == id))
                        .unwrap_or(self.active_tab);
                    if let Some(tab) = self.tabs.get_mut(target_idx) {
                        tab.comparison = Some((previous_block_id, current_block_id));
                    }
                }
            }
        }
        if let Some(cmd_text) = pending_submit {
            self.submit_input(Some(cmd_text), false);
        }
    }

    fn process_async_events(&mut self) {
        for (block_id, (tab_id, output)) in &self.running_output {
            let chunk = output.lock().map(|mut pending| std::mem::take(&mut *pending)).unwrap_or_default();
            if chunk.is_empty() { continue; }
            if let Some(block) = self.tabs.iter_mut().find(|tab| tab.id == *tab_id)
                .and_then(|tab| tab.blocks.iter_mut().find(|block| block.id == *block_id)) {
                block.raw_output.push_str(&chunk);
                (block.ansi_spans, block.plain_output) = mrt_core::parse_ansi_text(&block.raw_output);
            }
        }
        while let Ok(ev) = self.rx.try_recv() {
            self.busy_count = self.busy_count.saturating_sub(1);
            match ev {
                AsyncWorkerEvent::BlockCompleted {
                    tab_id,
                    block,
                    updated_cwd,
                } => {
                    self.running_output.remove(&block.id);
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
                        if let Some(running) = tab.blocks.iter_mut().find(|running| running.id == block.id) {
                            let pinned = running.pinned;
                            *running = block.clone();
                            running.pinned = pinned;
                        } else {
                            tab.blocks.push(block.clone());
                        }
                    }
                    self.sync_mcp_from_gui();
                    self.on_runbook_block_completed(&block);
                }
                AsyncWorkerEvent::AiCommandReady { tab_id, proposal } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if tab.input_line.is_empty() {
                            tab.input_line = proposal.proposed_command.clone();
                        }
                        tab.ai_proposal = Some(proposal);
                    }
                }
                AsyncWorkerEvent::ExplorerLoaded { path, entries } => {
                    if self.explorer_path.as_ref() == Some(&path) {
                        self.explorer_entries = entries;
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

        if raw_input.starts_with(":dns") {
            self.spawn_dns_command(tab_id, cwd, raw_input);
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
                let block_id = self.spawn_shell_command(
                    pending.tab_id,
                    pending.shell,
                    pending.cwd,
                    pending.assessment.command,
                );
                if let Some(replay) = self.active_replay.as_mut() {
                    if replay.tab_id == pending.tab_id && replay.waiting_block_id.is_none() {
                        replay.waiting_block_id = Some(block_id);
                    }
                }
            }
        }
    }

    fn start_runbook_replay(&mut self, runbook: crate::history::Runbook) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if runbook.steps.is_empty() {
            return;
        }
        self.runbooks_state.replay_notice = None;
        self.active_replay = Some(ActiveRunbookReplay {
            runbook_name: runbook.name,
            tab_id: tab.id,
            steps: runbook.steps,
            next_step_idx: 0,
            waiting_block_id: None,
        });
        self.advance_runbook_replay();
    }

    fn stop_runbook_replay(&mut self, notice: Option<String>) {
        self.active_replay = None;
        if let Some(msg) = notice {
            self.runbooks_state.replay_notice = Some(msg);
        }
    }

    fn on_runbook_block_completed(&mut self, block: &CommandBlock) {
        let Some(replay) = self.active_replay.as_mut() else {
            return;
        };
        if replay.waiting_block_id != Some(block.id) {
            return;
        }
        replay.waiting_block_id = None;
        let step_num = replay.next_step_idx;
        let total_steps = replay.steps.len();
        let runbook_name = replay.runbook_name.clone();
        if block.exit_code == Some(0) {
            self.advance_runbook_replay();
        } else {
            let code_label = block
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown".into());
            self.stop_runbook_replay(Some(format!(
                "Runbook '{runbook_name}' stopped at step {step_num}/{total_steps}: '{}' exited with code {code_label}.",
                block.command
            )));
        }
    }

    fn advance_runbook_replay(&mut self) {
        if self.pending_safety.is_some() {
            return;
        }
        let Some(replay) = self.active_replay.as_mut() else {
            return;
        };
        if replay.waiting_block_id.is_some() {
            return;
        }
        let tab_id = replay.tab_id;
        let Some(tab_cwd) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| t.cwd.clone())
        else {
            self.stop_runbook_replay(Some(
                "Runbook stopped because its target tab was closed.".into(),
            ));
            return;
        };
        if replay.next_step_idx >= replay.steps.len() {
            let name = replay.runbook_name.clone();
            let total = replay.steps.len();
            self.stop_runbook_replay(Some(format!(
                "Runbook '{name}' completed ({total}/{total} steps)."
            )));
            return;
        }

        let step = replay.steps[replay.next_step_idx].clone();
        replay.next_step_idx += 1;

        let shell = self
            .shells
            .iter()
            .find(|s| s.kind.short_label() == step.shell_name)
            .cloned()
            .unwrap_or_else(|| self.shells[0].clone());
        let cwd = if step.cwd.trim().is_empty() {
            tab_cwd
        } else {
            PathBuf::from(&step.cwd)
        };

        let assessment = assess_command(&step.command);
        if assessment.requires_approval || assessment.risk == RiskLevel::Critical {
            self.pending_safety = Some(PendingSafetyApproval {
                tab_id,
                assessment,
                shell,
                cwd,
            });
            return;
        }

        let block_id = self.spawn_shell_command(tab_id, shell, cwd, step.command);
        if let Some(replay) = self.active_replay.as_mut() {
            replay.waiting_block_id = Some(block_id);
        }
    }

    fn spawn_shell_command(&mut self, tab_id: Uuid, shell: ShellProfile, cwd: PathBuf, raw_input: String) -> Uuid {
        let mut running = CommandBlock::new_completed(&raw_input, shell.kind.short_label(), cwd.display().to_string(), "", 0, 0);
        running.exit_code = None;
        let block_id = running.id;
        let timestamp = running.timestamp;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.blocks.push(running);
        }
        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.running_output.insert(block_id, (tab_id, output.clone()));
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let cwd_str = cwd.display().to_string();
            match execute_command_streaming(&shell, &raw_input, &cwd, |chunk| {
                if let Ok(mut pending) = output.lock() { pending.push_str(chunk); }
            }).await {
                Ok(res) => {
                    let mut block = CommandBlock::new_completed(
                        &raw_input,
                        shell.kind.short_label(),
                        cwd_str,
                        res.combined_output,
                        res.exit_code,
                        res.duration_ms,
                    );
                    block.id = block_id;
                    block.timestamp = timestamp;
                    let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                        tab_id,
                        block,
                        updated_cwd: res.updated_cwd,
                    });
                }
                Err(e) => {
                    let mut block = CommandBlock::new_completed(
                        &raw_input,
                        shell.kind.short_label(),
                        cwd_str,
                        format!("Error: {e}"),
                        1,
                        0,
                    );
                    block.id = block_id;
                    block.timestamp = timestamp;
                    let _ = tx.send(AsyncWorkerEvent::BlockCompleted {
                        tab_id,
                        block,
                        updated_cwd: None,
                    });
                }
            }
        });
        block_id
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

    fn spawn_dns_command(&mut self, tab_id: Uuid, cwd: PathBuf, cmd_str: String) {
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = std::time::Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match inspect_dns(&cmd_str).await {
                Ok((structured, plain, _)) => CommandBlock::new_structured(
                    &cmd_str,
                    "mrt-dns",
                    cwd_str,
                    plain,
                    structured,
                    0,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => CommandBlock::new_completed(
                    &cmd_str,
                    "mrt-dns",
                    cwd_str,
                    format!("DNS lookup failed: {e}"),
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
            SidePanelEvent::RunDnsTrace { host } => {
                let cmd = if host.trim().starts_with(":dns") {
                    host.trim().to_string()
                } else {
                    format!(":dns {}", host.trim())
                };
                self.spawn_dns_command(tab_id, cwd, cmd);
            }
        }
    }
}

fn read_directory(path: &std::path::Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
    let mut entries = std::fs::read_dir(path)?
        .map(|entry| {
            let path = entry?.path();
            let directory = path.is_dir();
            Ok((path, directory))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by(|(left, left_dir), (right, right_dir)| {
        right_dir.cmp(left_dir).then_with(|| {
            left.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
                .cmp(
                    &right
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase(),
                )
        })
    });
    Ok(entries)
}

impl eframe::App for MrtGuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let first_frame = !self.focused_on_start;
        let mut focus_prompt = first_frame;
        let palette_was_open = self.show_command_palette;
        let previous_tab = self.tabs[self.active_tab].id;
        if first_frame {
            self.focused_on_start = true;
            apply_mrt_theme(ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::Normal,
            ));
        }

        self.process_async_events();
        self.process_history_events();
        self.process_mcp_ui_commands();

        ctx.request_repaint_after(std::time::Duration::from_millis(if self.busy_count > 0 || self.history_pending > 0 {
            50
        } else {
            250
        }));

        // Keyboard shortcut for Command Palette (Ctrl+K or Ctrl+P)
        if ctx.input_mut(|i| {
            i.consume_key(Modifiers::CTRL, Key::K) || i.consume_key(Modifiers::CTRL, Key::P)
        }) {
            self.show_command_palette = !self.show_command_palette;
        }

        // Top Navigation Bar: Tabs, Shell Switcher, and Side Panel Toggles
        egui::TopBottomPanel::top("mrt_top_bar")
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_SURFACE)
                    .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("MRT")
                        .size(14.0)
                        .monospace()
                        .strong()
                        .color(Palette::ACCENT_BLUE),
                );
                ui.separator();

                ui.toggle_value(&mut self.mosaic, "Mosaic");
                ui.toggle_value(&mut self.show_explorer, "Explorer");

                // Render Tabs
                let mut tab_to_close = None;
                for idx in 0..self.tabs.len() {
                    let is_active = idx == self.active_tab;
                    let title = self.tabs[idx].title.clone();
                    let btn = egui::Button::new(
                        RichText::new(&title)
                            .size(12.0)
                            .color(if is_active {
                                Palette::TEXT_PRIMARY
                            } else {
                                Palette::TEXT_SECONDARY
                            }),
                    )
                    .fill(if is_active {
                        Palette::BG_OVERLAY
                    } else {
                        Color32::TRANSPARENT
                    })
                    .stroke(if is_active {
                        egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE)
                    } else {
                        egui::Stroke::NONE
                    });

                    if ui.add(btn).clicked() {
                        self.active_tab = idx;
                    }
                    if self.tabs.len() > 1 && ui.small_button("×").clicked() {
                        tab_to_close = Some(idx);
                    }
                }
                if let Some(close_idx) = tab_to_close {
                    let id = self.tabs[close_idx].id;
                    self.close_terminal(id);
                }

                // New Tab dropdown per discovered shell
                egui::menu::menu_button(ui, "+ Tab", |ui| {
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

                // Active Tab Shell Profile Switcher
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    let current_shell_name = self
                        .shells
                        .get(tab.active_shell_idx)
                        .map(|s| s.kind.short_label().to_string())
                        .unwrap_or_else(|| "shell".into());

                    egui::ComboBox::from_id_salt("active_shell_selector")
                        .selected_text(RichText::new(current_shell_name).monospace().size(12.0))
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
                    if ui.button("Ctrl+K").clicked() {
                        self.show_command_palette = !self.show_command_palette;
                    }
                    ui.toggle_value(&mut self.side_state.show_internet_panel, "Net");
                    ui.toggle_value(&mut self.side_state.show_mcp_panel, "MCP");
                    if ui.toggle_value(&mut self.show_history, "History").changed() && self.show_history {
                        self.refresh_history();
                    }
                    if ui
                        .toggle_value(&mut self.runbooks_state.show_panel, "Runbooks")
                        .changed()
                        && self.runbooks_state.show_panel
                    {
                        self.refresh_runbooks();
                        self.refresh_history();
                    }
                });
            });
        });

        if self.show_explorer {
            self.render_explorer(ctx);
        }

        if self.show_history {
            let mut restore = None;
            let mut add_to_runbook: Option<crate::history::RunbookStep> = None;
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
                            ui.horizontal(|ui| {
                                if ui.button("View output").clicked() {
                                    restore = Some(block.clone());
                                }
                                if ui.button("+ Runbook").clicked() {
                                    add_to_runbook = Some(crate::history::RunbookStep {
                                        command: block.command.clone(),
                                        shell_name: block.shell_name.clone(),
                                        cwd: block.cwd.clone(),
                                    });
                                }
                            });
                        });
                    }
                });
            });
            if let Some(step) = add_to_runbook {
                self.runbooks_state.show_panel = true;
                self.refresh_runbooks();
                if let Some(draft) = self.runbooks_state.editing.as_mut() {
                    draft.steps.push(step);
                } else {
                    self.runbooks_state.validation_error = None;
                    self.runbooks_state.editing = Some(crate::history::Runbook {
                        id: Uuid::new_v4(),
                        name: String::new(),
                        steps: vec![step],
                        updated_at: chrono::Utc::now(),
                    });
                }
            }
            if let Some(block) = restore {
                let shell_idx = self.shells.iter().position(|shell| shell.kind.short_label() == block.shell_name).unwrap_or(0);
                let mut tab = TerminalTab::new("History".into(), shell_idx, PathBuf::from(&block.cwd));
                tab.command_history.push(block.command.clone());
                tab.blocks.push(block);
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
        }

        if self.runbooks_state.show_panel {
            let available_shells: Vec<String> = self
                .shells
                .iter()
                .map(|s| s.kind.short_label().to_string())
                .collect();
            let active_tab = self.tabs.get(self.active_tab);
            let default_shell = active_tab
                .and_then(|t| self.shells.get(t.active_shell_idx))
                .map(|s| s.kind.short_label().to_string())
                .unwrap_or_else(|| {
                    available_shells
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "sh".into())
                });
            let default_cwd = active_tab
                .map(|t| t.cwd.display().to_string())
                .unwrap_or_else(|| ".".into());
            let mut runbook_action = None;
            egui::SidePanel::right("runbooks_panel")
                .default_width(380.0)
                .resizable(true)
                .show(ctx, |ui| {
                    let replay_status = self.active_replay.as_ref().map(|r| ReplayStatusView {
                        runbook_name: &r.runbook_name,
                        current_step: r.next_step_idx.min(r.steps.len()),
                        total_steps: r.steps.len(),
                    });
                    runbook_action = render_runbooks_panel(
                        ui,
                        &mut self.runbooks_state,
                        replay_status,
                        &mut self.history_query,
                        &self.history_results,
                        &available_shells,
                        &default_shell,
                        &default_cwd,
                    );
                });
            if let Some(action) = runbook_action {
                match action {
                    RunbookAction::StartReplay(runbook) => self.start_runbook_replay(runbook),
                    RunbookAction::StopReplay => {
                        self.stop_runbook_replay(Some("Runbook replay stopped by user.".into()));
                    }
                    RunbookAction::SaveRunbook(runbook) => {
                        if let Some(worker) = &self.history {
                            if worker.send(crate::history::HistoryRequest::SaveRunbook(runbook)) {
                                self.history_pending += 1;
                            }
                        }
                    }
                    RunbookAction::DeleteRunbook(id) => {
                        if let Some(worker) = &self.history {
                            if worker.send(crate::history::HistoryRequest::DeleteRunbook(id)) {
                                self.history_pending += 1;
                            }
                        }
                    }
                    RunbookAction::SearchHistory => self.refresh_history(),
                }
            }
        }

        // Right-hand Side Panels (MCP Hub & Internet Studio)
        let mut panel_event = None;
        if self.side_state.show_mcp_panel {
            egui::SidePanel::right("mrt_mcp_panel")
                .default_width(330.0)
                .resizable(true)
                .show(ctx, |ui| {
                    if let Some(ev) = render_mcp_side_panel(
                        ui,
                        &mut self.side_state,
                        &self.mcp_host,
                        self.bridge_port,
                    ) {
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
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_SURFACE)
                    .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::symmetric(16.0, 12.0)),
            )
            .show(ctx, |ui| {
            if !self.show_history {
                if let Some(error) = &self.history_error {
                    ui.colored_label(Palette::ACCENT_RED, error);
                }
            }
            if self.mosaic {
                ui.label(
                    RichText::new(format!("Active · {}", self.tabs[self.active_tab].title))
                        .size(11.0)
                        .color(Palette::TEXT_MUTED),
                );
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
                    .fill(Color32::from_rgba_unmultiplied(88, 166, 255, 18))
                    .stroke(egui::Stroke::new(1.0_f32, Palette::ACCENT_BLUE))
                    .rounding(egui::Rounding::same(6.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("AI · {provider}"))
                                    .size(11.5)
                                    .strong()
                                    .color(Palette::ACCENT_BLUE),
                            );
                            ui.label(
                                RichText::new(cmd)
                                    .monospace()
                                    .size(12.5)
                                    .strong()
                                    .color(Palette::TEXT_PRIMARY),
                            );
                            ui.label(
                                RichText::new(format!("— {explanation}"))
                                    .size(12.0)
                                    .color(Palette::TEXT_SECONDARY),
                            );
                            if ui.small_button("Run").clicked() {
                                run_ai_cmd = Some(cmd.clone());
                                dismiss_ai = true;
                            }
                            if ui.small_button("Dismiss").clicked() {
                                dismiss_ai = true;
                            }
                        });
                    });
                ui.add_space(6.0);
            }
            if dismiss_ai {
                tab.ai_proposal = None;
            }
            if let Some(c) = run_ai_cmd {
                self.submit_input(Some(c), false);
            }

            let mut should_submit = false;
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                ui.add(
                    egui::Label::new(
                        RichText::new(tab.cwd.display().to_string())
                            .size(11.5)
                            .monospace()
                            .color(Palette::TEXT_MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(tab.cwd.display().to_string());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("›")
                            .size(20.0)
                            .monospace()
                            .strong()
                            .color(Palette::ACCENT_GREEN),
                    );
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut tab.input_line)
                            .id(egui::Id::new(("command_input", tab.id)))
                            .font(egui::FontId::monospace(14.5))
                            .margin(egui::Margin::symmetric(10.0, 8.0))
                            .hint_text(if tab.blocks.is_empty() {
                                "Enter a command, # for AI, or Ctrl+K for palette"
                            } else {
                                "Enter a command..."
                            })
                            .desired_width((ui.available_width() - 72.0).max(32.0)),
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

                    if ui.add_enabled(!tab.input_line.trim().is_empty(), egui::Button::new("Run").min_size(egui::vec2(52.0, 34.0))).clicked() {
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
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_BASE)
                    .inner_margin(egui::Margin::same(16.0)),
            )
            .show(ctx, |ui| {
            if self.mosaic {
                self.render_mosaic(ui);
                return;
            }
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
                            ui.add_space(10.0);
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
                self.handle_block_action(action);
            }
        });

        // Safety Gate Modal Overlay
        if self.pending_safety.is_some() {
            let mut approve = false;
            let mut cancel = false;

            egui::Area::new(egui::Id::new("safety_backdrop"))
                .fixed_pos(egui::Pos2::ZERO)
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    ui.painter().rect_filled(
                        screen,
                        0.0,
                        Color32::from_black_alpha(140),
                    );
                });

            if let Some(ref pending) = self.pending_safety {
                egui::Window::new("MRT Safety Gate")
                    .title_bar(false)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(460.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new("Destructive Command Intercepted")
                                .size(15.0)
                                .strong()
                                .color(Palette::ACCENT_RED),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                pending.assessment.risk.label(),
                                pending.assessment.reason
                            ))
                            .size(12.0)
                            .color(Palette::TEXT_SECONDARY),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                pending.shell.name,
                                pending.cwd.display()
                            ))
                            .size(11.5)
                            .monospace()
                            .color(Palette::TEXT_MUTED),
                        );
                        ui.add_space(8.0);
                        egui::Frame::none()
                            .fill(Color32::from_rgba_unmultiplied(248, 81, 73, 20))
                            .stroke(egui::Stroke::new(1.0_f32, Palette::ACCENT_RED))
                            .rounding(egui::Rounding::same(6.0))
                            .inner_margin(egui::Margin::same(10.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&pending.assessment.command)
                                        .monospace()
                                        .size(13.0)
                                        .strong()
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button(
                                    RichText::new("Approve & Execute")
                                        .color(Palette::ACCENT_RED),
                                )
                                .clicked()
                            {
                                approve = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
            }

            if approve {
                self.approve_pending_command();
            } else if cancel {
                let cancelled_tab = self.pending_safety.as_ref().map(|p| p.tab_id);
                self.pending_safety = None;
                if let Some(replay) = &self.active_replay {
                    if Some(replay.tab_id) == cancelled_tab && replay.waiting_block_id.is_none() {
                        let name = replay.runbook_name.clone();
                        let step = replay.next_step_idx;
                        let total = replay.steps.len();
                        self.stop_runbook_replay(Some(format!(
                            "Runbook '{name}' stopped at step {step}/{total}: cancelled at Safety Gate."
                        )));
                    }
                }
            }
        }

        // Universal Command Palette Modal (Ctrl+K / Ctrl+P)
        if self.show_command_palette {
            let mut close_palette = false;
            let mut cmd_to_run = None;
            let mut switch_shell_idx = None;

            egui::Area::new(egui::Id::new("palette_backdrop"))
                .fixed_pos(egui::Pos2::ZERO)
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    ui.painter().rect_filled(
                        screen,
                        0.0,
                        Color32::from_black_alpha(130),
                    );
                });

            egui::Window::new("Command palette")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .default_width(500.0)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 64.0])
                .show(ctx, |ui| {
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.palette_query)
                            .font(egui::FontId::proportional(14.0))
                            .margin(egui::Margin::symmetric(10.0, 8.0))
                            .hint_text("Type a command or switch shell...")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    ui.separator();

                    if !palette_was_open { search.request_focus(); }
                    let q = self.palette_query.trim().to_lowercase();
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        ui.label(
                            RichText::new("Shell Profiles")
                                .size(11.0)
                                .strong()
                                .color(Palette::TEXT_MUTED),
                        );
                        for (idx, s) in self.shells.iter().enumerate() {
                            let label = format!("Switch Shell: {} ({})", s.name, s.program);
                            if q.is_empty() || label.to_lowercase().contains(&q) {
                                if ui.button(label).clicked() {
                                    switch_shell_idx = Some(idx);
                                    close_palette = true;
                                }
                            }
                        }

                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Tools & Actions")
                                .size(11.0)
                                .strong()
                                .color(Palette::TEXT_MUTED),
                        );
                        for (title, cmd) in [
                            ("Scan Listening TCP/UDP Ports", ":ports"),
                            ("DNS & Latency Trace (google.com)", ":dns google.com"),
                            ("HTTP GET Sample JSON API", ":http GET https://httpbin.org/json"),
                            ("Fetch Rust Homepage as Markdown", ":web https://www.rust-lang.org"),
                            ("AI: Show System Info in Active Shell", "# show system info"),
                            ("Fetch example.com as Markdown", ":web https://example.com"),
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
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Esc to close")
                                .size(11.0)
                                .color(Palette::TEXT_MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Close").clicked() {
                                close_palette = true;
                            }
                        });
                    });
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

        self.sync_mcp_from_gui();
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

    #[test]
    fn mosaic_reordering_and_closing_preserve_terminal_identity() {
        let mut app = MrtGuiApp::new();
        let first = app.tabs[0].id;
        let cwd = app.tabs[0].cwd.clone();
        app.tabs
            .push(TerminalTab::new("Second".into(), 0, cwd.clone()));
        app.tabs.push(TerminalTab::new("Third".into(), 0, cwd));
        app.active_tab = 1;
        let second = app.tabs[1].id;
        let third = app.tabs[2].id;
        app.move_terminal(first, third);
        assert_eq!(
            app.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            vec![second, third, first]
        );
        assert_eq!(app.tabs[app.active_tab].id, second);
        app.close_terminal(third);
        assert_eq!(app.tabs[app.active_tab].id, second);
        app.close_terminal(second);
        app.close_terminal(first);
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn explorer_tracks_the_selected_terminal_and_rejects_old_results() {
        let mut app = MrtGuiApp::new();
        let path = app.tabs[0].cwd.clone();
        app.explorer_path = Some(path.clone());
        app.tx
            .send(AsyncWorkerEvent::ExplorerLoaded {
                path: path.join("old-folder"),
                entries: Err("stale".into()),
            })
            .unwrap();
        app.process_async_events();
        assert!(app.explorer_entries.is_ok());
        let entries = read_directory(&path).unwrap();
        assert!(!entries.is_empty());
        assert!(entries
            .iter()
            .all(|(entry, directory)| entry.parent() == Some(path.as_path())
                && *directory == entry.is_dir()));
        assert!(entries.windows(2).all(|pair| pair[0].1 || !pair[1].1));
        app.tabs[0].cwd = path.join("src");
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.render_explorer(ctx));
        assert_eq!(app.explorer_path, Some(app.tabs[0].cwd.clone()));
    }

    #[test]
    fn mosaic_renders_multiple_terminals_at_narrow_widths() {
        let mut app = MrtGuiApp::new();
        for number in 0..3 {
            app.tabs.push(TerminalTab::new(
                format!("Terminal {number}"),
                0,
                app.tabs[0].cwd.clone(),
            ));
        }
        let ctx = egui::Context::default();
        for width in [320.0, 900.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 700.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.render_mosaic(ui));
            });
            assert!(!output.shapes.is_empty());
        }
    }

    #[test]
    fn streaming_updates_the_originating_block_and_completes_without_duplicates() {
        let mut app = MrtGuiApp::new();
        let tab_id = app.tabs[0].id;
        let cwd = app.tabs[0].cwd.clone();
        let mut block = CommandBlock::new_completed("echo streaming", "test", cwd.display().to_string(), "", 0, 0);
        block.exit_code = None;
        let block_id = block.id;
        app.tabs[0].blocks.push(block.clone());
        app.tabs.push(TerminalTab::new("Other".into(), 0, cwd));
        app.active_tab = 1;
        let output = std::sync::Arc::new(std::sync::Mutex::new("first".into()));
        app.running_output.insert(block_id, (tab_id, output.clone()));
        app.busy_count = 1;
        app.process_async_events();
        assert_eq!(app.tabs[0].blocks[0].plain_output, "first");
        assert!(app.tabs[1].blocks.is_empty());
        assert_eq!(app.busy_count, 1);
        assert!(output.lock().unwrap().is_empty());
        block.exit_code = Some(0);
        block.raw_output = "first last".into();
        block.plain_output = "first last".into();
        app.tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd: None }).unwrap();
        app.process_async_events();
        assert_eq!(app.tabs[0].blocks.len(), 1);
        assert_eq!(app.tabs[0].blocks[0].id, block_id);
        assert_eq!(app.tabs[0].blocks[0].exit_code, Some(0));
        assert_eq!(app.tabs[0].blocks[0].plain_output, "first last");
        assert!(app.running_output.is_empty());
        assert_eq!(app.busy_count, 0);
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

    #[test]
    fn mcp_tools_synchronize_live_gui_tabs_and_layout() {
        let mut app = MrtGuiApp::new();
        let host = app.mcp_host.clone();

        // Agent creates a tab via MCP
        app.runtime.block_on(async {
            host.call_tool(
                "mrt-native-mcp",
                "mrt_create_tab",
                json!({ "title": "MCP Controlled Tab", "activate": true }),
            )
            .await
            .unwrap();
        });
        app.process_mcp_ui_commands();
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs[app.active_tab].title, "MCP Controlled Tab");

        // Agent toggles Mosaic & Explorer and sets input draft via mrt_control_gui
        app.runtime.block_on(async {
            host.call_tool(
                "mrt-native-mcp",
                "mrt_control_gui",
                json!({
                    "input_text": "echo drafted_by_agent",
                    "submit_input": false,
                    "mosaic": true,
                    "mosaic_columns": 3,
                    "explorer": true
                }),
            )
            .await
            .unwrap();
        });
        app.process_mcp_ui_commands();
        assert!(app.mosaic);
        assert_eq!(app.mosaic_columns, 3);
        assert!(app.show_explorer);
        assert_eq!(app.tabs[app.active_tab].input_line, "echo drafted_by_agent");
    }

    #[test]
    fn runbook_replay_advances_and_stops_on_first_failure() {
        let mut app = MrtGuiApp::new();
        let shell_name = app.shells[0].kind.short_label().to_string();
        let cwd = app.tabs[0].cwd.display().to_string();
        let runbook = crate::history::Runbook {
            id: Uuid::new_v4(),
            name: "Test sequence".into(),
            steps: vec![
                crate::history::RunbookStep {
                    command: "echo step-one".into(),
                    shell_name: shell_name.clone(),
                    cwd: cwd.clone(),
                },
                crate::history::RunbookStep {
                    command: "exit 2".into(),
                    shell_name: shell_name.clone(),
                    cwd: cwd.clone(),
                },
                crate::history::RunbookStep {
                    command: "echo step-three".into(),
                    shell_name,
                    cwd,
                },
            ],
            updated_at: chrono::Utc::now(),
        };

        app.start_runbook_replay(runbook);
        assert!(app.active_replay.is_some());

        // Wait for step 1 to complete
        let start = std::time::Instant::now();
        while app.tabs[0].blocks.is_empty()
            || app.tabs[0].blocks[0].exit_code.is_none()
        {
            app.process_async_events();
            assert!(start.elapsed() < std::time::Duration::from_secs(15), "Step 1 timed out");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.tabs[0].blocks[0].exit_code, Some(0));
        assert!(app.tabs[0].blocks[0].plain_output.contains("step-one"));

        // Wait for step 2 to fail and stop the replay
        let start = std::time::Instant::now();
        while app.active_replay.is_some() {
            app.process_async_events();
            assert!(start.elapsed() < std::time::Duration::from_secs(15), "Step 2 timed out");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert_eq!(app.tabs[0].blocks.len(), 2);
        assert_ne!(app.tabs[0].blocks[1].exit_code, Some(0));
        let notice = app.runbooks_state.replay_notice.as_deref().unwrap_or("");
        assert!(notice.contains("stopped at step 2/3"));
    }

    #[test]
    fn runbook_replay_passes_through_safety_gate() {
        let mut app = MrtGuiApp::new();
        let shell_name = app.shells[0].kind.short_label().to_string();
        let cwd = app.tabs[0].cwd.display().to_string();
        let runbook = crate::history::Runbook {
            id: Uuid::new_v4(),
            name: "Dangerous sequence".into(),
            steps: vec![crate::history::RunbookStep {
                command: "rm -rf /tmp/mrt-nonexistent-test".into(),
                shell_name,
                cwd,
            }],
            updated_at: chrono::Utc::now(),
        };

        app.start_runbook_replay(runbook);
        assert!(app.pending_safety.is_some());
        assert_eq!(app.busy_count, 0);
        assert!(app
            .active_replay
            .as_ref()
            .is_some_and(|r| r.waiting_block_id.is_none()));
    }

    #[test]
    fn dns_command_creates_structured_block() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(":dns 127.0.0.1:9".to_string()), false);

        let start = std::time::Instant::now();
        while app.tabs[0].blocks.is_empty()
            || app.tabs[0].blocks[0].exit_code.is_none()
        {
            app.process_async_events();
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "DNS command timed out");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let block = &app.tabs[0].blocks[0];
        assert_eq!(block.exit_code, Some(0));
        assert!(block.structured.is_structured());
        assert!(block.plain_output.contains("127.0.0.1"));
    }
}
