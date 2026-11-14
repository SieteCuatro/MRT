use crate::theme::{
    apply_mrt_theme, chip, icon_button, panel_frame, primary_button, secondary_button, section_label,
    Palette, Radius,
};
use crate::widgets::block_view::{render_command_block, BlockAction};
use crate::widgets::chrome::{
    brand_mark, centered_column, nav_row, palette_row, search_pill, tab_row, TabStatus,
};
use crate::widgets::runbooks::{
    render_runbooks_panel, ReplayStatusView, RunbookAction, RunbooksUiState,
};
use crate::widgets::side_panels::{
    render_internet_side_panel, render_mcp_side_panel, SidePanelEvent, SidePanelState,
};
use egui::{Color32, Key, Modifiers, RichText};
use mrt_ai::{
    diagnose_command_error_streaming, translate_nl_to_command,
    AiCommandProposal,
};
use mrt_core::{
    assess_command, discover_shells, execute_command_streaming, CommandBlock, InteractivePty,
    RiskLevel, SafetyAssessment, ShellProfile, StructuredOutput,
};
use mrt_mcp::{
    start_gui_bridge_listener, GuiUiCommand, McpHostManager, McpTabSession, MrtMcpServerState,
};
use mrt_tools::{
    execute_http_request, fetch_url_as_markdown, generate_markdown_bug_report, inspect_dns,
    inspect_listening_ports, inspect_websocket, parse_http_command,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    None,
    Horizontal,
    Vertical,
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct SplitPane {
    pub id: Uuid,
    pub active_shell_idx: usize,
    pub cwd: PathBuf,
    pub blocks: Vec<CommandBlock>,
    pub input_line: String,
    pub table_filters: HashMap<Uuid, String>,
}

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
    pub split_direction: SplitDirection,
    pub secondary_pane: Option<SplitPane>,
    pub active_pane_idx: usize,
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
            split_direction: SplitDirection::None,
            secondary_pane: None,
            active_pane_idx: 0,
        }
    }

    pub fn split(&mut self, direction: SplitDirection, shells_count: usize) {
        let shell_idx = self.active_shell_idx.min(shells_count.saturating_sub(1));
        self.secondary_pane = Some(SplitPane {
            id: Uuid::new_v4(),
            active_shell_idx: shell_idx,
            cwd: self.cwd.clone(),
            blocks: Vec::new(),
            input_line: String::new(),
            table_filters: HashMap::new(),
        });
        self.split_direction = direction;
        self.active_pane_idx = 1;
    }

    pub fn close_split(&mut self) {
        self.split_direction = SplitDirection::None;
        self.secondary_pane = None;
        self.active_pane_idx = 0;
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
    AiExplanationChunk {
        tab_id: Uuid,
        block_id: Uuid,
        chunk: String,
        suggested_fix: Option<String>,
        is_final: bool,
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

pub struct ActiveTuiSession {
    pub pty: std::sync::Arc<InteractivePty>,
    pub tab_id: Uuid,
    pub command: String,
    pub start_time: std::time::Instant,
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
    pub active_tui: Option<ActiveTuiSession>,
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
    sidebar_collapsed: bool,
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
            active_tui: None,
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
            sidebar_collapsed: false,
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
                    let tab_id = tab.id;
                    let shell = self
                        .shells
                        .get(tab.active_shell_idx)
                        .cloned()
                        .unwrap_or_else(|| self.shells[0].clone());
                    if let Some(blk) = tab.blocks.iter_mut().find(|b| b.id == block_id) {
                        blk.ai_explanation = Some(String::new());
                        let cmd = blk.command.clone();
                        let out = blk.plain_output.clone();
                        let code = blk.exit_code.unwrap_or(1);
                        let tx = self.tx.clone();

                        self.busy_count += 1;
                        self.runtime.spawn(async move {
                            let tx_inner = tx.clone();
                            let triage = diagnose_command_error_streaming(
                                &cmd,
                                &out,
                                code,
                                &shell,
                                move |token| {
                                    let _ = tx_inner.send(AsyncWorkerEvent::AiExplanationChunk {
                                        tab_id,
                                        block_id,
                                        chunk: token.to_string(),
                                        suggested_fix: None,
                                        is_final: false,
                                    });
                                },
                            )
                            .await;

                            let _ = tx.send(AsyncWorkerEvent::AiExplanationChunk {
                                tab_id,
                                block_id,
                                chunk: String::new(),
                                suggested_fix: triage.suggested_fix,
                                is_final: true,
                            });
                        });
                    }
                }
            }
            BlockAction::CopyBugReport(block_id) => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    if let Some(idx) = tab.blocks.iter().position(|b| b.id == block_id) {
                        let blk = &tab.blocks[idx];
                        let history = &tab.blocks[..idx];
                        let os_info = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
                        let report = generate_markdown_bug_report(blk, history, &os_info, "0.1.0");
                        let new_block = CommandBlock::new_structured(
                            &format!(":report {}", blk.command),
                            "mrt-report",
                            tab.cwd.display().to_string(),
                            report.clone(),
                            StructuredOutput::Markdown(report),
                            0,
                            0,
                        );
                        tab.blocks.push(new_block);
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
        egui::SidePanel::left("folder_explorer").frame(panel_frame())
            .default_width(240.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Explorer").size(15.0).strong().color(Palette::TEXT_PRIMARY));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, "🔄", "Refresh").clicked() {
                            refresh = true;
                        }
                    });
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(path.display().to_string())
                            .monospace()
                            .size(12.0)
                            .color(Palette::TEXT_MUTED),
                    )
                    .truncate(),
                )
                .on_hover_text(path.display().to_string());
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                if let Some(parent) = path.parent() {
                    if nav_row(ui, "↑", "Parent folder", None, false, false).clicked() {
                        navigate = Some(parent.to_path_buf());
                    }
                }
                match &self.explorer_entries {
                    Err(error) => {
                        ui.colored_label(Palette::ACCENT_RED, error);
                    }
                    Ok(entries) => {
                        egui::ScrollArea::vertical()
                            .id_salt("explorer_entries")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (entry, directory) in entries {
                                    let name =
                                        entry.file_name().unwrap_or_default().to_string_lossy();
                                    if *directory {
                                        if nav_row(ui, "🗀", &name, Some("›"), false, false).clicked() {
                                            navigate = Some(entry.clone());
                                        }
                                    } else {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), 30.0),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().text(
                                            egui::pos2(rect.left() + 42.0, rect.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            name.as_ref(),
                                            egui::FontId::proportional(13.5),
                                            Palette::TEXT_SECONDARY,
                                        );
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
            let found_in_primary = self.tabs.iter_mut().find(|tab| tab.id == *tab_id)
                .and_then(|tab| tab.blocks.iter_mut().find(|block| block.id == *block_id));
            if let Some(block) = found_in_primary {
                block.raw_output.push_str(&chunk);
                (block.ansi_spans, block.plain_output) = mrt_core::parse_ansi_text(&block.raw_output);
            } else if let Some(sec) = self.tabs.iter_mut().find(|tab| tab.id == *tab_id).and_then(|tab| tab.secondary_pane.as_mut()) {
                if let Some(block) = sec.blocks.iter_mut().find(|block| block.id == *block_id) {
                    block.raw_output.push_str(&chunk);
                    (block.ansi_spans, block.plain_output) = mrt_core::parse_ansi_text(&block.raw_output);
                }
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
                        } else if let Some(running) = tab.secondary_pane.as_mut().and_then(|s| s.blocks.iter_mut().find(|b| b.id == block.id)) {
                            let pinned = running.pinned;
                            *running = block.clone();
                            running.pinned = pinned;
                        } else if tab.active_pane_idx == 1 && tab.secondary_pane.is_some() {
                            tab.secondary_pane.as_mut().unwrap().blocks.push(block.clone());
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
                AsyncWorkerEvent::AiExplanationChunk {
                    tab_id,
                    block_id,
                    chunk,
                    suggested_fix,
                    is_final,
                } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if let Some(blk) = tab.blocks.iter_mut().find(|b| b.id == block_id) {
                            let exp = blk.ai_explanation.get_or_insert_with(String::new);
                            exp.push_str(&chunk);
                            if is_final {
                                if let Some(fix) = suggested_fix {
                                    blk.ai_suggested_fix = Some(fix);
                                }
                            }
                        }
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

        if raw_input.eq_ignore_ascii_case(":split") || raw_input.eq_ignore_ascii_case(":split-h") {
            let shells_count = self.shells.len();
            tab.split(SplitDirection::Horizontal, shells_count);
            return;
        }

        if raw_input.eq_ignore_ascii_case(":split-v") {
            let shells_count = self.shells.len();
            tab.split(SplitDirection::Vertical, shells_count);
            return;
        }

        if raw_input.eq_ignore_ascii_case(":unsplit") {
            tab.close_split();
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

        if raw_input.starts_with(":ws") {
            self.spawn_ws_command(tab_id, cwd, raw_input);
            return;
        }

        if raw_input.eq_ignore_ascii_case(":report") || raw_input.eq_ignore_ascii_case(":bug") {
            let last_failed_idx = tab
                .blocks
                .iter()
                .rposition(|b| b.exit_code.map(|c| c != 0).unwrap_or(false))
                .or_else(|| if !tab.blocks.is_empty() { Some(tab.blocks.len() - 1) } else { None });

            if let Some(idx) = last_failed_idx {
                let blk = &tab.blocks[idx];
                let history = &tab.blocks[..idx];
                let os_info = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
                let report = generate_markdown_bug_report(blk, history, &os_info, "0.1.0");
                let new_block = CommandBlock::new_structured(
                    ":report",
                    "mrt-report",
                    cwd.display().to_string(),
                    report.clone(),
                    StructuredOutput::Markdown(report),
                    0,
                    0,
                );
                tab.blocks.push(new_block);
            } else {
                let new_block = CommandBlock::new_completed(
                    ":report",
                    "mrt-report",
                    cwd.display().to_string(),
                    "No commands in current tab history to report.".to_string(),
                    1,
                    0,
                );
                tab.blocks.push(new_block);
            }
            return;
        }

        if raw_input.starts_with(":diff") {
            let parts: Vec<&str> = raw_input.split_whitespace().collect();
            if parts.len() >= 3 {
                if let (Ok(u1), Ok(u2)) = (Uuid::parse_str(parts[1]), Uuid::parse_str(parts[2])) {
                    tab.comparison = Some((u1, u2));
                    return;
                }
            }

            if tab.blocks.len() >= 2 {
                let curr_id = tab.blocks[tab.blocks.len() - 1].id;
                let prev_id = tab.blocks[tab.blocks.len() - 2].id;
                tab.comparison = Some((prev_id, curr_id));
            } else {
                let new_block = CommandBlock::new_completed(
                    ":diff",
                    "mrt-diff",
                    cwd.display().to_string(),
                    "Need at least 2 blocks in history to perform a diff comparison.".to_string(),
                    1,
                    0,
                );
                tab.blocks.push(new_block);
            }
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

        let is_tui_command = if let Some(cmd) = raw_input.strip_prefix(":tui ") {
            Some(cmd.trim().to_string())
        } else {
            let first_token = raw_input
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_lowercase();
            let base_token = std::path::Path::new(&first_token)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&first_token);
            if matches!(
                base_token,
                "vim" | "vi" | "nvim" | "nano" | "htop" | "top" | "lazygit" | "lazydocker" | "less" | "man" | "fzf"
            ) {
                Some(raw_input.clone())
            } else {
                None
            }
        };

        if let Some(tui_cmd) = is_tui_command {
            match InteractivePty::spawn_with_command(&shell, &cwd, 120, 35, Some(&tui_cmd)) {
                Ok(pty) => {
                    self.active_tui = Some(ActiveTuiSession {
                        pty: std::sync::Arc::new(pty),
                        tab_id,
                        command: tui_cmd,
                        start_time: std::time::Instant::now(),
                    });
                }
                Err(e) => {
                    let err_block = CommandBlock::new_completed(
                        raw_input,
                        shell.name,
                        cwd.display().to_string(),
                        format!("Failed to launch TUI session: {e}"),
                        1,
                        0,
                    );
                    tab.blocks.push(err_block);
                }
            }
            return;
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

    fn spawn_ws_command(&mut self, tab_id: Uuid, cwd: PathBuf, cmd_str: String) {
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = std::time::Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match inspect_websocket(&cmd_str).await {
                Ok((structured, plain, _)) => CommandBlock::new_structured(
                    &cmd_str,
                    "mrt-ws",
                    cwd_str,
                    plain,
                    structured,
                    0,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => CommandBlock::new_completed(
                    &cmd_str,
                    "mrt-ws",
                    cwd_str,
                    format!("WebSocket error: {e}"),
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
            SidePanelEvent::RunWsInspector { url, msg } => {
                let cmd = if msg.trim().is_empty() {
                    format!(":ws {url}")
                } else {
                    format!(":ws {url} {}", msg.trim())
                };
                self.spawn_ws_command(tab_id, cwd, cmd);
            }
        }
    }

    fn process_tui_session(&mut self, ctx: &egui::Context) {
        let Some(ref session) = self.active_tui else {
            return;
        };

        ctx.request_repaint_after(std::time::Duration::from_millis(30));

        let mut close_session = false;
        let mut exit_code = 0;

        // Check if the process exited on its own
        if let Ok(Some(code)) = session.pty.try_wait_exit() {
            close_session = true;
            exit_code = code as i32;
        }

        // Check for manual exit via Ctrl+Q or forward keys to PTY
        let mut kill_requested = false;
        ctx.input(|i| {
            if i.modifiers.ctrl && i.key_pressed(Key::Q) {
                kill_requested = true;
                return;
            }

            for event in &i.events {
                forward_egui_event_to_pty(event, &session.pty);
            }
        });

        if kill_requested {
            let _ = session.pty.kill();
            close_session = true;
            exit_code = 130;
        }

        if close_session {
            if let Some(session) = self.active_tui.take() {
                let transcript = session.pty.read_transcript();
                let output = if transcript.is_empty() {
                    session.pty.drain_output()
                } else {
                    transcript
                };
                let duration_ms = session.start_time.elapsed().as_millis() as u64;

                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == session.tab_id) {
                    let shell_name = self
                        .shells
                        .get(tab.active_shell_idx)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| "default".into());
                    let block = CommandBlock::new_completed(
                        session.command,
                        shell_name,
                        tab.cwd.display().to_string(),
                        output,
                        exit_code,
                        duration_ms,
                    );
                    tab.blocks.push(block);
                }
            }
        }
    }

    fn render_tui_overlay(&mut self, ctx: &egui::Context) {
        let Some(ref session) = self.active_tui else {
            return;
        };

        let screen_rect = ctx.screen_rect();
        egui::Area::new(egui::Id::new("tui_overlay_modal"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen_rect,
                    0.0,
                    Color32::from_black_alpha(220),
                );

                let win_width = (screen_rect.width() * 0.94).max(300.0);
                let win_height = (screen_rect.height() * 0.90).max(200.0);

                egui::Window::new(format!("📺 Interactive TUI: {} (Ctrl+Q to exit)", session.command))
                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                    .fixed_size(egui::vec2(win_width, win_height))
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Alt-Screen PTY Running")
                                    .monospace()
                                    .color(Palette::ACCENT_BLUE),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .button(
                                        RichText::new("Exit Session (Ctrl+Q)")
                                            .color(Palette::ACCENT_RED),
                                    )
                                    .clicked()
                                {
                                    let _ = session.pty.kill();
                                }
                            });
                        });
                        ui.separator();

                        let transcript = session.pty.read_transcript();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false; 2])
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                let content = if transcript.is_empty() {
                                    "Starting terminal emulator session..."
                                } else {
                                    &transcript
                                };
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(content)
                                            .monospace()
                                            .size(12.0)
                                            .color(Palette::TEXT_PRIMARY),
                                    )
                                    .wrap(),
                                );
                            });
                    });
            });
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

fn forward_egui_event_to_pty(event: &egui::Event, pty: &InteractivePty) {
    match event {
        egui::Event::Text(text) => {
            let _ = pty.send_input(text, false);
        }
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            if modifiers.ctrl && !modifiers.alt {
                match key {
                    Key::C => {
                        let _ = pty.send_input("\x03", false);
                    }
                    Key::D => {
                        let _ = pty.send_input("\x04", false);
                    }
                    Key::Z => {
                        let _ = pty.send_input("\x1a", false);
                    }
                    Key::L => {
                        let _ = pty.send_input("\x0c", false);
                    }
                    Key::A => {
                        let _ = pty.send_input("\x01", false);
                    }
                    Key::E => {
                        let _ = pty.send_input("\x05", false);
                    }
                    Key::U => {
                        let _ = pty.send_input("\x15", false);
                    }
                    Key::W => {
                        let _ = pty.send_input("\x17", false);
                    }
                    _ => {}
                }
            } else if !modifiers.ctrl && !modifiers.alt && !modifiers.command {
                match key {
                    Key::Enter => {
                        let _ = pty.send_input(if cfg!(windows) { "\r\n" } else { "\n" }, false);
                    }
                    Key::Backspace => {
                        let _ = pty.send_input("\x08", false);
                    }
                    Key::Tab => {
                        let _ = pty.send_input("\t", false);
                    }
                    Key::Escape => {
                        let _ = pty.send_input("\x1b", false);
                    }
                    Key::ArrowUp => {
                        let _ = pty.send_input("\x1b[A", false);
                    }
                    Key::ArrowDown => {
                        let _ = pty.send_input("\x1b[B", false);
                    }
                    Key::ArrowRight => {
                        let _ = pty.send_input("\x1b[C", false);
                    }
                    Key::ArrowLeft => {
                        let _ = pty.send_input("\x1b[D", false);
                    }
                    Key::PageUp => {
                        let _ = pty.send_input("\x1b[5~", false);
                    }
                    Key::PageDown => {
                        let _ = pty.send_input("\x1b[6~", false);
                    }
                    Key::Home => {
                        let _ = pty.send_input("\x1b[H", false);
                    }
                    Key::End => {
                        let _ = pty.send_input("\x1b[F", false);
                    }
                    Key::Delete => {
                        let _ = pty.send_input("\x1b[3~", false);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

impl MrtGuiApp {
    fn tab_status(tab: &TerminalTab) -> TabStatus {
        if tab.blocks.iter().any(|b| b.exit_code.is_none()) {
            TabStatus::Running
        } else {
            match tab.blocks.last().and_then(|b| b.exit_code) {
                None => TabStatus::Idle,
                Some(0) => TabStatus::Succeeded,
                Some(_) => TabStatus::Failed,
            }
        }
    }

    fn open_new_tab(&mut self, shell_idx: usize) {
        let cwd = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.cwd.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let n = self.tabs.len() + 1;
        self.tabs.push(TerminalTab::new(
            format!("Tab {n} ({})", self.shells[shell_idx].kind.short_label()),
            shell_idx,
            cwd,
        ));
        self.active_tab = self.tabs.len() - 1;
    }

    /// Left sidebar: brand, new tab, tabs list, workspace panels and status card.
    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let compact = self.sidebar_collapsed;
        let mut new_tab_shell = None;
        let mut tab_to_close = None;
        let mut select_tab = None;
        let mut toggle_history = false;
        let mut toggle_runbooks = false;
        let new_tab_popup = egui::Id::new("mrt_new_tab_popup");

        egui::SidePanel::left("mrt_sidebar")
            .resizable(false)
            .exact_width(if compact { 60.0 } else { 264.0 })
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_SIDEBAR)
                    .inner_margin(egui::Margin::symmetric(if compact { 8.0 } else { 10.0 }, 14.0)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                // Brand row
                if compact {
                    ui.vertical_centered(|ui| {
                        brand_mark(ui, 28.0);
                        ui.add_space(6.0);
                        if icon_button(ui, "»", "Expand sidebar").clicked() {
                            self.sidebar_collapsed = false;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        brand_mark(ui, 26.0);
                        ui.label(
                            RichText::new("MRT")
                                .size(15.0)
                                .strong()
                                .color(Palette::TEXT_PRIMARY),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, "«", "Collapse sidebar").clicked() {
                                self.sidebar_collapsed = true;
                            }
                        });
                    });
                }
                ui.add_space(12.0);

                // New tab (popup listing every discovered shell)
                let new_tab = if compact {
                    nav_row(ui, "+", "New tab", None, false, true)
                } else {
                    let response = ui.add(
                        egui::Button::new(
                            RichText::new("+   New tab")
                                .size(14.0)
                                .color(Palette::TEXT_PRIMARY),
                        )
                        .fill(Color32::from_rgb(26, 26, 30))
                        .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                        .rounding(egui::Rounding::same(Radius::BUTTON))
                        .min_size(egui::vec2(ui.available_width(), 40.0)),
                    );
                    response
                };
                if new_tab.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(new_tab_popup));
                }
                egui::popup_below_widget(
                    ui,
                    new_tab_popup,
                    &new_tab,
                    egui::PopupCloseBehavior::CloseOnClick,
                    |ui| {
                        ui.set_min_width(220.0);
                        for (idx, shell) in self.shells.iter().enumerate() {
                            if ui
                                .add(
                                    egui::Button::new(format!(
                                        "{}   {}",
                                        shell.name, shell.program
                                    ))
                                    .frame(false)
                                    .min_size(egui::vec2(ui.available_width(), 30.0)),
                                )
                                .clicked()
                            {
                                new_tab_shell = Some(idx);
                            }
                        }
                    },
                );
                if nav_row(ui, "🔍", "Search history", None, self.show_history, compact).clicked() {
                    toggle_history = true;
                }

                // Tabs
                if compact {
                    ui.add_space(10.0);
                } else {
                    section_label(ui, "   Tabs");
                }
                let closable = self.tabs.len() > 1;
                for (idx, tab) in self.tabs.iter().enumerate() {
                    let shell = self
                        .shells
                        .get(tab.active_shell_idx)
                        .map(|s| s.kind.short_label())
                        .unwrap_or("shell");
                    let folder = tab
                        .cwd
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| tab.cwd.display().to_string());
                    let title = if tab.title.starts_with("Tab ") { folder.clone() } else { tab.title.clone() };
                    let status = Self::tab_status(tab);
                    let subtitle = if tab.title.starts_with("Tab ") {
                        format!("{shell} · {}", status.label())
                    } else {
                        format!("{shell} · {folder}")
                    };
                    let row = tab_row(
                        ui,
                        &title,
                        &subtitle,
                        status,
                        idx == self.active_tab,
                        closable,
                        compact,
                    );
                    if row.close {
                        tab_to_close = Some(tab.id);
                    } else if row.selected {
                        select_tab = Some(idx);
                    }
                }

                // Workspace panels
                if compact {
                    ui.add_space(10.0);
                } else {
                    section_label(ui, "   Workspace");
                }
                if nav_row(ui, "🗀", "Explorer", None, self.show_explorer, compact).clicked() {
                    self.show_explorer = !self.show_explorer;
                }
                if nav_row(ui, "☰", "Runbooks", None, self.runbooks_state.show_panel, compact).clicked() {
                    toggle_runbooks = true;
                }
                if nav_row(ui, "🕘", "History", None, self.show_history, compact).clicked() {
                    toggle_history = true;
                }
                if nav_row(ui, "🔌", "MCP Hub", None, self.side_state.show_mcp_panel, compact).clicked() {
                    self.side_state.show_mcp_panel = !self.side_state.show_mcp_panel;
                }
                if nav_row(ui, "🌐", "Internet Studio", None, self.side_state.show_internet_panel, compact).clicked() {
                    self.side_state.show_internet_panel = !self.side_state.show_internet_panel;
                }
                if nav_row(ui, "⊞", "Mosaic", None, self.mosaic, compact).clicked() {
                    self.mosaic = !self.mosaic;
                }

                // Status card pinned to the bottom
                if !compact {
                    const CARD_HEIGHT: f32 = 66.0;
                    let gap = ui.available_height() - CARD_HEIGHT;
                    if gap > 0.0 {
                        ui.add_space(gap);
                    } else {
                        ui.add_space(12.0);
                    }
                    egui::Frame::none()
                        .fill(Color32::from_rgb(26, 26, 30))
                        .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT))
                        .rounding(egui::Rounding::same(12.0))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").size(10.0).color(Palette::ACCENT_GREEN));
                                ui.label(RichText::new("Safety Gate on").size(13.0).color(Palette::TEXT_PRIMARY));
                            });
                            let bridge = match self.bridge_port {
                                Some(port) => format!("MCP bridge · port {port}"),
                                None => "MCP bridge offline".to_string(),
                            };
                            ui.label(RichText::new(bridge).size(12.0).color(Palette::TEXT_MUTED));
                        });
                }
            });

        if let Some(idx) = new_tab_shell {
            self.open_new_tab(idx);
        }
        if let Some(idx) = select_tab {
            self.active_tab = idx;
        }
        if let Some(id) = tab_to_close {
            self.close_terminal(id);
        }
        if toggle_history {
            self.show_history = !self.show_history;
            if self.show_history {
                self.refresh_history();
            }
        }
        if toggle_runbooks {
            self.runbooks_state.show_panel = !self.runbooks_state.show_panel;
            if self.runbooks_state.show_panel {
                self.refresh_runbooks();
                self.refresh_history();
            }
        }
    }

    /// Header: working directory, command palette pill and layout controls.
    fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("mrt_header")
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_BASE)
                    .inner_margin(egui::Margin::symmetric(20.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(tab) = self.tabs.get(self.active_tab) {
                        let full = tab.cwd.display().to_string();
                        let leaf = tab
                            .cwd
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| full.clone());
                        let parent = full.strip_suffix(&leaf).unwrap_or("").to_string();
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.label(RichText::new(parent).monospace().size(13.0).color(Palette::TEXT_SECONDARY))
                            .on_hover_text(&full);
                        ui.label(RichText::new(leaf).monospace().size(13.0).color(Palette::TEXT_PRIMARY))
                            .on_hover_text(&full);
                        ui.spacing_mut().item_spacing.x = 8.0;
                    }
                    if self.mosaic {
                        ui.add_space(10.0);
                        chip(ui, "Mosaic", Palette::BG_OVERLAY, Palette::TEXT_BODY);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.mosaic {
                            let shells_len = self.shells.len();
                            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                if tab.split_direction == SplitDirection::None {
                                    if icon_button(ui, "⊟", "Split vertically (:split-v)").clicked() {
                                        tab.split(SplitDirection::Vertical, shells_len);
                                    }
                                    if icon_button(ui, "‖", "Split horizontally (:split-h)").clicked() {
                                        tab.split(SplitDirection::Horizontal, shells_len);
                                    }
                                } else if icon_button(ui, "×", "Close split (:unsplit)").clicked() {
                                    tab.close_split();
                                }
                            }
                        }
                        ui.add_space(4.0);
                        let width = ui.available_width().clamp(160.0, 320.0);
                        if search_pill(ui, width, "Commands, tabs, tools…", "Ctrl K").clicked() {
                            self.show_command_palette = !self.show_command_palette;
                        }
                    });
                });
            });
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
        self.process_tui_session(ctx);

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

        self.render_sidebar(ctx);
        self.render_header(ctx);

        if self.show_explorer {
            self.render_explorer(ctx);
        }

        if self.show_history {
            let mut restore = None;
            let mut add_to_runbook: Option<crate::history::RunbookStep> = None;
            egui::SidePanel::right("history_panel").frame(panel_frame()).default_width(360.0).resizable(true).show(ctx, |ui| {
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
            egui::SidePanel::right("runbooks_panel").frame(panel_frame())
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
            egui::SidePanel::right("mrt_mcp_panel").frame(panel_frame())
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
            egui::SidePanel::right("mrt_internet_panel").frame(panel_frame())
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

        // Floating composer: AI suggestion card + command input card
        egui::TopBottomPanel::bottom("mrt_bottom_prompt")
            .show_separator_line(false)
            .frame(egui::Frame::none().fill(Palette::BG_BASE).inner_margin(egui::Margin {
                left: 20.0,
                right: 20.0,
                top: 6.0,
                bottom: 18.0,
            }))
            .show(ctx, |ui| {
                centered_column(ui, |ui| {
                    if !self.show_history {
                        if let Some(error) = &self.history_error {
                            ui.colored_label(Palette::ACCENT_RED, error);
                        }
                    }

                    // AI command proposal card
                    let mut run_ai_cmd = None;
                    let mut dismiss_ai = false;
                    if let Some(proposal) = &self.tabs[self.active_tab].ai_proposal {
                        egui::Frame::none()
                            .fill(Palette::AI_FILL)
                            .stroke(egui::Stroke::new(1.0_f32, Palette::AI_BORDER))
                            .rounding(egui::Rounding::same(14.0))
                            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("✨").size(14.0).color(Palette::ACCENT));
                                    ui.label(
                                        RichText::new("Suggested command")
                                            .size(13.0)
                                            .strong()
                                            .color(Palette::ACCENT_TEXT),
                                    );
                                    ui.label(
                                        RichText::new(&proposal.provider_used)
                                            .size(12.0)
                                            .color(Palette::TEXT_SECONDARY),
                                    );
                                });
                                if !proposal.explanation.trim().is_empty() {
                                    ui.label(
                                        RichText::new(&proposal.explanation)
                                            .size(13.5)
                                            .color(Palette::TEXT_BODY),
                                    );
                                }
                                ui.add_space(4.0);
                                egui::Frame::none()
                                    .fill(Palette::BG_INSET)
                                    .rounding(egui::Rounding::same(Radius::BUTTON))
                                    .inner_margin(egui::Margin { left: 12.0, right: 8.0, top: 6.0, bottom: 6.0 })
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(&proposal.proposed_command)
                                                    .monospace()
                                                    .size(13.0)
                                                    .color(Palette::TEXT_PRIMARY),
                                            );
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if ui.add(primary_button("Run")).clicked() {
                                                    run_ai_cmd = Some(proposal.proposed_command.clone());
                                                    dismiss_ai = true;
                                                }
                                                if ui.add(secondary_button("Dismiss")).clicked() {
                                                    dismiss_ai = true;
                                                }
                                            });
                                        });
                                    });
                            });
                        ui.add_space(10.0);
                    }
                    if dismiss_ai {
                        self.tabs[self.active_tab].ai_proposal = None;
                    }
                    if let Some(c) = run_ai_cmd {
                        self.submit_input(Some(c), false);
                    }

                    let mut should_submit = false;
                    let active_idx = self.active_tab;
                    let mosaic = self.mosaic;
                    let may_focus = focus_prompt
                        && !self.show_command_palette
                        && self.pending_safety.is_none()
                        && self.active_tui.is_none();
                    let tui_active = self.active_tui.is_some();
                    let shells = &self.shells;
                    if let Some(tab) = self.tabs.get_mut(active_idx) {
                        egui::Frame::none()
                            .fill(Palette::BG_ELEVATED)
                            .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_STRONG))
                            .rounding(egui::Rounding::same(Radius::COMPOSER))
                            .inner_margin(egui::Margin { left: 16.0, right: 12.0, top: 12.0, bottom: 10.0 })
                            .shadow(egui::epaint::Shadow {
                                offset: egui::vec2(0.0, 12.0),
                                blur: 32.0,
                                spread: 0.0,
                                color: Color32::from_black_alpha(90),
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if mosaic {
                                    ui.label(
                                        RichText::new(format!("Active · {}", tab.title))
                                            .size(12.0)
                                            .color(Palette::TEXT_MUTED),
                                    );
                                }
                                let response = ui.add(
                                    egui::TextEdit::singleline(&mut tab.input_line)
                                        .id(egui::Id::new(("command_input", tab.id)))
                                        .font(egui::FontId::monospace(14.5))
                                        .frame(false)
                                        .margin(egui::Margin::symmetric(0.0, 6.0))
                                        .hint_text(if tab.blocks.is_empty() {
                                            "Run a command — start with # to ask AI, Ctrl+K for everything else"
                                        } else {
                                            "Run a command — or start with # to ask AI"
                                        })
                                        .desired_width(f32::INFINITY),
                                );
                                if may_focus {
                                    response.request_focus();
                                }
                                if !tui_active && response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                    should_submit = true;
                                    response.request_focus();
                                }

                                // Up/Down history navigation
                                if response.has_focus() {
                                    if ui.input(|i| i.key_pressed(Key::ArrowUp)) && !tab.command_history.is_empty() {
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

                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    let current_shell_name = shells
                                        .get(tab.active_shell_idx)
                                        .map(|s| s.kind.short_label().to_string())
                                        .unwrap_or_else(|| "shell".into());
                                    egui::ComboBox::from_id_salt("active_shell_selector")
                                        .selected_text(
                                            RichText::new(format!("›_  {current_shell_name}"))
                                                .monospace()
                                                .size(12.0)
                                                .color(Palette::TEXT_BODY),
                                        )
                                        .show_ui(ui, |ui| {
                                            for (idx, s) in shells.iter().enumerate() {
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
                                                        active_idx + 1,
                                                        s.kind.short_label()
                                                    );
                                                }
                                            }
                                        });
                                    ui.add_space(6.0);
                                    ui.label(RichText::new("🛡").size(12.0).color(Palette::ACCENT_GREEN));
                                    ui.label(RichText::new("Safety Gate on").size(12.0).color(Palette::TEXT_MUTED));

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let has_input = !tab.input_line.trim().is_empty();
                                        let run = egui::Button::new(
                                            RichText::new("→")
                                                .size(17.0)
                                                .strong()
                                                .color(if has_input { Palette::ON_ACCENT } else { Palette::TEXT_MUTED }),
                                        )
                                        .fill(if has_input { Palette::ACCENT } else { Palette::BG_OVERLAY })
                                        .stroke(egui::Stroke::NONE)
                                        .rounding(egui::Rounding::same(17.0))
                                        .min_size(egui::vec2(34.0, 34.0));
                                        if ui.add_enabled(has_input, run).on_hover_text("Run command (Enter)").clicked() {
                                            response.request_focus();
                                            should_submit = true;
                                        }
                                    });
                                });
                            });
                    }

                    if should_submit {
                        self.submit_input(None, false);
                    }
                });
            });

        // Central Terminal Block Stream
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_BASE)
                    .inner_margin(egui::Margin { left: 20.0, right: 20.0, top: 12.0, bottom: 4.0 }),
            )
            .show(ctx, |ui| {
            if self.mosaic {
                self.render_mosaic(ui);
                return;
            }
            let mut pending_block_action = None;

            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if tab.split_direction == SplitDirection::Horizontal && tab.secondary_pane.is_some() {
                    ui.columns(2, |cols| {
                        // Left column: Primary pane
                        cols[0].group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Pane 1 (Primary)").strong().color(
                                    if tab.active_pane_idx == 0 { Palette::ACCENT_BLUE } else { Palette::TEXT_MUTED }
                                ));
                                if tab.active_pane_idx != 0 && ui.small_button("Focus").clicked() {
                                    tab.active_pane_idx = 0;
                                }
                            });
                            egui::ScrollArea::vertical()
                                .id_salt("split_pane_primary")
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
                                });
                        });

                        // Right column: Secondary pane
                        cols[1].group(|ui| {
                            let sec = tab.secondary_pane.as_mut().unwrap();
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Pane 2 (Split)").strong().color(
                                    if tab.active_pane_idx == 1 { Palette::ACCENT_GREEN } else { Palette::TEXT_MUTED }
                                ));
                                if tab.active_pane_idx != 1 && ui.small_button("Focus").clicked() {
                                    tab.active_pane_idx = 1;
                                }
                            });
                            egui::ScrollArea::vertical()
                                .id_salt("split_pane_secondary")
                                .auto_shrink([false, false])
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    if sec.blocks.is_empty() {
                                        ui.label(RichText::new("Secondary pane active. Commands run here when focused.").italics().color(Palette::TEXT_MUTED));
                                    }
                                    let mut previous_runs = HashMap::new();
                                    for block in &mut sec.blocks {
                                        let key = (block.command.clone(), block.shell_name.clone(), block.cwd.clone());
                                        let previous_id = previous_runs.insert(key, block.id);
                                        let filter = sec.table_filters.entry(block.id).or_default();
                                        if let Some(action) = render_command_block(ui, block, filter, previous_id) {
                                            pending_block_action = Some(action);
                                        }
                                        ui.add_space(8.0);
                                    }
                                });
                        });
                    });
                } else if tab.split_direction == SplitDirection::Vertical && tab.secondary_pane.is_some() {
                    let half_h = (ui.available_height() / 2.0 - 10.0).max(100.0);
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Pane 1 (Top)").strong().color(
                                if tab.active_pane_idx == 0 { Palette::ACCENT_BLUE } else { Palette::TEXT_MUTED }
                            ));
                            if tab.active_pane_idx != 0 && ui.small_button("Focus").clicked() {
                                tab.active_pane_idx = 0;
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_salt("split_v_primary")
                            .max_height(half_h)
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
                                    ui.add_space(6.0);
                                }
                            });
                    });

                    ui.separator();

                    ui.group(|ui| {
                        let sec = tab.secondary_pane.as_mut().unwrap();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Pane 2 (Bottom)").strong().color(
                                if tab.active_pane_idx == 1 { Palette::ACCENT_GREEN } else { Palette::TEXT_MUTED }
                            ));
                            if tab.active_pane_idx != 1 && ui.small_button("Focus").clicked() {
                                tab.active_pane_idx = 1;
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_salt("split_v_secondary")
                            .max_height(half_h)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                if sec.blocks.is_empty() {
                                    ui.label(RichText::new("Secondary pane active. Commands run here when focused.").italics().color(Palette::TEXT_MUTED));
                                }
                                let mut previous_runs = HashMap::new();
                                for block in &mut sec.blocks {
                                    let key = (block.command.clone(), block.shell_name.clone(), block.cwd.clone());
                                    let previous_id = previous_runs.insert(key, block.id);
                                    let filter = sec.table_filters.entry(block.id).or_default();
                                    if let Some(action) = render_command_block(ui, block, filter, previous_id) {
                                        pending_block_action = Some(action);
                                    }
                                    ui.add_space(6.0);
                                }
                            });
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| centered_column(ui, |ui| {
                            ui.add_space(8.0);
                            if tab.blocks.is_empty() {
                                ui.add_space((ui.available_height() * 0.28).max(48.0));
                                ui.vertical_centered(|ui| {
                                    brand_mark(ui, 40.0);
                                    ui.add_space(14.0);
                                    ui.label(RichText::new("Ready when you are").size(22.0).strong().color(Palette::TEXT_PRIMARY));
                                    ui.add_space(4.0);
                                    ui.label(RichText::new("Run any command below. Output lands here as blocks you can search, diff and diagnose.").size(14.0).color(Palette::TEXT_SECONDARY));
                                    ui.add_space(18.0);
                                    for (keys, hint) in [
                                        ("#", "Describe what you want and AI writes the command"),
                                        (":http  :dns  :ports  :web", "Built-in network tools"),
                                        ("Ctrl K", "Command palette"),
                                    ] {
                                        ui.horizontal(|ui| {
                                            let total = 420.0_f32.min(ui.available_width());
                                            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                                            chip(ui, keys, Palette::BG_OVERLAY, Palette::TEXT_BODY);
                                            ui.label(RichText::new(hint).size(13.0).color(Palette::TEXT_MUTED));
                                        });
                                    }
                                });
                            }
                            let mut previous_runs = HashMap::new();
                            for block in &mut tab.blocks {
                                let key = (block.command.clone(), block.shell_name.clone(), block.cwd.clone());
                                let previous_id = previous_runs.insert(key, block.id);
                                let filter = tab.table_filters.entry(block.id).or_default();
                                if let Some(action) = render_command_block(ui, block, filter, previous_id) {
                                    pending_block_action = Some(action);
                                }
                                ui.add_space(14.0);
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
                        }));
                }
            }

            if let Some(action) = pending_block_action {
                self.handle_block_action(action);
            }
        });

        // Safety Gate: anchored just above the composer, like an inline confirmation
        if self.pending_safety.is_some() {
            let mut approve = false;
            let mut cancel = false;

            egui::Area::new(egui::Id::new("safety_backdrop"))
                .fixed_pos(egui::Pos2::ZERO)
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    ui.painter()
                        .rect_filled(screen, 0.0, Color32::from_black_alpha(120));
                });

            if let Some(ref pending) = self.pending_safety {
                let critical = pending.assessment.risk == RiskLevel::Critical;
                let width = (ctx.screen_rect().width() - 80.0).clamp(320.0, 720.0);
                egui::Window::new("MRT Safety Gate")
                    .title_bar(false)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(width)
                    .min_width(width)
                    .max_width(width)
                    .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -150.0])
                    .frame(
                        egui::Frame::window(&ctx.style())
                            .fill(Palette::DANGER_FILL)
                            .stroke(egui::Stroke::new(1.0_f32, Palette::DANGER_BORDER))
                            .rounding(egui::Rounding::same(Radius::COMPOSER))
                            .inner_margin(egui::Margin::symmetric(22.0, 20.0)),
                    )
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            egui::Frame::none()
                                .fill(Color32::from_rgb(58, 38, 20))
                                .rounding(egui::Rounding::same(12.0))
                                .inner_margin(egui::Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.label(RichText::new("🛡").size(18.0).color(Color32::from_rgb(253, 186, 116)));
                                });
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(if critical {
                                            "This command can’t be undone"
                                        } else {
                                            "Check this command before it runs"
                                        })
                                        .size(16.0)
                                        .strong()
                                        .color(Palette::TEXT_PRIMARY),
                                    );
                                    let (fill, fg) = if critical {
                                        (Color32::from_rgb(74, 31, 31), Color32::from_rgb(252, 165, 165))
                                    } else {
                                        (Color32::from_rgb(66, 48, 18), Color32::from_rgb(253, 224, 138))
                                    };
                                    chip(ui, pending.assessment.risk.label(), fill, fg);
                                });
                                ui.label(
                                    RichText::new(&pending.assessment.reason)
                                        .size(14.0)
                                        .color(Color32::from_rgb(214, 206, 200)),
                                );
                            });
                        });
                        ui.add_space(12.0);
                        egui::Frame::none()
                            .fill(Palette::BG_INSET)
                            .rounding(egui::Rounding::same(12.0))
                            .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(
                                    RichText::new(&pending.assessment.command)
                                        .monospace()
                                        .size(13.5)
                                        .color(Palette::TEXT_PRIMARY),
                                );
                            });
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("{} · {}", pending.shell.name, pending.cwd.display()))
                                .size(12.0)
                                .monospace()
                                .color(Color32::from_rgb(189, 179, 172)),
                        );
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Esc to cancel").size(12.0).color(Color32::from_rgb(189, 179, 172)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let run = egui::Button::new(
                                    RichText::new("Run anyway")
                                        .strong()
                                        .color(Color32::from_rgb(31, 10, 10)),
                                )
                                .fill(Palette::ACCENT_RED)
                                .stroke(egui::Stroke::NONE)
                                .rounding(egui::Rounding::same(Radius::BUTTON))
                                .min_size(egui::vec2(0.0, 36.0));
                                if ui.add(run).clicked() {
                                    approve = true;
                                }
                                if ui
                                    .add(secondary_button("Cancel").min_size(egui::vec2(0.0, 36.0)))
                                    .clicked()
                                {
                                    cancel = true;
                                }
                            });
                        });
                    });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
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

        // Universal Command Palette (Ctrl+K / Ctrl+P)
        if self.show_command_palette {
            let mut close_palette = false;
            let mut cmd_to_run = None;
            let mut switch_shell_idx = None;

            egui::Area::new(egui::Id::new("palette_backdrop"))
                .fixed_pos(egui::Pos2::ZERO)
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    ui.painter()
                        .rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(8, 8, 10, 160));
                });

            let width = (ctx.screen_rect().width() - 48.0).clamp(300.0, 640.0);
            egui::Window::new("Command palette")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .default_width(width)
                    .min_width(width)
                    .max_width(width)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 96.0])
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(Palette::BG_ELEVATED)
                        .rounding(egui::Rounding::same(Radius::COMPOSER))
                        .inner_margin(egui::Margin::same(0.0)),
                )
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // Search row
                    let mut enter_pressed = false;
                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(18.0, 14.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("🔍").size(15.0).color(Palette::TEXT_SECONDARY));
                                let search = ui.add(
                                    egui::TextEdit::singleline(&mut self.palette_query)
                                        .font(egui::FontId::proportional(16.0))
                                        .frame(false)
                                        .hint_text("Search commands, tools and shells…")
                                        .desired_width(ui.available_width() - 50.0),
                                );
                                if !palette_was_open {
                                    search.request_focus();
                                }
                                enter_pressed = search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                                chip(ui, "Esc", Palette::BG_OVERLAY, Palette::TEXT_SECONDARY);
                            });
                        });
                    ui.painter().hline(
                        ui.min_rect().x_range(),
                        ui.min_rect().bottom(),
                        egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT),
                    );

                    let q = self.palette_query.trim().to_lowercase();
                    let matches = |title: &str, hint: &str| {
                        q.is_empty() || title.to_lowercase().contains(&q) || hint.to_lowercase().contains(&q)
                    };
                    let mut first_cmd: Option<String> = None;
                    let mut first_shell: Option<usize> = None;
                    let mut results = 0usize;

                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let sections: [(&str, &str, &[(&str, &str)]); 3] = [
                                    (
                                        "Layout",
                                        "‖",
                                        &[
                                            ("Split pane horizontally", ":split-h"),
                                            ("Split pane vertically", ":split-v"),
                                            ("Unsplit", ":unsplit"),
                                            ("Clear terminal blocks", ":clear"),
                                        ],
                                    ),
                                    (
                                        "Tools",
                                        "🌐",
                                        &[
                                            ("Scan listening TCP/UDP ports", ":ports"),
                                            ("DNS and latency trace", ":dns google.com"),
                                            ("HTTP GET sample JSON API", ":http GET https://httpbin.org/json"),
                                            ("Read a web page as Markdown", ":web https://www.rust-lang.org"),
                                            ("Test the Safety Gate", "rm -rf ./important_production_data"),
                                        ],
                                    ),
                                    (
                                        "Ask AI",
                                        "✨",
                                        &[
                                            ("Show system info", "# show system info"),
                                            ("List all files in this folder", "# list all files in directory"),
                                            ("Show running processes", "# show running processes"),
                                        ],
                                    ),
                                ];
                                for (section, icon, items) in sections {
                                    let visible: Vec<_> = items.iter().filter(|(t, c)| matches(t, c)).collect();
                                    if visible.is_empty() {
                                        continue;
                                    }
                                    section_label(ui, &format!("   {section}"));
                                    for (title, cmd) in visible {
                                        results += 1;
                                        if first_cmd.is_none() && first_shell.is_none() {
                                            first_cmd = Some(cmd.to_string());
                                        }
                                        if palette_row(ui, icon, title, cmd).clicked() {
                                            cmd_to_run = Some(cmd.to_string());
                                            close_palette = true;
                                        }
                                    }
                                }
                                let shell_rows: Vec<_> = self
                                    .shells
                                    .iter()
                                    .enumerate()
                                    .map(|(idx, s)| (idx, format!("Switch shell to {}", s.name), s.program.clone()))
                                    .filter(|(_, title, program)| matches(title, program))
                                    .collect();
                                if !shell_rows.is_empty() {
                                    section_label(ui, "   Shells");
                                    for (idx, title, program) in shell_rows {
                                        results += 1;
                                        if first_cmd.is_none() && first_shell.is_none() {
                                            first_shell = Some(idx);
                                        }
                                        if palette_row(ui, "›_", &title, &program).clicked() {
                                            switch_shell_idx = Some(idx);
                                            close_palette = true;
                                        }
                                    }
                                }
                                if results == 0 {
                                    ui.add_space(18.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(RichText::new("No matches. Press Enter to run it as a command.").size(13.0).color(Palette::TEXT_MUTED));
                                    });
                                    ui.add_space(18.0);
                                }
                                ui.add_space(4.0);
                            });
                        });

                    if enter_pressed {
                        if let Some(cmd) = first_cmd.take() {
                            cmd_to_run = Some(cmd);
                        } else if let Some(idx) = first_shell {
                            switch_shell_idx = Some(idx);
                        } else if !q.is_empty() {
                            cmd_to_run = Some(self.palette_query.trim().to_string());
                        }
                        close_palette = true;
                    }

                    // Footer hints
                    let top = ui.min_rect().bottom();
                    ui.painter().hline(
                        ui.min_rect().x_range(),
                        top,
                        egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT),
                    );
                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(18.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 18.0;
                                for hint in ["Enter  Run first match", "Click  Run", "Esc  Close"] {
                                    ui.label(RichText::new(hint).size(12.0).color(Palette::TEXT_MUTED));
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format!("{results} results"))
                                            .size(12.0)
                                            .color(Palette::TEXT_MUTED),
                                    );
                                });
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
                self.palette_query.clear();
                self.submit_input(Some(cmd), false);
            }
            if close_palette || ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_command_palette = false;
                if self.pending_safety.is_none() {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new(("command_input", self.tabs[self.active_tab].id))));
                }
            }
        }

        self.render_tui_overlay(ctx);
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

    #[test]
    fn split_and_unsplit_tab_lifecycle() {
        let mut app = MrtGuiApp::new();
        assert_eq!(app.tabs[0].split_direction, SplitDirection::None);
        assert!(app.tabs[0].secondary_pane.is_none());

        // Split horizontal
        app.submit_input(Some(":split-h".to_string()), false);
        assert_eq!(app.tabs[0].split_direction, SplitDirection::Horizontal);
        assert!(app.tabs[0].secondary_pane.is_some());

        // Split vertical
        app.submit_input(Some(":split-v".to_string()), false);
        assert_eq!(app.tabs[0].split_direction, SplitDirection::Vertical);
        assert!(app.tabs[0].secondary_pane.is_some());

        // Unsplit
        app.submit_input(Some(":unsplit".to_string()), false);
        assert_eq!(app.tabs[0].split_direction, SplitDirection::None);
        assert!(app.tabs[0].secondary_pane.is_none());
    }

    #[test]
    fn tui_session_spawns_and_closes_into_block() {
        let mut app = MrtGuiApp::new();
        assert!(app.active_tui.is_none());

        // Launch TUI command
        app.submit_input(Some(":tui echo interactive_tui_test".to_string()), false);
        assert!(app.active_tui.is_some());

        let ctx = egui::Context::default();

        // Run process_tui_session until child completes or kill it
        let start = std::time::Instant::now();
        while app.active_tui.is_some() && start.elapsed() < std::time::Duration::from_secs(5) {
            app.process_tui_session(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }

        // If still alive, terminate cleanly
        if let Some(ref session) = app.active_tui {
            let _ = session.pty.kill();
            app.process_tui_session(&ctx);
        }

        assert!(app.active_tui.is_none());
        assert!(!app.tabs[0].blocks.is_empty());
        let last_blk = app.tabs[0].blocks.last().unwrap();
        assert!(last_blk.command.contains("interactive_tui_test"));
    }
}
