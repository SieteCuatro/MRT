use crate::keymap::{Action, KeyChord, KeyContext, KeyResolver, Keymap, Platform, Resolution, PALETTE_ACTIONS};
use crate::theme::{
    apply_mrt_theme, chip, ghost_button, icon_button, panel_frame, primary_button, secondary_button, section_label,
    Palette, Radius,
};
use crate::widgets::activity::{activity_indicator, is_animated};
use crate::widgets::block_view::{render_command_block, render_compact_block, BlockAction};
use crate::widgets::mosaic::{mosaic_rects, MosaicLayout};
use crate::widgets::chrome::{
    brand_mark, centered_column, nav_row, palette_row, search_pill, tab_row,
};
use crate::widgets::runbooks::{
    render_runbooks_panel, ReplayStatusView, RunbookAction, RunbooksUiState,
};
use crate::widgets::side_panels::{render_mcp_side_panel, SidePanelEvent, SidePanelState};
use crate::widgets::task_manager::TaskManagerEvent;
use crate::widgets::toolbox::{render_toolbox_side_panel, ToolboxTab};
use egui::{Color32, Key, RichText};
use mrt_ai::{
    diagnose_command_error_streaming, translate_nl_to_command,
    AiCommandProposal,
};
use mrt_core::{
    ActivitySnapshot, ActivityTracker, assess_command, discover_shells, execute_command_streaming_with_pid, CommandBlock,
    InteractivePty, RiskLevel, SafetyAssessment, ShellProfile, StructuredOutput, TerminalEmulator,
};
use mrt_mcp::{
    start_gui_bridge_listener, parse_http_headers, GuiUiCommand, McpHostManager, McpServerConfig,
    McpServerStatus, McpTabSession, McpTransportKind, MrtMcpServerState,
};
use mrt_tools::{
    execute_http_request, fetch_url_as_markdown, generate_markdown_bug_report, inspect_dns,
    inspect_listening_ports, inspect_process, inspect_websocket, is_process_block,
    kill_outcomes_markdown, parse_http_command, parse_kill_command, parse_ps_command,
    parse_watch_command, plan_kill, process_details_markdown, processes_to_table,
    query_processes, KillPlan, KillRequest, OwnedProcess, OwnedProcessKind,
    OwnedProcessMap, PortEntry, ProcessQuery, ProcessSelector, ProcessSnapshot, ProcessWatch,
    SharedProcessSampler, PROCESS_BLOCK_SHELL,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How often the Task Manager, live `:top` blocks and `:watch-proc` watches resample.
const PROCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

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
    /// Running / progress / waiting-for-input / finished state shown on the tab.
    pub activity: ActivityTracker,
    /// `ssh` arguments when this is a remote tab (`:ssh user@host`): its commands run
    /// on that host through `shell_pool::run_remote`, never locally.
    pub remote: Option<Vec<String>>,
    /// Keys typed into one pane's program go to the programs of every pane (`:broadcast`).
    pub broadcast: bool,
    /// Agent sandbox tab (`:sandbox`): commands always start in this folder, and every
    /// command the Safety Gate does not rate Safe waits for approval, whatever the config.
    pub sandbox_root: Option<PathBuf>,
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
            activity: ActivityTracker::new(),
            remote: None,
            broadcast: false,
            sandbox_root: None,
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
    /// Periodic process sample for the Task Manager, live `:top` blocks and watches.
    /// Background events do not count towards `busy_count`.
    ProcessSnapshot(Result<Arc<ProcessSnapshot>, String>),
    /// A `:kill` request (prompt, Task Manager or MCP) was resolved against a fresh snapshot
    /// and is ready for the Safety Approval Gate.
    KillPlanned {
        tab_id: Uuid,
        cwd: PathBuf,
        plan: Result<KillPlan, String>,
    },
    /// The block "Stop" button finished stopping a command's process tree.
    StopFinished {
        tab_id: Uuid,
        error: Option<String>,
    },
}

/// Returns the arguments of `input` when its first word is exactly `name`
/// (so `:ps` matches `:ps node` but not `:psql`).
fn command_args<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(name)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

impl AsyncWorkerEvent {
    fn is_background(&self) -> bool {
        matches!(self, AsyncWorkerEvent::ProcessSnapshot(_))
    }
}

/// Processes approved for stopping once the user confirms the Safety Gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingKill {
    pub pids: Vec<u32>,
    pub force: bool,
    pub tree: bool,
}

pub struct PendingSafetyApproval {
    pub tab_id: Uuid,
    pub assessment: SafetyAssessment,
    pub shell: ShellProfile,
    pub cwd: PathBuf,
    /// `Some` when the approval stops processes instead of running a shell command.
    pub kill: Option<PendingKill>,
}

/// A `:top` block that is refreshed in place until stopped.
pub struct LiveTopBlock {
    pub tab_id: Uuid,
    pub block_id: Uuid,
    pub query: ProcessQuery,
}

pub struct ActiveProcessWatch {
    pub tab_id: Uuid,
    pub watch: ProcessWatch,
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
    /// The screen the program draws on, fed with the PTY's output every frame.
    pub term: TerminalEmulator,
    /// History (with colors) moved out of `term` before Windows resizes; see
    /// `render_tui_overlay`.
    pub saved_history: Vec<String>,
    /// Mouse state for reports to the program: the button held since a press over the
    /// grid, the last cell reported (motion is only sent when it changes), and the
    /// part of a wheel notch not sent yet.
    pub mouse_held: Option<crate::term_input::MouseButton>,
    pub mouse_cell: Option<(u16, u16)>,
    pub wheel_accum: f32,
    /// Text selected by dragging (when the program does not take the mouse, or with
    /// Shift), and whether a selection drag is in progress.
    pub selection: Option<crate::widgets::term_view::Selection>,
    pub selecting: bool,
    /// `None` for the full-screen overlay; `Some(0)` / `Some(1)` for a session drawn
    /// inside the primary / secondary pane of a split tab.
    pub pane: Option<usize>,
}

/// Which TUI session receives the keyboard this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedTui {
    Overlay,
    /// Index into `MrtGuiApp::pane_tuis`.
    Pane(usize),
}

/// Lines of history the TUI overlay's emulator keeps.
const TUI_SCROLLBACK_LINES: usize = 5_000;
/// The PTY size a TUI session starts with, before the overlay has measured its area.
const TUI_INITIAL_SIZE: (u16, u16) = (120, 35);

impl ActiveTuiSession {
    fn new(pty: InteractivePty, tab_id: Uuid, command: String, pane: Option<usize>, cols: u16, rows: u16) -> Self {
        Self {
            pty: std::sync::Arc::new(pty),
            tab_id,
            command,
            start_time: std::time::Instant::now(),
            term: TerminalEmulator::new(cols, rows, TUI_SCROLLBACK_LINES),
            saved_history: Vec::new(),
            mouse_held: None,
            mouse_cell: None,
            wheel_accum: 0.0,
            selection: None,
            selecting: false,
            pane,
        }
    }

    /// Feeds new PTY output into the emulator, flushes an expired synchronized update
    /// and answers the program's queries. Returns the new output (for the activity
    /// tracker).
    fn pump_output(&mut self) -> String {
        let fresh = self.pty.drain_output();
        if !fresh.is_empty() {
            self.term.feed(fresh.as_bytes());
        }
        if self.term.sync_deadline().is_some_and(|deadline| Instant::now() >= deadline) {
            self.term.flush_sync();
        }
        // Answers to the program's queries (device attributes, cursor position, size).
        let replies = self.term.take_replies();
        if !replies.is_empty() {
            let _ = self.pty.send_bytes(&replies);
        }
        fresh
    }

    /// Sends this frame's keyboard input to the program. Keys bound to MRT actions
    /// (e.g. `exit_tui`) were already removed by `handle_keybindings`.
    fn forward_keys(&mut self, ctx: &egui::Context) -> Vec<u8> {
        let modes = self.term.modes();
        // egui-winit turns Ctrl+C (Cmd+C on macOS) into `Event::Copy` with no key event.
        // It copies the selection when there is one, with Ctrl+Shift+C, and on macOS,
        // as in Windows Terminal; otherwise it reaches the program as ^C (`encode_event`).
        let has_selection = self.selection.is_some();
        let (events, copy) = ctx.input(|i| {
            let shift = i.modifiers.shift;
            let is_copy = |e: &egui::Event| {
                matches!(e, egui::Event::Copy) && (cfg!(target_os = "macos") || shift || has_selection)
            };
            let copy = i.events.iter().any(is_copy);
            let events: Vec<egui::Event> = i.events.iter().filter(|e| !is_copy(e)).cloned().collect();
            (events, copy)
        });
        if copy {
            if let Some(selection) = self.selection.take() {
                let text = crate::widgets::term_view::selection_text(&self.term.snapshot(), &selection);
                ctx.copy_text(text);
            }
        }
        let bytes = crate::term_input::encode_events(&events, &modes);
        if !bytes.is_empty() {
            // Typing replaces the selection, as in other terminals.
            self.selection = None;
            let _ = self.pty.send_bytes(&bytes);
        }
        bytes
    }

    /// Draws the terminal into the space `ui` has left, resizing the emulator and the
    /// PTY to the whole cells that fit. A `focused` surface takes keyboard focus and the
    /// mouse; an unfocused one only shows the program.
    fn draw_surface(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, focused: bool, window_focused: bool) -> egui::Response {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let cell = crate::widgets::term_view::cell_size(ui, &font);
        // Only the visible part counts: inside a pane group the available width can run
        // past the clip rect, which would cut off the last column.
        let clip = ui.clip_rect();
        let origin = ui.cursor().min;
        let visible = egui::vec2(
            ui.available_width().min(clip.right() - origin.x),
            ui.available_height().min(clip.bottom() - origin.y),
        );
        let (cols, rows) = crate::widgets::term_view::grid_dims(visible, cell);
        if (cols, rows) != (self.term.cols(), self.term.rows()) {
            // portable-pty opens ConPTY with PSEUDOCONSOLE_RESIZE_QUIRK: ConPTY
            // does not repaint after a resize and expects the terminal to resize
            // its copy the way conhost does, adding blank rows at the bottom.
            // alacritty instead pulls lines back from history when the screen
            // grows, which shifts the screen and leaves stale rows. Move the
            // history out first (it is kept for the command block).
            if cfg!(windows) && self.term.scrollback_len() > 0 {
                let history = self.term.scrollback_with_colors();
                self.saved_history.extend(history);
                self.term.clear_scrollback();
            }
            self.term.resize(cols, rows);
            self.term.set_cell_size_px(cell.x.round() as u16, cell.y.round() as u16);
            let _ = self.pty.resize(cols, rows);
        }
        let snapshot = self.term.snapshot();
        let response =
            crate::widgets::term_view::paint_terminal(
                ui,
                egui::Id::new(("tui_surface", self.tab_id, self.pane)),
                &snapshot,
                &font,
                cell,
                focused && window_focused,
            );
        // Screen readers (AccessKit) get the visible screen as text; the closure only
        // runs when accessibility is active.
        let term = &self.term;
        let title = self.command.clone();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("Terminal: {title}\n{}", term.screen_text()))
        });
        if focused {
            // Keep keyboard focus on the terminal so the command input does not also
            // receive the keys, and so Tab, arrows and Esc stay with the program
            // instead of moving egui's focus.
            response.request_focus();
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            forward_tui_mouse(ctx, self, response.rect, cell, &snapshot.modes);
        }
        if let Some(selection) = &self.selection {
            crate::widgets::term_view::paint_selection(ui, response.rect, cell, snapshot.cols, snapshot.rows, selection);
        }
        response
    }

    /// Text to store in the command block when the session ends: what the terminal
    /// shows (history plus the main screen) with its colors. The raw transcript is
    /// mostly cursor movement (full-screen programs, and ConPTY on Windows), which
    /// would turn into blank lines in a block.
    fn final_output(&self) -> String {
        let current = self.term.text_with_colors();
        if self.saved_history.is_empty() {
            return current;
        }
        let mut text = self.saved_history.join("\n");
        if !current.is_empty() {
            text.push('\n');
            text.push_str(&current);
        }
        text
    }
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
    /// TUI sessions running inside split panes (`ActiveTuiSession::pane` is `Some`).
    pub pane_tuis: Vec<ActiveTuiSession>,
    /// Resolves key presses into actions (configurable keybindings).
    pub(crate) key_resolver: KeyResolver,
    /// Problems found while loading keybinding overrides, shown in the palette.
    pub(crate) keymap_issues: Vec<String>,
    /// Set by the `exit_tui` action; handled by `process_tui_session`.
    tui_exit_requested: bool,
    history: Option<crate::history::HistoryWorker>,
    history_error: Option<String>,
    history_pending: usize,
    show_history: bool,
    history_query: String,
    history_results: Vec<CommandBlock>,
    runbooks_state: RunbooksUiState,
    active_replay: Option<ActiveRunbookReplay>,
    running_output: HashMap<Uuid, (Uuid, std::sync::Arc<std::sync::Mutex<String>>)>,
    /// Parser state of each streaming block, so a chunk is parsed once (O(n) in total).
    vt_streams: HashMap<Uuid, mrt_core::VtStream>,
    pub(crate) mosaic: bool,
    mosaic_columns: usize,
    mosaic_layout: MosaicLayout,
    show_explorer: bool,
    explorer_path: Option<PathBuf>,
    explorer_entries: Result<Vec<(PathBuf, bool)>, String>,
    sidebar_collapsed: bool,
    /// Shared with the MCP server so both see the same CPU baselines.
    process_sampler: SharedProcessSampler,
    /// OS PID of the shell behind each running command block.
    running_pids: Arc<Mutex<HashMap<Uuid, u32>>>,
    /// Each tab's persistent shell (`shell_pool.rs`).
    pub(crate) shell_pool: crate::shell_pool::ShellPool,
    live_top: Option<LiveTopBlock>,
    process_watches: Vec<ActiveProcessWatch>,
    next_watch_id: usize,
    process_tick_last: Option<Instant>,
    process_tick_in_flight: bool,
    /// Sample on the next frame even if no live view needs it (Refresh, `:top`, PID links).
    force_process_tick: bool,
    /// Tab whose next `:ps` block should be diffed against its previous `:ps` block.
    pending_ps_compare: Option<Uuid>,
    /// The loaded configuration file and its watcher (see `config_runtime.rs`).
    pub config_state: crate::config_runtime::ConfigState,
    /// Desktop notifications and sidebar badges for commands that finish in the background.
    pub(crate) notifier: crate::notifications::CommandNotifier,
    /// Find in output (Ctrl+F).
    pub(crate) find: crate::find::FindState,
    /// Where the layout is saved (`None` in tests or with `terminal.restore_layout = false`).
    pub(crate) layout_file: Option<PathBuf>,
    /// The layout last written, so unchanged layouts are not written again.
    pub(crate) layout_saved: Option<crate::layout::SavedLayout>,
    pub(crate) layout_next_check: Instant,
    /// Keyboard selection in the active tab's output, while copy mode is on.
    pub(crate) copy_mode: Option<crate::copy_mode::CopyMode>,
    /// A window action from `mrt quick` / `mrt_control_gui` (`show`, `hide`, `toggle`),
    /// applied in the next frame.
    pub(crate) pending_window: Option<String>,
}

/// The egui context, set once the window exists, so background threads can wake
/// the UI ([`wake_ui`]) instead of the UI polling on a timer.
static UI_CONTEXT: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// Remembers the window's egui context for [`wake_ui`].
pub fn set_ui_context(ctx: &egui::Context) {
    let _ = UI_CONTEXT.set(ctx.clone());
}

/// Asks for a frame now, from any thread (no-op before the window exists and in tests).
pub fn wake_ui() {
    if let Some(ctx) = UI_CONTEXT.get() {
        ctx.request_repaint();
    }
}

/// A channel whose receiver wakes the UI for every message: a thread forwards
/// messages from the returned sender to the returned receiver and calls
/// [`wake_ui`]. The thread ends when every sender is dropped. Unit tests use a
/// plain channel, because they send and then read in the same step.
fn waking_channel<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    if cfg!(test) {
        channel()
    } else {
        forwarding_channel()
    }
}

fn forwarding_channel<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let (tx, raw_rx) = channel::<T>();
    let (fwd_tx, rx) = channel::<T>();
    std::thread::Builder::new()
        .name("mrt-ui-wake".into())
        .spawn(move || {
            while let Ok(message) = raw_rx.recv() {
                if fwd_tx.send(message).is_err() {
                    break;
                }
                wake_ui();
            }
        })
        .expect("spawn UI wake thread");
    (tx, rx)
}

/// Frame interval when nothing runs: only the config file watcher and relative
/// times need it. Events from workers and the MCP bridge wake the UI at once.
pub const IDLE_REPAINT: std::time::Duration = std::time::Duration::from_secs(1);

impl MrtGuiApp {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime initialization");
        let shells = discover_shells();
        let initial_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let (gui_cmd_tx, gui_cmd_rx) = waking_channel();
        let mut mcp_state = MrtMcpServerState::default();
        mcp_state.gui_tx = Some(gui_cmd_tx);

        // Unit tests never read the user's or the repository's config files.
        let config_state = if cfg!(test) {
            crate::config_runtime::ConfigState::load(mrt_core::ConfigSources::default(), false)
        } else {
            crate::config_runtime::ConfigState::load(mrt_core::ConfigSources::discover(&initial_cwd), true)
        };
        let first_shell_idx = crate::config_runtime::default_shell_index(&shells, &config_state.config);
        let first_shell = &shells[first_shell_idx];
        let first_tab = TerminalTab::new(
            format!("Tab 1 ({})", first_shell.kind.short_label()),
            first_shell_idx,
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
                activity: None,
                sandbox_root: None,
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
            .map(|endpoint| endpoint.addr.port());

        let process_sampler = mcp_state.process_sampler.clone();
        let mcp_host = McpHostManager::new(mcp_state);

        let (tx, rx) = waking_channel();
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

        let (key_resolver, keymap_issues) = load_key_resolver(config_state.config.keybindings());

        let mut app = Self {
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
            pane_tuis: Vec::new(),
            key_resolver,
            keymap_issues,
            tui_exit_requested: false,
            history,
            history_error,
            history_pending: 0,
            show_history: false,
            history_query: String::new(),
            history_results: Vec::new(),
            runbooks_state: RunbooksUiState::default(),
            active_replay: None,
            running_output: HashMap::new(),
            vt_streams: HashMap::new(),
            mosaic: false,
            mosaic_columns: 3,
            mosaic_layout: MosaicLayout::Grid,
            show_explorer: false,
            explorer_path: None,
            explorer_entries: Ok(Vec::new()),
            sidebar_collapsed: false,
            process_sampler,
            running_pids: Arc::new(Mutex::new(HashMap::new())),
            // Unit tests run one process per command unless a test opts in.
            shell_pool: crate::shell_pool::ShellPool::new(
                !cfg!(test) && config_state.config.terminal.persistent_shell,
            ),
            live_top: None,
            process_watches: Vec::new(),
            next_watch_id: 1,
            process_tick_last: None,
            process_tick_in_flight: false,
            force_process_tick: false,
            pending_ps_compare: None,
            notifier: crate::notifications::CommandNotifier::new(),
            find: crate::find::FindState::default(),
            config_state,
            layout_file: None,
            layout_saved: None,
            layout_next_check: Instant::now(),
            copy_mode: None,
            pending_window: None,
        };
        app.apply_config_side_effects();
        if !cfg!(test) && app.config_state.config.terminal.restore_layout {
            app.layout_file = crate::layout::layout_path();
            if let Some(layout) = app.layout_file.as_deref().and_then(crate::layout::load) {
                let fallback = app.tabs[0].cwd.clone();
                app.restore_layout(&layout, &fallback);
                app.layout_saved = Some(layout);
            }
        }
        app
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
                        let report = generate_markdown_bug_report(blk, history, &os_info, env!("CARGO_PKG_VERSION"));
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
            BlockAction::InspectProcess(pid) => {
                self.open_task_manager(Some(pid));
            }
            BlockAction::StopBlock(block_id) => {
                self.stop_block(block_id);
            }
        }
    }

    // ---------------------------------------------------------------------
    // Task Manager and process tools (`:ps`, `:top`, `:proc`, `:kill`, `:watch-proc`)
    // ---------------------------------------------------------------------

    fn open_task_manager(&mut self, focus: Option<u32>) {
        self.side_state.show_toolbox_panel = true;
        self.side_state.toolbox_tab = ToolboxTab::TaskManager;
        if let Some(pid) = focus {
            self.side_state.task_manager.focus_pid(pid);
        }
        self.force_process_tick = true;
    }

    fn wants_process_ticks(&self) -> bool {
        let tm = &self.side_state.task_manager;
        let task_manager_visible =
            self.side_state.show_toolbox_panel && self.side_state.toolbox_tab == ToolboxTab::TaskManager;
        self.live_top.is_some()
            || !self.process_watches.is_empty()
            || (task_manager_visible && (tm.live || tm.snapshot.is_none()))
    }

    fn maybe_spawn_process_tick(&mut self) {
        if self.process_tick_in_flight {
            return;
        }
        let due = self
            .process_tick_last
            .map(|t| t.elapsed() >= PROCESS_REFRESH_INTERVAL)
            .unwrap_or(true);
        if !(self.force_process_tick || (due && self.wants_process_ticks())) {
            return;
        }
        self.force_process_tick = false;
        self.process_tick_in_flight = true;
        self.side_state.task_manager.refreshing = true;
        let sampler = self.process_sampler.clone();
        let tx = self.tx.clone();
        self.runtime.spawn(async move {
            let result = sampler.snapshot().await.map(Arc::new).map_err(|e| e.to_string());
            let _ = tx.send(AsyncWorkerEvent::ProcessSnapshot(result));
        });
    }

    /// Processes MRT started: running command blocks, the TUI overlay, and MCP PTYs.
    fn owned_processes(&self) -> OwnedProcessMap {
        let mut map: OwnedProcessMap = self
            .mcp_host
            .local_server_state
            .owned_processes
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|(_, o)| o.kind == OwnedProcessKind::Pty)
                    .map(|(pid, o)| (*pid, o.clone()))
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(pids) = self.running_pids.lock() {
            for (block_id, pid) in pids.iter() {
                let found = self.tabs.iter().find_map(|t| {
                    t.blocks
                        .iter()
                        .chain(t.secondary_pane.iter().flat_map(|p| p.blocks.iter()))
                        .find(|b| b.id == *block_id)
                        .map(|b| (t.id, format!("{} · {}", t.title, shorten(&b.command, 40))))
                });
                let (tab_id, label) = match found {
                    Some((tab_id, label)) => (Some(tab_id.to_string()), label),
                    None => (None, "MRT command".to_string()),
                };
                map.insert(
                    *pid,
                    OwnedProcess {
                        pid: *pid,
                        label,
                        tab_id,
                        kind: OwnedProcessKind::Command,
                    },
                );
            }
        }
        for tui in self.active_tui.iter().chain(self.pane_tuis.iter()) {
            if let Some(pid) = tui.pty.process_id() {
                map.insert(
                    pid,
                    OwnedProcess {
                        pid,
                        label: format!("TUI · {}", shorten(&tui.command, 40)),
                        tab_id: Some(tui.tab_id.to_string()),
                        kind: OwnedProcessKind::Tui,
                    },
                );
            }
        }
        map
    }

    fn with_block_mut(&mut self, tab_id: Uuid, block_id: Uuid, f: impl FnOnce(&mut CommandBlock)) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return false;
        };
        let block = tab
            .blocks
            .iter_mut()
            .chain(tab.secondary_pane.iter_mut().flat_map(|p| p.blocks.iter_mut()))
            .find(|b| b.id == block_id);
        match block {
            Some(block) => {
                f(block);
                true
            }
            None => false,
        }
    }

    fn push_block(&mut self, tab_id: Uuid, block: CommandBlock) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.blocks.push(block);
        }
    }

    fn process_error_block(command: &str, cwd: &std::path::Path, message: impl Into<String>) -> CommandBlock {
        CommandBlock::new_completed(command, PROCESS_BLOCK_SHELL, cwd.display().to_string(), message.into(), 1, 0)
    }

    fn on_process_snapshot(&mut self, result: Result<Arc<ProcessSnapshot>, String>) {
        self.process_tick_in_flight = false;
        self.process_tick_last = Some(Instant::now());
        let snapshot = match result {
            Ok(s) => s,
            Err(e) => {
                self.side_state.task_manager.error = Some(e);
                self.side_state.task_manager.refreshing = false;
                return;
            }
        };
        let owned = self.owned_processes();
        self.side_state
            .task_manager
            .apply_snapshot(snapshot.clone(), owned.clone());
        self.observe_activity_processes(&snapshot);

        // Live `:top` block
        if let Some(live) = &self.live_top {
            let (tab_id, block_id) = (live.tab_id, live.block_id);
            let rows = query_processes(&snapshot, &live.query, &owned);
            let (structured, csv) = processes_to_table(&rows);
            let updated = self.with_block_mut(tab_id, block_id, |block| {
                let elapsed = (chrono::Local::now() - block.timestamp).num_milliseconds().max(0) as u64;
                let mut fresh = CommandBlock::new_structured(
                    &block.command,
                    PROCESS_BLOCK_SHELL,
                    block.cwd.clone(),
                    csv,
                    structured,
                    0,
                    elapsed,
                );
                fresh.id = block.id;
                fresh.timestamp = block.timestamp;
                fresh.exit_code = None;
                fresh.pinned = block.pinned;
                fresh.view_mode = block.view_mode.clone();
                *block = fresh;
            });
            if !updated {
                self.live_top = None;
            }
        }

        // `:watch-proc` alerts
        let mut alerts: Vec<(Uuid, usize, String)> = Vec::new();
        let mut finished = Vec::new();
        for (idx, active) in self.process_watches.iter_mut().enumerate() {
            for alert in active.watch.evaluate(&snapshot) {
                if matches!(alert, mrt_tools::WatchAlert::Exited { .. })
                    && matches!(active.watch.spec.selector, ProcessSelector::Pid(_))
                {
                    finished.push(idx);
                }
                alerts.push((active.tab_id, active.watch.id, alert.message()));
            }
        }
        for (tab_id, watch_id, message) in alerts {
            let cwd = self
                .tabs
                .iter()
                .find(|t| t.id == tab_id)
                .map(|t| t.cwd.clone())
                .unwrap_or_default();
            let block = CommandBlock::new_completed(
                format!(":watch-proc #{watch_id}"),
                PROCESS_BLOCK_SHELL,
                cwd.display().to_string(),
                format!("⏱ {message}"),
                0,
                0,
            );
            self.push_block(tab_id, block);
        }
        for idx in finished.into_iter().rev() {
            self.process_watches.remove(idx);
        }
        let open_tabs: Vec<Uuid> = self.tabs.iter().map(|t| t.id).collect();
        self.process_watches.retain(|w| open_tabs.contains(&w.tab_id));
    }

    fn spawn_ps_command(&mut self, tab_id: Uuid, cwd: PathBuf, cmd: String, query: ProcessQuery) {
        let sampler = self.process_sampler.clone();
        let owned = self.owned_processes();
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match sampler.snapshot().await {
                Ok(snapshot) => {
                    let rows = query_processes(&snapshot, &query, &owned);
                    let (structured, csv) = processes_to_table(&rows);
                    CommandBlock::new_structured(
                        &cmd,
                        PROCESS_BLOCK_SHELL,
                        cwd_str,
                        csv,
                        structured,
                        0,
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => CommandBlock::new_completed(
                    &cmd,
                    PROCESS_BLOCK_SHELL,
                    cwd_str,
                    format!("Process listing failed: {e}"),
                    1,
                    start.elapsed().as_millis() as u64,
                ),
            };
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd: None });
        });
    }

    fn start_live_top(&mut self, tab_id: Uuid, cwd: PathBuf, cmd: String, query: ProcessQuery) {
        self.stop_live_top();
        let mut block = CommandBlock::new_structured(
            &cmd,
            PROCESS_BLOCK_SHELL,
            cwd.display().to_string(),
            String::new(),
            StructuredOutput::Table {
                headers: mrt_tools::process_inspector::PROCESS_TABLE_HEADERS
                    .iter()
                    .map(|h| h.to_string())
                    .collect(),
                rows: Vec::new(),
            },
            0,
            0,
        );
        block.exit_code = None;
        let block_id = block.id;
        self.push_block(tab_id, block);
        self.live_top = Some(LiveTopBlock { tab_id, block_id, query });
        self.force_process_tick = true;
    }

    fn stop_live_top(&mut self) {
        if let Some(live) = self.live_top.take() {
            self.with_block_mut(live.tab_id, live.block_id, |block| {
                block.exit_code = Some(0);
                block.duration_ms = (chrono::Local::now() - block.timestamp).num_milliseconds().max(0) as u64;
            });
        }
    }

    fn spawn_proc_command(&mut self, tab_id: Uuid, cwd: PathBuf, cmd: String, pid: u32) {
        let sampler = self.process_sampler.clone();
        let owned = self.owned_processes();
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match inspect_process(&sampler, pid, &owned).await {
                Ok(details) => {
                    let md = process_details_markdown(&details);
                    CommandBlock::new_structured(
                        &cmd,
                        PROCESS_BLOCK_SHELL,
                        cwd_str,
                        md.clone(),
                        StructuredOutput::Markdown(md),
                        0,
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => CommandBlock::new_completed(&cmd, PROCESS_BLOCK_SHELL, cwd_str, e.to_string(), 1, 0),
            };
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd: None });
        });
    }

    /// Resolves a kill request against a fresh snapshot; the result opens the Safety Gate.
    /// `:suspend`, `:resume`, `:priority`: resolved like `:kill`; Critical targets are
    /// refused and the rest changed at once (each change can be undone).
    fn spawn_process_control(&mut self, tab_id: Uuid, cwd: PathBuf, raw_input: String, request: mrt_tools::ControlRequest) {
        let sampler = self.process_sampler.clone();
        let owned = self.owned_processes();
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let started = Instant::now();
            let (text, code) = match sampler.snapshot().await {
                Ok(snapshot) => match mrt_tools::plan_control(&snapshot, &request, &owned) {
                    Ok(targets) => {
                        let action = request.action;
                        let outcomes = tokio::task::spawn_blocking(move || mrt_tools::apply_control(action, &targets))
                            .await
                            .unwrap_or_default();
                        let failed = outcomes.iter().any(|o| o.result.is_err());
                        (mrt_tools::control_outcomes_markdown(action, &outcomes), i32::from(failed))
                    }
                    Err(e) => (e.to_string(), 1),
                },
                Err(e) => (format!("Process snapshot failed: {e}"), 1),
            };
            let block = CommandBlock::new_completed(&raw_input, "mrt-proc", cwd.display().to_string(), text, code, started.elapsed().as_millis() as u64);
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd: None });
        });
    }

    fn spawn_kill_plan(&mut self, tab_id: Uuid, cwd: PathBuf, request: KillRequest) {
        let sampler = self.process_sampler.clone();
        let owned = self.owned_processes();
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let plan = match sampler.snapshot().await {
                Ok(snapshot) => plan_kill(&snapshot, &request, &owned).map_err(|e| e.to_string()),
                Err(e) => Err(format!("Process snapshot failed: {e}")),
            };
            let _ = tx.send(AsyncWorkerEvent::KillPlanned { tab_id, cwd, plan });
        });
    }

    fn on_kill_planned(&mut self, tab_id: Uuid, cwd: PathBuf, plan: Result<KillPlan, String>) {
        match plan {
            Err(message) => {
                self.push_block(tab_id, Self::process_error_block(":kill", &cwd, message));
            }
            Ok(plan) => {
                if self.pending_safety.is_some() {
                    self.push_block(
                        tab_id,
                        Self::process_error_block(
                            &plan.assessment.command,
                            &cwd,
                            "Another Safety Gate approval is pending; answer it first, then retry.",
                        ),
                    );
                    return;
                }
                let shell = self
                    .tabs
                    .iter()
                    .find(|t| t.id == tab_id)
                    .and_then(|t| self.shells.get(t.active_shell_idx))
                    .cloned()
                    .unwrap_or_else(|| self.shells[0].clone());
                self.pending_safety = Some(PendingSafetyApproval {
                    tab_id,
                    kill: Some(PendingKill {
                        pids: plan.pids(),
                        force: plan.request.force,
                        tree: plan.request.tree,
                    }),
                    assessment: plan.assessment,
                    shell,
                    cwd,
                });
            }
        }
    }

    fn spawn_kill_execution(&mut self, tab_id: Uuid, cwd: PathBuf, command: String, kill: PendingKill) {
        let sampler = self.process_sampler.clone();
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let start = Instant::now();
            let cwd_str = cwd.display().to_string();
            let block = match sampler.terminate(kill.pids, kill.force, kill.tree).await {
                Ok(outcomes) => {
                    let md = kill_outcomes_markdown(&outcomes);
                    let ok = outcomes.iter().all(|o| o.succeeded());
                    CommandBlock::new_structured(
                        &command,
                        PROCESS_BLOCK_SHELL,
                        cwd_str,
                        md.clone(),
                        StructuredOutput::Markdown(md),
                        if ok { 0 } else { 1 },
                        start.elapsed().as_millis() as u64,
                    )
                }
                Err(e) => CommandBlock::new_completed(&command, PROCESS_BLOCK_SHELL, cwd_str, e.to_string(), 1, 0),
            };
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd: None });
        });
    }

    /// The block "Stop" button: ends a live `:top`, or force-stops the command's process tree.
    /// The command's own block then completes normally with the exit status.
    fn stop_block(&mut self, block_id: Uuid) {
        if self.live_top.as_ref().map(|l| l.block_id) == Some(block_id) {
            self.stop_live_top();
            return;
        }
        // A command in a persistent shell gets Ctrl+C; killing the shell's process tree
        // would end the shell and lose its state. If the command is still running a moment
        // later (Ctrl+C does not reach programs through Windows ConPTY), stop the shell's
        // child processes instead, which leaves the shell at its prompt.
        if let Some(session) = self.shell_pool.interrupt(block_id) {
            if let Some(shell_pid) = session.process_id() {
                let pool = self.shell_pool.clone();
                let sampler = self.process_sampler.clone();
                self.runtime.spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    if !pool.is_running(block_id) {
                        return;
                    }
                    if let Ok(snapshot) = sampler.snapshot().await {
                        let children: Vec<u32> = snapshot.children_of(shell_pid).iter().map(|e| e.pid).collect();
                        if !children.is_empty() {
                            let _ = sampler.terminate(children, true, true).await;
                        }
                    }
                });
            }
            return;
        }
        let Some(pid) = self.running_pids.lock().ok().and_then(|g| g.get(&block_id).copied()) else {
            return;
        };
        let tab_id = self
            .tabs
            .iter()
            .find(|t| {
                t.blocks
                    .iter()
                    .chain(t.secondary_pane.iter().flat_map(|p| p.blocks.iter()))
                    .any(|b| b.id == block_id)
            })
            .map(|t| t.id)
            .unwrap_or(self.tabs[self.active_tab].id);
        let sampler = self.process_sampler.clone();
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let error = match sampler.terminate(vec![pid], true, true).await {
                Ok(outcomes) => outcomes
                    .iter()
                    .find(|o| !o.succeeded())
                    .map(|o| format!("Could not stop {} (PID {}): {:?}", o.name, o.pid, o.status)),
                Err(e) => Some(e.to_string()),
            };
            let _ = tx.send(AsyncWorkerEvent::StopFinished { tab_id, error });
        });
    }

    /// Handles `:toolbox`, `:ps`, `:top`, `:proc`, `:kill` and `:watch-proc`.
    /// Returns `false` when `raw_input` is not a process/toolbox command.
    fn try_process_command(&mut self, tab_id: Uuid, cwd: &std::path::Path, raw_input: &str) -> bool {
        let cwd = cwd.to_path_buf();
        if let Some(rest) = command_args(raw_input, ":toolbox") {
            match rest.trim() {
                "close" | "hide" => self.side_state.show_toolbox_panel = false,
                "" => self.side_state.show_toolbox_panel = !self.side_state.show_toolbox_panel,
                name => match ToolboxTab::parse(name) {
                    Some(ToolboxTab::TaskManager) => self.open_task_manager(None),
                    Some(tab) => {
                        self.side_state.show_toolbox_panel = true;
                        self.side_state.toolbox_tab = tab;
                    }
                    None => self.push_block(
                        tab_id,
                        Self::process_error_block(raw_input, &cwd, format!("Unknown Toolbox tab '{name}' (use network or task-manager)")),
                    ),
                },
            }
            return true;
        }
        if command_args(raw_input, ":ps").is_some() {
            match parse_ps_command(raw_input) {
                Ok(query) => self.spawn_ps_command(tab_id, cwd, raw_input.to_string(), query),
                Err(e) => self.push_block(tab_id, Self::process_error_block(raw_input, &cwd, e.to_string())),
            }
            return true;
        }
        if let Some(rest) = command_args(raw_input, ":top") {
            if rest.trim().eq_ignore_ascii_case("stop") {
                self.stop_live_top();
                return true;
            }
            match parse_ps_command(raw_input) {
                Ok(mut query) => {
                    if query.limit.is_none() {
                        query.limit = Some(25);
                    }
                    self.start_live_top(tab_id, cwd, raw_input.to_string(), query);
                }
                Err(e) => self.push_block(tab_id, Self::process_error_block(raw_input, &cwd, e.to_string())),
            }
            return true;
        }
        if let Some(rest) = command_args(raw_input, ":proc") {
            match rest.trim().parse::<u32>() {
                Ok(pid) => self.spawn_proc_command(tab_id, cwd, raw_input.to_string(), pid),
                Err(_) => self.push_block(tab_id, Self::process_error_block(raw_input, &cwd, "Usage: :proc <pid>")),
            }
            return true;
        }
        if [":suspend", ":resume", ":priority"].iter().any(|c| command_args(raw_input, c).is_some()) {
            match mrt_tools::parse_control_command(raw_input) {
                Ok(request) => self.spawn_process_control(tab_id, cwd, raw_input.to_string(), request),
                Err(e) => self.push_block(tab_id, Self::process_error_block(raw_input, &cwd, e.to_string())),
            }
            return true;
        }
        if let Some(rest) = command_args(raw_input, ":kill") {
            if rest.trim().is_empty() {
                self.open_task_manager(None);
                return true;
            }
            match parse_kill_command(raw_input) {
                Ok(request) => self.spawn_kill_plan(tab_id, cwd, request),
                Err(e) => self.push_block(tab_id, Self::process_error_block(raw_input, &cwd, e.to_string())),
            }
            return true;
        }
        let unwatch = command_args(raw_input, ":unwatch-proc").map(|r| format!("stop {r}"));
        if let Some(rest) = command_args(raw_input, ":watch-proc").map(str::to_string).or(unwatch) {
            let rest = rest.trim();
            let mut words = rest.split_whitespace();
            match words.next() {
                Some("list") | None => {
                    let text = if self.process_watches.is_empty() {
                        "No active process watches. Start one with :watch-proc <pid|name> [--mem 2GB] [--cpu 90].".to_string()
                    } else {
                        self.process_watches
                            .iter()
                            .map(|w| w.watch.describe())
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    self.push_block(
                        tab_id,
                        CommandBlock::new_completed(raw_input, PROCESS_BLOCK_SHELL, cwd.display().to_string(), text, 0, 0),
                    );
                }
                Some("clear") => {
                    let count = self.process_watches.len();
                    self.process_watches.clear();
                    self.push_block(
                        tab_id,
                        CommandBlock::new_completed(raw_input, PROCESS_BLOCK_SHELL, cwd.display().to_string(), format!("Stopped {count} watch(es)."), 0, 0),
                    );
                }
                Some("stop") => {
                    let target = words.next().unwrap_or("").trim_start_matches('#');
                    let before = self.process_watches.len();
                    if target == "all" {
                        self.process_watches.clear();
                    } else if let Ok(id) = target.parse::<usize>() {
                        self.process_watches.retain(|w| w.watch.id != id);
                    }
                    let removed = before - self.process_watches.len();
                    self.push_block(
                        tab_id,
                        CommandBlock::new_completed(
                            raw_input,
                            PROCESS_BLOCK_SHELL,
                            cwd.display().to_string(),
                            format!("Stopped {removed} watch(es)."),
                            if removed > 0 { 0 } else { 1 },
                            0,
                        ),
                    );
                }
                Some(_) => match parse_watch_command(raw_input) {
                    Ok(spec) => {
                        let id = self.next_watch_id;
                        self.next_watch_id += 1;
                        let watch = ProcessWatch::new(id, spec);
                        let text = format!(
                            "Watching {}. Alerts appear in this tab; stop it with :watch-proc stop {id}.",
                            watch.describe()
                        );
                        self.process_watches.push(ActiveProcessWatch { tab_id, watch });
                        self.force_process_tick = true;
                        self.push_block(
                            tab_id,
                            CommandBlock::new_completed(raw_input, PROCESS_BLOCK_SHELL, cwd.display().to_string(), text, 0, 0),
                        );
                    }
                    Err(e) => self.push_block(tab_id, Self::process_error_block(raw_input, &cwd, e.to_string())),
                },
            }
            return true;
        }
        false
    }

    fn handle_task_manager_event(&mut self, tab_id: Uuid, cwd: PathBuf, event: TaskManagerEvent) {
        match event {
            TaskManagerEvent::Refresh => self.force_process_tick = true,
            TaskManagerEvent::Stop { pid, force, tree } => self.spawn_kill_plan(
                tab_id,
                cwd,
                KillRequest {
                    selectors: vec![ProcessSelector::Pid(pid)],
                    force,
                    tree,
                },
            ),
            TaskManagerEvent::Inspect(pid) => self.submit_input(Some(format!(":proc {pid}")), false),
            TaskManagerEvent::Watch(pid) => self.submit_input(Some(format!(":watch-proc {pid}")), false),
            TaskManagerEvent::Control(command) => self.submit_input(Some(command), false),
            TaskManagerEvent::SnapshotToTab { command, compare } => {
                if compare {
                    self.pending_ps_compare = Some(tab_id);
                }
                self.submit_input(Some(command), false);
            }
            TaskManagerEvent::ScanPorts => self.spawn_port_scanner(tab_id, cwd),
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
        self.shell_pool.close_tab(id);
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
        if self.live_top.as_ref().is_some_and(|live| live.tab_id == id) {
            self.live_top = None;
        }
        self.process_watches.retain(|w| w.tab_id != id);
    }

    fn add_mosaic_terminal(&mut self) {
        let active = &self.tabs[self.active_tab];
        let tab = TerminalTab::new(
            format!("Terminal {}", self.tabs.len() + 1),
            active.active_shell_idx,
            active.cwd.clone(),
        );
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    /// Mosaic: every terminal on screen at once, arranged by a layout preset
    /// (Grid / Columns / Rows / Focus). Tiles are reordered by dragging their header.
    fn render_mosaic(&mut self, ui: &mut egui::Ui) {
        // Toolbar: layout presets, column stepper, add terminal.
        let mut add_terminal = false;
        ui.horizontal_wrapped(|ui| {
            egui::Frame::none()
                .fill(Palette::BG_INSET())
                .rounding(egui::Rounding::same(Radius::BUTTON))
                .inner_margin(egui::Margin::same(3.0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for layout in MosaicLayout::ALL {
                        let selected = self.mosaic_layout == layout;
                        let button = egui::Button::new(
                            RichText::new(layout.label()).size(13.0).color(if selected {
                                Palette::TEXT_PRIMARY()
                            } else {
                                Palette::TEXT_SECONDARY()
                            }),
                        )
                        .fill(if selected { Palette::BORDER_SUBTLE() } else { Color32::TRANSPARENT })
                        .stroke(egui::Stroke::NONE)
                        .rounding(egui::Rounding::same(8.0))
                        .min_size(egui::vec2(0.0, 28.0));
                        if ui.add(button).on_hover_text(layout.description()).clicked() {
                            self.mosaic_layout = layout;
                        }
                    }
                });
            if self.mosaic_layout == MosaicLayout::Grid {
                ui.add_space(6.0);
                ui.label(RichText::new("Per row").size(13.0).color(Palette::TEXT_MUTED()));
                if ui
                    .add_enabled(self.mosaic_columns > 1, secondary_button("−").min_size(egui::vec2(30.0, 30.0)))
                    .on_hover_text("Fewer columns")
                    .clicked()
                {
                    self.mosaic_columns -= 1;
                }
                ui.label(
                    RichText::new(self.mosaic_columns.to_string())
                        .monospace()
                        .size(14.0)
                        .color(Palette::TEXT_PRIMARY()),
                );
                if ui
                    .add_enabled(self.mosaic_columns < 6, secondary_button("+").min_size(egui::vec2(30.0, 30.0)))
                    .on_hover_text("More columns")
                    .clicked()
                {
                    self.mosaic_columns += 1;
                }
            }
            ui.add_space(6.0);
            if ui.add(secondary_button("+  Terminal")).clicked() {
                add_terminal = true;
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(match self.mosaic_layout {
                    MosaicLayout::Focus => "Click a terminal to make it the main one · drag headers to reorder",
                    _ => "Drag a terminal's header onto another to move it there",
                })
                .size(12.0)
                .color(Palette::TEXT_MUTED()),
            );
        });
        if add_terminal {
            self.add_mosaic_terminal();
        }
        ui.add_space(10.0);

        let gap = 10.0;
        let viewport = ui.available_rect_before_wrap();
        let mut reorder = None;
        let mut close = None;
        let mut submit = None;
        let mut action = None;
        let mut expand = None;
        let mut select = None;
        let active_index = self.active_tab;
        let layout = self.mosaic_layout;
        let columns = self.mosaic_columns;
        // Tabs running an interactive program show its screen instead of their output:
        // the overlay's session, else the session in the tab's active pane, else any pane.
        let mut tui_screens: HashMap<Uuid, mrt_core::TermSnapshot> = HashMap::new();
        for session in self.active_tui.iter().chain(self.pane_tuis.iter()) {
            let preferred = session.pane.is_none()
                || self
                    .tabs
                    .iter()
                    .any(|t| t.id == session.tab_id && Some(t.active_pane_idx) == session.pane);
            if preferred || !tui_screens.contains_key(&session.tab_id) {
                tui_screens.insert(session.tab_id, session.term.snapshot());
            }
        }
        let shells = &self.shells;
        let tabs = &mut self.tabs;

        egui::ScrollArea::vertical()
            .id_salt("mosaic_grid")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let area = egui::Rect::from_min_size(
                    ui.cursor().min,
                    egui::vec2(ui.available_width(), (viewport.height() - 10.0).max(200.0)),
                );
                let (order, rects) = mosaic_rects(layout, tabs.len(), columns, active_index, area, gap);
                let content_bottom = rects.iter().map(|r| r.bottom()).fold(area.top(), f32::max);
                ui.allocate_rect(
                    egui::Rect::from_min_max(area.min, egui::pos2(area.right(), content_bottom)),
                    egui::Sense::hover(),
                );

                for (slot, rect) in rects.iter().enumerate() {
                    let index = order[slot];
                    let tab = &mut tabs[index];
                    let id = tab.id;
                    let is_active = index == active_index;
                    let shell_label = shells
                        .get(tab.active_shell_idx)
                        .map(|s| s.kind.short_label())
                        .unwrap_or("shell");
                    let status = Self::tab_status(tab);
                    let folder = tab
                        .cwd
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| tab.cwd.display().to_string());
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(*rect).id_salt(("mosaic_tile", id)), |ui| {
                        ui.set_clip_rect(rect.expand(2.0).intersect(ui.clip_rect()));
                        let frame = egui::Frame::none()
                            .fill(Palette::BG_SURFACE())
                            .stroke(egui::Stroke::new(
                                if is_active { 1.5_f32 } else { 1.0_f32 },
                                if is_active { Palette::ACCENT() } else { Palette::BORDER_FAINT() },
                            ))
                            .rounding(egui::Rounding::same(Radius::BLOCK))
                            .inner_margin(egui::Margin::symmetric(12.0, 10.0));
                        let (panel, dropped) = ui.dnd_drop_zone::<Uuid, _>(frame, |ui| {
                            let inner_w = (rect.width() - 26.0).max(1.0);
                            let inner_h = (rect.height() - 22.0).max(1.0);
                            ui.set_width(inner_w);
                            ui.set_height(inner_h);

                            // Header: actions are laid out first (right to left) so the
                            // title gets the remaining width; narrow tiles drop the extras.
                            let wide = inner_w >= 330.0;
                            ui.horizontal(|ui| {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if icon_button(ui, "×", "Close terminal").clicked() {
                                        close = Some(id);
                                    }
                                    if wide {
                                        if ui
                                            .add(ghost_button("Open"))
                                            .on_hover_text("Leave Mosaic and open this terminal")
                                            .clicked()
                                        {
                                            expand = Some(index);
                                        }
                                        egui::ComboBox::from_id_salt(("mosaic_shell", id))
                                            .width(96.0)
                                            .selected_text(
                                                RichText::new(shell_label).monospace().size(11.5).color(Palette::TEXT_SECONDARY()),
                                            )
                                            .show_ui(ui, |ui| {
                                                for (shell_index, shell) in shells.iter().enumerate() {
                                                    ui.selectable_value(&mut tab.active_shell_idx, shell_index, &shell.name);
                                                }
                                            });
                                    }
                                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        ui.dnd_drag_source(egui::Id::new(("terminal_drag", id)), id, |ui| {
                                            ui.horizontal(|ui| {
                                                activity_indicator(ui, &status);
                                                let title = ui.add(
                                                    egui::Label::new(
                                                        RichText::new(format!("{}  {folder}", index + 1))
                                                            .size(13.5)
                                                            .strong()
                                                            .color(if is_active { Palette::TEXT_PRIMARY() } else { Palette::TEXT_BODY() }),
                                                    )
                                                    .truncate()
                                                    .sense(egui::Sense::click()),
                                                );
                                                if title.double_clicked() {
                                                    expand = Some(index);
                                                } else if title.clicked() {
                                                    select = Some(index);
                                                }
                                            });
                                        })
                                        .response
                                        .on_hover_text(format!(
                                            "{shell_label} · {}\nDrag onto another terminal to move it there · double-click to open",
                                            tab.cwd.display()
                                        ));
                                    });
                                });
                            });

                            ui.add_space(4.0);
                            // Output
                            let input_h = 36.0;
                            let output_h = (ui.available_height() - input_h - 6.0).max(24.0);
                            if let Some(screen) = tui_screens.get(&id) {
                                draw_mosaic_tui(ui, id, screen, output_h);
                            } else {
                            egui::ScrollArea::vertical()
                                .id_salt(("mosaic_output", id))
                                .auto_shrink([false, false])
                                .max_height(output_h)
                                .min_scrolled_height(output_h)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    if tab.blocks.is_empty() {
                                        ui.label(
                                            RichText::new(tab.cwd.display().to_string())
                                                .monospace()
                                                .size(11.5)
                                                .color(Palette::TEXT_MUTED()),
                                        );
                                    }
                                    for block in &tab.blocks {
                                        if let Some(event) = render_compact_block(ui, block) {
                                            action = Some((index, event));
                                        }
                                    }
                                });
                            }

                            // Input
                            ui.add_space(6.0);
                            egui::Frame::none()
                                .fill(Palette::BG_INSET())
                                .rounding(egui::Rounding::same(Radius::BUTTON))
                                .inner_margin(egui::Margin::symmetric(10.0, 4.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new("›")
                                                .monospace()
                                                .size(14.0)
                                                .color(if is_active { Palette::ACCENT() } else { Palette::TEXT_MUTED() }),
                                        );
                                        let response = ui.add(
                                            egui::TextEdit::singleline(&mut tab.input_line)
                                                .id(egui::Id::new(("mosaic_input", id)))
                                                .font(egui::FontId::monospace(13.0))
                                                .frame(false)
                                                .desired_width(ui.available_width())
                                                .hint_text("Command, or # to ask AI"),
                                        );
                                        if response.gained_focus() || response.clicked() {
                                            select = Some(index);
                                        }
                                        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                            submit = Some(index);
                                            response.request_focus();
                                        }
                                    });
                                });
                        });
                        if is_active {
                            // dnd_drop_zone restyles the frame stroke, so mark the active tile on top.
                            ui.painter().rect_stroke(
                                rect.shrink(0.75),
                                egui::Rounding::same(Radius::BLOCK),
                                egui::Stroke::new(1.5_f32, Palette::ACCENT()),
                            );
                        }
                        if panel.response.clicked() {
                            select = Some(index);
                        }
                        if let Some(source) = dropped {
                            reorder = Some((*source, id));
                        }
                    });
                }
            });

        if let Some(index) = select {
            self.active_tab = index;
        }
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
        if let Some(index) = expand {
            self.active_tab = index;
            self.mosaic = false;
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
        if self.tabs[self.active_tab].remote.is_some() {
            egui::SidePanel::left("folder_explorer").frame(panel_frame()).default_width(240.0).show(ctx, |ui| {
                ui.label(RichText::new("Explorer").size(15.0).strong().color(Palette::TEXT_PRIMARY()));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("This is a remote (SSH) tab. The Explorer shows folders on this computer only.")
                        .size(12.5)
                        .color(Palette::TEXT_MUTED()),
                );
            });
            return;
        }
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
                    ui.label(RichText::new("Explorer").size(15.0).strong().color(Palette::TEXT_PRIMARY()));
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
                            .color(Palette::TEXT_MUTED()),
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
                        ui.colored_label(Palette::ACCENT_RED(), error);
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
                                            Palette::TEXT_SECONDARY(),
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
        let owned = self.owned_processes();
        if let Ok(mut guard) = state.owned_processes.lock() {
            *guard = owned;
        }
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
                        activity: Some(t.activity.snapshot(Instant::now())),
                        sandbox_root: t.sandbox_root.clone(),
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
                GuiUiCommand::SetMosaic { enabled, columns, layout } => {
                    self.mosaic = enabled;
                    if let Some(layout) = layout.as_deref().and_then(MosaicLayout::parse) {
                        self.mosaic_layout = layout;
                    }
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
                GuiUiCommand::SetToolbox { visible, tab } => {
                    self.side_state.show_toolbox_panel = visible;
                    if let Some(tab) = tab.as_deref().and_then(ToolboxTab::parse) {
                        self.side_state.toolbox_tab = tab;
                        if tab == ToolboxTab::TaskManager && visible {
                            self.force_process_tick = true;
                        }
                    }
                }
                GuiUiCommand::SetWindow { action } => self.pending_window = Some(action),
                GuiUiCommand::RequestProcessKill { pids, force, tree } => {
                    if let Some(tab) = self.tabs.get(self.active_tab) {
                        let (tab_id, cwd) = (tab.id, tab.cwd.clone());
                        self.spawn_kill_plan(
                            tab_id,
                            cwd,
                            KillRequest {
                                selectors: pids.into_iter().map(ProcessSelector::Pid).collect(),
                                force,
                                tree,
                            },
                        );
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
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == *tab_id) {
                tab.activity.feed_output(&chunk, Instant::now());
            }
            let found_in_primary = self.tabs.iter_mut().find(|tab| tab.id == *tab_id)
                .and_then(|tab| tab.blocks.iter_mut().find(|block| block.id == *block_id));
            let stream = self.vt_streams.entry(*block_id).or_default();
            if let Some(block) = found_in_primary {
                block.append_output(&chunk, stream);
            } else if let Some(sec) = self.tabs.iter_mut().find(|tab| tab.id == *tab_id).and_then(|tab| tab.secondary_pane.as_mut()) {
                if let Some(block) = sec.blocks.iter_mut().find(|block| block.id == *block_id) {
                    block.append_output(&chunk, stream);
                }
            }
        }
        let mut hook_events: Vec<crate::hooks::HookEvent> = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            if !ev.is_background() {
                self.busy_count = self.busy_count.saturating_sub(1);
            }
            match ev {
                AsyncWorkerEvent::ProcessSnapshot(result) => self.on_process_snapshot(result),
                AsyncWorkerEvent::KillPlanned { tab_id, cwd, plan } => self.on_kill_planned(tab_id, cwd, plan),
                AsyncWorkerEvent::StopFinished { tab_id, error } => {
                    if let Some(message) = error {
                        let cwd = self.tabs.iter().find(|t| t.id == tab_id).map(|t| t.cwd.clone()).unwrap_or_default();
                        self.push_block(tab_id, Self::process_error_block(":stop", &cwd, message));
                    }
                }
                AsyncWorkerEvent::BlockCompleted {
                    tab_id,
                    block,
                    updated_cwd,
                } => {
                    let was_streaming = self.running_output.remove(&block.id).is_some();
                    self.vt_streams.remove(&block.id);
                    if let Ok(mut pids) = self.running_pids.lock() {
                        pids.remove(&block.id);
                    }
                    if block.shell_name == "mrt-net" {
                        if let StructuredOutput::Table { rows, .. } = &block.structured {
                            self.side_state.last_ports = rows
                                .iter()
                                .filter(|r| r.len() >= 5)
                                .map(|r| PortEntry {
                                    protocol: r[0].clone(),
                                    local_address: r[1].clone(),
                                    port: r[2].clone(),
                                    state: r[3].clone(),
                                    pid: r[4].clone(),
                                })
                                .collect();
                        }
                    }
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
                        if was_streaming {
                            tab.activity.command_finished(block.exit_code.unwrap_or(1), Instant::now());
                        }
                        // A sandbox tab's folder never moves.
                        let updated_cwd = if tab.sandbox_root.is_some() { None } else { updated_cwd };
                        let cwd_changed = updated_cwd.as_ref().is_some_and(|new_cwd| *new_cwd != tab.cwd);
                        hook_events.push(crate::hooks::HookEvent {
                            event: if block.exit_code.unwrap_or(0) == 0 { "command_finished" } else { "command_failed" },
                            command: block.command.clone(),
                            exit_code: block.exit_code,
                            duration_ms: block.duration_ms,
                            cwd: block.cwd.clone(),
                            tab_title: tab.title.clone(),
                        });
                        if cwd_changed {
                            hook_events.push(crate::hooks::HookEvent {
                                event: "cwd_changed",
                                command: block.command.clone(),
                                cwd: updated_cwd.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                                tab_title: tab.title.clone(),
                                ..Default::default()
                            });
                        }
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
                        // "Compare with last snapshot" from the Task Manager
                        if self.pending_ps_compare == Some(tab_id)
                            && is_process_block(&block.command, &block.shell_name)
                            && block.command.starts_with(":ps")
                        {
                            self.pending_ps_compare = None;
                            let previous = tab
                                .blocks
                                .iter()
                                .rev()
                                .filter(|b| b.id != block.id)
                                .find(|b| {
                                    is_process_block(&b.command, &b.shell_name)
                                        && b.command.starts_with(":ps")
                                        && b.exit_code == Some(0)
                                })
                                .map(|b| b.id);
                            if let Some(previous) = previous {
                                tab.comparison = Some((previous, block.id));
                            }
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
        for event in hook_events {
            self.fire_hooks(event);
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
        let cwd = tab.sandbox_root.clone().unwrap_or_else(|| tab.cwd.clone());
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

        // 3. Toolbox and Task Manager commands (`:toolbox`, `:ps`, `:top`, `:proc`, `:kill`, `:watch-proc`)
        if raw_input.starts_with(':') && self.try_process_command(tab_id, &cwd, &raw_input) {
            return;
        }
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };

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
                let report = generate_markdown_bug_report(blk, history, &os_info, env!("CARGO_PKG_VERSION"));
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
        if let Some(pattern) = self.config_state.config.safety.blocked_pattern(&raw_input) {
            tab.blocks.push(CommandBlock::new_completed(
                raw_input.clone(),
                shell.name,
                cwd.display().to_string(),
                format!("Blocked by the Safety Gate: the command matches blocked pattern \"{pattern}\" from your MRT config."),
                1,
                0,
            ));
            return;
        }
        if !bypass_safety {
            let assessment = assess_command(&raw_input);
            let needs_approval = if tab.sandbox_root.is_some() {
                assessment.risk != RiskLevel::Safe
            } else {
                self.config_state.config.safety.requires_approval(&assessment)
            };
            if needs_approval {
                self.pending_safety = Some(PendingSafetyApproval {
                    tab_id,
                    assessment,
                    shell,
                    cwd,
                    kill: None,
                });
                return;
            }
        }

        if raw_input == ":assist" || raw_input.starts_with(":assist ") {
            let question = raw_input.trim_start_matches(":assist").trim();
            let question = if question.is_empty() {
                "Why did the last failed command fail, and what changed since it last worked?".to_string()
            } else {
                question.to_string()
            };
            let shell_label = shell.kind.short_label().to_string();
            self.spawn_assist(tab_id, cwd, shell_label, raw_input, question);
            return;
        }

        if raw_input == ":sandbox" || raw_input.starts_with(":sandbox ") {
            let arg = raw_input.trim_start_matches(":sandbox").trim();
            let root = if arg.is_empty() { cwd.clone() } else { cwd.join(arg) };
            self.open_sandbox_tab(root);
            return;
        }

        if raw_input == ":broadcast" || raw_input.starts_with(":broadcast ") {
            let arg = raw_input.trim_start_matches(":broadcast").trim();
            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                tab.broadcast = match arg {
                    "on" => true,
                    "off" => false,
                    _ => !tab.broadcast,
                };
            }
            return;
        }

        if let Some(spec) = raw_input.strip_prefix(":ssh ") {
            let args = crate::widgets::links::split_command_line(spec.trim());
            if !args.is_empty() {
                self.open_remote_tab(args);
            }
            return;
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

        // In a remote tab a full-screen program runs on the host, through `ssh -tt`, from a
        // local folder (the tab's folder is on the other machine).
        let (is_tui_command, cwd) = match (is_tui_command, tab.remote.as_ref()) {
            (Some(cmd), Some(args)) => {
                let remote_dir = tab.cwd.to_str().filter(|d| d.starts_with('/')).map(str::to_string);
                let line = crate::shell_pool::remote_tui_command(&shell.kind, args, remote_dir.as_deref(), &cmd);
                (Some(line), std::env::temp_dir())
            }
            (other, _) => (other, cwd),
        };

        if let Some(tui_cmd) = is_tui_command {
            let (cols, rows) = TUI_INITIAL_SIZE;
            // In a split tab the program runs inside the active pane (unless that pane
            // already has one); otherwise it gets the full-screen overlay.
            let pane = tab
                .secondary_pane
                .is_some()
                .then_some(tab.active_pane_idx)
                .filter(|p| !self.pane_tuis.iter().any(|s| s.tab_id == tab_id && s.pane == Some(*p)));
            match InteractivePty::spawn_with_command(&shell, &cwd, cols, rows, Some(&tui_cmd)) {
                Ok(pty) => {
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.activity.command_started(&tui_cmd, Instant::now());
                    }
                    let session = ActiveTuiSession::new(pty, tab_id, tui_cmd, pane, cols, rows);
                    if pane.is_some() {
                        self.pane_tuis.push(session);
                    } else {
                        self.active_tui = Some(session);
                    }
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
            if let Some(kill) = pending.kill {
                if self.tabs.iter().any(|tab| tab.id == pending.tab_id) {
                    self.spawn_kill_execution(pending.tab_id, pending.cwd, pending.assessment.command, kill);
                }
                return;
            }
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

        // MRT commands (`:split-h`, `:ports`...) in a named action run at once.
        if step.command.starts_with(':') {
            self.submit_input(Some(step.command), false);
            self.advance_runbook_replay();
            return;
        }

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

        if let Some(pattern) = self.config_state.config.safety.blocked_pattern(&step.command) {
            let message = format!(
                "Runbook stopped: '{}' matches blocked pattern \"{pattern}\" from your MRT config.",
                step.command
            );
            self.stop_runbook_replay(Some(message));
            return;
        }
        let assessment = assess_command(&step.command);
        if self.config_state.config.safety.requires_approval(&assessment) {
            self.pending_safety = Some(PendingSafetyApproval {
                tab_id,
                assessment,
                shell,
                cwd,
                kill: None,
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
            tab.activity.command_started(&raw_input, Instant::now());
        }
        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.running_output.insert(block_id, (tab_id, output.clone()));
        let running_pids = self.running_pids.clone();
        let tx = self.tx.clone();
        let pool = self.shell_pool.clone();
        let remote = self.tabs.iter().find(|t| t.id == tab_id).and_then(|t| t.remote.clone());
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let cwd_str = cwd.display().to_string();

            // Remote tab: always on the host, never in a local process.
            if let Some(args) = remote {
                let result = crate::shell_pool::run_remote(&pool, tab_id, block_id, &args, &raw_input, &output).await;
                let (text, exit_code, duration_ms, updated_cwd) = match result {
                    Ok(run) => (run.output, run.exit_code, run.duration_ms, run.cwd),
                    Err(e) => (e, 1, 0, None),
                };
                let mut block = CommandBlock::new_completed(&raw_input, "ssh", cwd_str, text, exit_code, duration_ms);
                block.id = block_id;
                block.timestamp = timestamp;
                let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd });
                return;
            }

            // Run in the tab's persistent shell when it is free; otherwise in a new process.
            if let Some(result) = crate::shell_pool::run_with_pool(&pool, tab_id, block_id, &shell, &raw_input, &cwd, &output, &running_pids).await {
                let (text, exit_code, duration_ms, updated_cwd) = match result {
                    Ok(run) => (run.output, run.exit_code, run.duration_ms, run.cwd),
                    Err(e) => (format!("Error: {e}"), 1, 0, None),
                };
                let mut block = CommandBlock::new_completed(
                    &raw_input,
                    shell.kind.short_label(),
                    cwd_str,
                    text,
                    exit_code,
                    duration_ms,
                );
                block.id = block_id;
                block.timestamp = timestamp;
                let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd });
                return;
            }

            let on_spawn = |pid: u32| {
                if let Ok(mut pids) = running_pids.lock() {
                    pids.insert(block_id, pid);
                }
            };
            match execute_command_streaming_with_pid(&shell, &raw_input, &cwd, on_spawn, |chunk| {
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
            SidePanelEvent::OpenProcessInTaskManager(pid) => self.open_task_manager(Some(pid)),
            SidePanelEvent::TaskManager(ev) => self.handle_task_manager_event(tab_id, cwd, ev),
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
                transport,
                command,
                args,
                url,
                headers,
            } => {
                let tx = self.tx.clone();
                let mut host = self.mcp_host.clone();
                // Invalid header lines are reported as a failed server row (without the values).
                let config = match transport {
                    McpTransportKind::Stdio => Ok(McpServerConfig::stdio(
                        name,
                        command,
                        args.split_whitespace().map(|s| s.to_string()).collect(),
                    )),
                    McpTransportKind::Http => match parse_http_headers(&headers) {
                        Ok(headers) => Ok(McpServerConfig::http(name, url.trim(), headers)),
                        Err(e) => Err(McpServerStatus::failed(
                            McpServerConfig::http(name, url.trim(), Vec::new()),
                            e.to_string(),
                        )),
                    },
                };
                self.busy_count += 1;
                match config {
                    Ok(config) => {
                        self.runtime.spawn(async move {
                            if let Ok(status) = host.connect_external_server(config).await {
                                let _ = tx.send(AsyncWorkerEvent::ExternalMcpConnected(status));
                            }
                        });
                    }
                    Err(status) => {
                        let _ = tx.send(AsyncWorkerEvent::ExternalMcpConnected(status));
                    }
                }
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

    /// The TUI session that receives the keyboard: the overlay when it is open,
    /// otherwise the session in the active pane of the active tab. None while the
    /// command palette or the Safety Gate is open, or in Mosaic.
    fn focused_tui(&self) -> Option<FocusedTui> {
        if self.active_tui.is_some() {
            return Some(FocusedTui::Overlay);
        }
        if self.mosaic || self.show_command_palette || self.pending_safety.is_some() {
            return None;
        }
        let tab = self.tabs.get(self.active_tab)?;
        let pane = if tab.secondary_pane.is_some() { tab.active_pane_idx } else { 0 };
        self.pane_tuis
            .iter()
            .position(|s| s.tab_id == tab.id && s.pane == Some(pane))
            .map(FocusedTui::Pane)
    }

    /// Runs every TUI session for one frame: output, exit detection, and keyboard input
    /// for the focused one. Finished sessions become command blocks in their pane.
    fn process_tui_session(&mut self, ctx: &egui::Context) {
        // Pane sessions whose pane is gone (tab closed, split removed) end without a block.
        let tabs = &self.tabs;
        self.pane_tuis.retain(|s| {
            let alive = tabs.iter().any(|t| t.id == s.tab_id && (s.pane != Some(1) || t.secondary_pane.is_some()));
            if !alive {
                let _ = s.pty.kill();
            }
            alive
        });
        if self.active_tui.is_none() && self.pane_tuis.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(30));

        let focused = self.focused_tui();
        let kill_requested = std::mem::take(&mut self.tui_exit_requested);
        let now = Instant::now();

        let mut overlay_exit = None;
        if let Some(session) = self.active_tui.as_mut() {
            let fresh = session.pump_output();
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == session.tab_id) {
                if !fresh.is_empty() {
                    tab.activity.feed_output(&fresh, now);
                }
            }
            overlay_exit = session.pty.try_wait_exit().ok().flatten().map(|c| c as i32);
            if focused == Some(FocusedTui::Overlay) {
                let _ = session.forward_keys(ctx);
                if kill_requested {
                    let _ = session.pty.kill();
                    overlay_exit = Some(130);
                }
            }
        }
        if let Some(code) = overlay_exit {
            if let Some(session) = self.active_tui.take() {
                self.finish_tui(session, code);
            }
        }

        let mut broadcast: Option<(Uuid, Option<usize>, Vec<u8>)> = None;
        let mut index = 0;
        while index < self.pane_tuis.len() {
            let session = &mut self.pane_tuis[index];
            let fresh = session.pump_output();
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == session.tab_id) {
                if !fresh.is_empty() {
                    tab.activity.feed_output(&fresh, now);
                }
            }
            let mut exit = session.pty.try_wait_exit().ok().flatten().map(|c| c as i32);
            if focused == Some(FocusedTui::Pane(index)) {
                let sent = session.forward_keys(ctx);
                let tab_broadcasts = self.tabs.iter().any(|t| t.id == session.tab_id && t.broadcast);
                if tab_broadcasts && !sent.is_empty() {
                    broadcast = Some((session.tab_id, session.pane, sent));
                }
                if kill_requested {
                    let _ = session.pty.kill();
                    exit = Some(130);
                }
            }
            if let Some(code) = exit {
                let session = self.pane_tuis.remove(index);
                self.finish_tui(session, code);
                continue;
            }
            index += 1;
        }
        // Broadcast: the same keys go to the programs in the tab's other panes.
        if let Some((tab_id, pane, bytes)) = broadcast {
            for other in self.pane_tuis.iter().filter(|s| s.tab_id == tab_id && s.pane != pane) {
                let _ = other.pty.send_bytes(&bytes);
            }
        }
    }

    /// Stores a finished TUI session as a command block in the pane it ran in.
    fn finish_tui(&mut self, mut session: ActiveTuiSession, exit_code: i32) {
        // Pick up output that arrived after this frame's drain.
        let rest = session.pty.drain_output();
        if !rest.is_empty() {
            session.term.feed(rest.as_bytes());
        }
        let output = session.final_output();
        let duration_ms = session.start_time.elapsed().as_millis() as u64;
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == session.tab_id) else {
            return;
        };
        tab.activity.command_finished(exit_code, Instant::now());
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
        match (session.pane, tab.secondary_pane.as_mut()) {
            (Some(1), Some(secondary)) => secondary.blocks.push(block),
            _ => tab.blocks.push(block),
        }
    }

    /// The find bar above the active tab's blocks (Ctrl+F): query box, match count,
    /// previous/next and close. Publishes the highlight blocks draw this frame.
    fn render_find_bar(&mut self, ui: &mut egui::Ui) {
        let key = egui::Id::new(crate::find::HIGHLIGHT_ID);
        if !self.find.open || self.mosaic {
            ui.ctx().data_mut(|d| d.remove::<crate::find::Highlight>(key));
            return;
        }
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let blocks = tab.blocks.iter().chain(tab.secondary_pane.iter().flat_map(|p| p.blocks.iter()));
        let (per_block, total) =
            crate::find::count_matches(blocks.map(|b| (b.id, b.plain_output.as_str())), &self.find.query);
        if self.find.current >= total {
            self.find.current = 0;
        }

        let mut step = None;
        let mut close = false;
        egui::Frame::none()
            .fill(Palette::BG_ELEVATED())
            .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_STRONG()))
            .rounding(egui::Rounding::same(Radius::BUTTON))
            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔍").color(Palette::TEXT_MUTED()));
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.find.query)
                            .hint_text("Find in output")
                            .desired_width(280.0),
                    );
                    if std::mem::take(&mut self.find.focus_requested) {
                        edit.request_focus();
                    }
                    if edit.changed() {
                        self.find.current = 0;
                        self.find.scroll_pending = true;
                    }
                    let (enter, shift, escape) =
                        ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                    if edit.lost_focus() && enter {
                        step = Some(!shift);
                        edit.request_focus();
                    }
                    if edit.lost_focus() && escape {
                        close = true;
                    }
                    let count = if self.find.query.is_empty() {
                        String::new()
                    } else if total == 0 {
                        "No matches".to_string()
                    } else {
                        format!("{} of {total}", self.find.current + 1)
                    };
                    ui.label(RichText::new(count).size(12.5).color(Palette::TEXT_MUTED()));
                    if ui.add_enabled(total > 0, egui::Button::new("▲").small()).on_hover_text("Previous match (Shift+Enter)").clicked() {
                        step = Some(false);
                    }
                    if ui.add_enabled(total > 0, egui::Button::new("▼").small()).on_hover_text("Next match (Enter)").clicked() {
                        step = Some(true);
                    }
                    if ui.small_button("×").on_hover_text("Close (Esc)").clicked() {
                        close = true;
                    }
                });
            });
        ui.add_space(6.0);

        if let Some(forward) = step {
            self.find.step(forward, total);
        }
        if close {
            self.find.close();
            ui.ctx().data_mut(|d| d.remove::<crate::find::Highlight>(key));
            return;
        }
        let highlight = crate::find::Highlight {
            query: self.find.query.clone(),
            current: crate::find::locate(&per_block, self.find.current),
            scroll: {
                if std::mem::take(&mut self.find.scroll_pending) {
                    self.find.scroll_frames = 3;
                }
                let scroll = self.find.scroll_frames > 0 && total > 0;
                self.find.scroll_frames = self.find.scroll_frames.saturating_sub(1);
                if self.find.scroll_frames > 0 {
                    ui.ctx().request_repaint();
                }
                scroll
            },
        };
        ui.ctx().data_mut(|d| d.insert_temp(key, highlight));
    }

    fn render_tui_overlay(&mut self, ctx: &egui::Context) {
        let exit_keys = self
            .key_resolver
            .keymap()
            .bindings_for(&Action::ExitTui)
            .into_iter()
            .next()
            .unwrap_or_default();
        let Some(session) = self.active_tui.as_mut() else {
            return;
        };

        let screen_rect = ctx.screen_rect();
        let window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let mut exit_clicked = false;
        // The dimmed backdrop sits in the Middle layer, above the app's panels;
        // the terminal window goes in the Foreground layer so it is drawn on top.
        egui::Area::new(egui::Id::new("tui_overlay_modal"))
            .order(egui::Order::Middle)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    screen_rect,
                    0.0,
                    Color32::from_black_alpha(220),
                );

                let win_width = (screen_rect.width() * 0.94).max(300.0);
                let win_height = (screen_rect.height() * 0.90).max(200.0);
                // The program may set a window title (OSC 0/2); fall back to the command.
                let title = session
                    .term
                    .title()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| session.command.clone());

                egui::Window::new(format!("📺 {title}"))
                    .id(egui::Id::new("tui_overlay_window"))
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                    .fixed_size(egui::vec2(win_width, win_height))
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{} × {}", session.term.cols(), session.term.rows()))
                                    .monospace()
                                    .color(Palette::TEXT_MUTED()),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let label = if exit_keys.is_empty() {
                                    "Exit Session".to_string()
                                } else {
                                    format!("Exit Session ({exit_keys})")
                                };
                                if ui.button(RichText::new(label).color(Palette::ACCENT_RED())).clicked() {
                                    exit_clicked = true;
                                }
                            });
                        });
                        ui.separator();

                        session.draw_surface(ctx, ui, true, window_focused);
                    });
            });
        if exit_clicked {
            let _ = session.pty.kill();
        }
    }
}

/// Draws the inline TUI session of `tab_id`'s pane `pane`, if it has one: a header
/// with the program's title and an exit button, then the terminal, `height` tall
/// (or all the space left). Returns `None` when the pane has no session, otherwise
/// whether the user clicked the terminal (to focus that pane).
#[allow(clippy::too_many_arguments)]
fn draw_pane_tui(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    sessions: &mut [ActiveTuiSession],
    tab_id: Uuid,
    pane: usize,
    height: Option<f32>,
    focused: bool,
    window_focused: bool,
    exit_keys: &str,
    broadcast: bool,
) -> Option<bool> {
    let session = sessions.iter_mut().find(|s| s.tab_id == tab_id && s.pane == Some(pane))?;
    let mut clicked = false;
    let size = egui::vec2(ui.available_width(), height.unwrap_or_else(|| ui.available_height()));
    ui.allocate_ui(size, |ui| {
        ui.horizontal(|ui| {
            let title = session
                .term
                .title()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| session.command.clone());
            ui.label(RichText::new(format!("📺 {}", shorten(&title, 60))).monospace().color(Palette::TEXT_SECONDARY()));
            if broadcast {
                ui.label(RichText::new("⇉ broadcast").strong().color(Palette::ACCENT_AMBER()))
                    .on_hover_text("Keys typed here also go to the other pane (:broadcast off to stop)");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if exit_keys.is_empty() { "Exit".to_string() } else { format!("Exit ({exit_keys})") };
                if ui.small_button(RichText::new(label).color(Palette::ACCENT_RED())).clicked() {
                    let _ = session.pty.kill();
                }
            });
        });
        let response = session.draw_surface(ctx, ui, focused, window_focused);
        // A press anywhere on the terminal focuses its pane. Read from this frame's
        // events: when press and release arrive in the same frame, egui has already
        // cleared `press_origin` and `response.clicked()` does not fire.
        let pressed_inside = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::PointerButton { pos, pressed: true, .. } if response.rect.contains(*pos))
            })
        });
        clicked = response.clicked() || response.drag_started() || pressed_inside;
    });
    Some(clicked)
}

/// Draws a read-only copy of a running program's screen in a Mosaic tile, with a
/// monospace font small enough for its columns to fit the tile's width. The program's
/// PTY keeps its size; the screen is clipped to `height`.
fn draw_mosaic_tui(ui: &mut egui::Ui, tab_id: Uuid, screen: &mrt_core::TermSnapshot, height: f32) {
    let width = ui.available_width();
    // A monospace glyph is about 0.6 em wide.
    let size = (width / (screen.cols.max(1) as f32 * 0.6)).clamp(5.0, 12.0);
    let font = egui::FontId::monospace(size);
    let cell = crate::widgets::term_view::cell_size(ui, &font);
    ui.allocate_ui(egui::vec2(width, height), |ui| {
        let clip = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, height));
        ui.set_clip_rect(clip.intersect(ui.clip_rect()));
        crate::widgets::term_view::paint_terminal(ui, egui::Id::new(("mosaic_tui", tab_id)), screen, &font, cell, false);
    });
}

/// Sends this frame's mouse input over the TUI grid (`rect`, cells of `cell` size) to
/// the program: presses, releases and motion as mouse reports when it asked for them,
/// and the wheel as mouse reports or, on an alternate screen with mode 1007, as arrows.
fn forward_tui_mouse(ctx: &egui::Context, session: &mut ActiveTuiSession, rect: egui::Rect, cell: egui::Vec2, modes: &mrt_core::TermModes) {
    use crate::term_input::{encode_mouse, wheel_as_arrows, MouseAction, MouseButton};
    let (cols, rows) = (session.term.cols(), session.term.rows());
    let to_cell = |pos: egui::Pos2| -> (u16, u16) {
        let col = ((pos.x - rect.left()) / cell.x).floor().clamp(0.0, (cols - 1) as f32) as u16;
        let row = ((pos.y - rect.top()) / cell.y).floor().clamp(0.0, (rows - 1) as f32) as u16;
        (col, row)
    };
    let (events, hover) = ctx.input(|i| (i.events.clone(), i.pointer.hover_pos()));
    let mut out: Vec<u8> = Vec::new();
    for event in events {
        match event {
            egui::Event::PointerButton { pos, button, pressed, modifiers } => {
                let Some(button) = MouseButton::from_egui(button) else { continue };
                // Selection: a left drag when the program does not take the mouse, or
                // with Shift held, as in xterm.
                let selects = modes.mouse == mrt_core::MouseMode::None || modifiers.shift;
                if button == MouseButton::Left && pressed && rect.contains(pos) && selects {
                    let at = to_cell(pos);
                    session.selection = Some(crate::widgets::term_view::Selection { anchor: at, head: at });
                    session.selecting = true;
                    continue;
                }
                if button == MouseButton::Left && !pressed && session.selecting {
                    session.selecting = false;
                    // A click without a drag selects nothing.
                    if session.selection.is_some_and(|s| s.anchor == s.head) {
                        session.selection = None;
                    }
                    continue;
                }
                if pressed && !rect.contains(pos) {
                    continue;
                }
                if !pressed && session.mouse_held != Some(button) {
                    continue;
                }
                session.mouse_held = pressed.then_some(button);
                let (col, row) = to_cell(pos);
                session.mouse_cell = Some((col, row));
                let action = if pressed { MouseAction::Press(button) } else { MouseAction::Release(button) };
                out.extend(encode_mouse(action, col, row, modifiers, modes).unwrap_or_default());
            }
            egui::Event::PointerMoved(pos) => {
                if session.selecting {
                    if let Some(selection) = session.selection.as_mut() {
                        selection.head = to_cell(pos);
                    }
                    continue;
                }
                if !rect.contains(pos) && session.mouse_held.is_none() {
                    continue;
                }
                let at = to_cell(pos);
                if session.mouse_cell == Some(at) {
                    continue;
                }
                session.mouse_cell = Some(at);
                let mods = ctx.input(|i| i.modifiers);
                out.extend(encode_mouse(MouseAction::Move(session.mouse_held), at.0, at.1, mods, modes).unwrap_or_default());
            }
            egui::Event::MouseWheel { unit, delta, modifiers } => {
                let Some(pos) = hover.filter(|p| rect.contains(*p)) else { continue };
                // Positive y moves the content down, i.e. scrolls up.
                let notches = match unit {
                    egui::MouseWheelUnit::Line => delta.y,
                    egui::MouseWheelUnit::Point => delta.y / cell.y,
                    egui::MouseWheelUnit::Page => delta.y * rows as f32,
                };
                session.wheel_accum += notches;
                let (col, row) = to_cell(pos);
                while session.wheel_accum.abs() >= 1.0 {
                    let up = session.wheel_accum > 0.0;
                    session.wheel_accum -= session.wheel_accum.signum();
                    let action = if up { MouseAction::WheelUp } else { MouseAction::WheelDown };
                    let bytes = encode_mouse(action, col, row, modifiers, modes).or_else(|| wheel_as_arrows(up, modes));
                    out.extend(bytes.unwrap_or_default());
                }
            }
            _ => {}
        }
    }
    if !out.is_empty() {
        let _ = session.pty.send_bytes(&out);
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

/// Builds the key resolver: the default keymap plus user overrides
/// (`binding -> action`) from the config file's `[keybindings]` table.
/// `canonicalize` on Windows returns `\\?\C:\...`; drop that prefix for display and shells.
fn dunce_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

pub(crate) fn load_key_resolver(overrides: &HashMap<String, String>) -> (KeyResolver, Vec<String>) {
    let (keymap, issues) = Keymap::with_overrides(overrides, Platform::current());
    (KeyResolver::new(keymap), issues.iter().map(|i| i.to_string()).collect())
}

impl MrtGuiApp {
    fn tab_status(tab: &TerminalTab) -> ActivitySnapshot {
        tab.activity.snapshot(Instant::now())
    }

    /// Lets each tab's activity tracker recognise agent CLIs (`claude`, `codex`, ...) started
    /// through wrapper scripts, from the process tree of its running commands.
    fn observe_activity_processes(&mut self, snapshot: &ProcessSnapshot) {
        let mut roots: Vec<(Uuid, u32)> = Vec::new();
        if let Ok(pids) = self.running_pids.lock() {
            for (block_id, pid) in pids.iter() {
                if let Some((tab_id, _)) = self.running_output.get(block_id) {
                    roots.push((*tab_id, *pid));
                }
            }
        }
        for session in self.active_tui.iter().chain(self.pane_tuis.iter()) {
            if let Some(pid) = session.pty.process_id() {
                roots.push((session.tab_id, pid));
            }
        }
        for (tab_id, pid) in roots {
            let names: Vec<&str> = snapshot
                .get(pid)
                .into_iter()
                .chain(snapshot.descendants(pid).into_iter().map(|(_, e)| e))
                .map(|e| e.name.as_str())
                .collect();
            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                tab.activity.observe_process_names(names);
            }
        }
    }

    /// Opens a remote tab (`:ssh user@host [ssh options]`) and runs `hostname` there, which
    /// connects the session and shows which machine the tab is on.
    fn open_remote_tab(&mut self, args: Vec<String>) {
        let shell_idx = self.tabs.get(self.active_tab).map(|t| t.active_shell_idx).unwrap_or(0);
        let host = args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| args.join(" "));
        let mut tab = TerminalTab::new(format!("ssh {host}"), shell_idx, std::env::temp_dir());
        tab.remote = Some(args);
        let tab_id = tab.id;
        let cwd = tab.cwd.clone();
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        if let Some(shell) = self.shells.get(shell_idx).cloned() {
            self.spawn_shell_command(tab_id, shell, cwd, "hostname".to_string());
        }
    }

    /// Runs an `[[actions]]` entry: one shell command is submitted like typed input;
    /// several lines (or MRT `:` commands) run in order as a runbook in the active tab.
    pub(crate) fn run_named_action(&mut self, id: &str) {
        let Some(entry) = self.config_state.config.actions.iter().find(|a| a.id() == id).cloned() else {
            self.runbooks_state.replay_notice = Some(format!("No [[actions]] entry named '{id}' in the config."));
            return;
        };
        let lines = entry.command_lines();
        match lines.as_slice() {
            [] => {}
            [only] => self.submit_input(Some(only.clone()), false),
            _ => {
                let shell_name = self
                    .tabs
                    .get(self.active_tab)
                    .and_then(|t| self.shells.get(t.active_shell_idx))
                    .map(|s| s.kind.short_label().to_string())
                    .unwrap_or_default();
                let steps = lines
                    .into_iter()
                    .map(|command| crate::history::RunbookStep { command, shell_name: shell_name.clone(), cwd: String::new() })
                    .collect();
                self.start_runbook_replay(crate::history::Runbook {
                    id: Uuid::new_v4(),
                    name: entry.title.clone().unwrap_or(entry.name.clone()),
                    steps,
                    updated_at: chrono::Utc::now(),
                });
            }
        }
    }

    /// Named actions for the Command Palette: (action, title, key hint).
    fn named_palette_actions(&self) -> Vec<(Action, String, String)> {
        self.config_state
            .config
            .actions
            .iter()
            .map(|entry| {
                let action = Action::RunNamed(entry.id());
                let title = entry.title.clone().unwrap_or_else(|| entry.name.clone());
                let keys = self.key_resolver.keymap().bindings_for(&action).join(" / ");
                let hint = if keys.is_empty() { action.name() } else { keys };
                (action, title, hint)
            })
            .collect()
    }

    /// Starts the `[[hooks]]` that apply to `event`.
    pub(crate) fn fire_hooks(&self, event: crate::hooks::HookEvent) {
        let hooks = &self.config_state.config.hooks;
        if hooks.is_empty() || self.shells.is_empty() {
            return;
        }
        let idx = crate::config_runtime::default_shell_index(&self.shells, &self.config_state.config);
        crate::hooks::fire(&self.runtime, hooks, &self.shells[idx], &event);
    }

    /// Quick terminal: shows the window in front (as a drop-down across the top of the
    /// screen when `terminal.quick_dropdown` is on) or minimizes it. The window is
    /// minimized rather than hidden, so it can always be brought back from the taskbar.
    fn apply_window_action(&mut self, ctx: &egui::Context) {
        let Some(action) = self.pending_window.take() else { return };
        let (focused, minimized, monitor) =
            ctx.input(|i| (i.viewport().focused.unwrap_or(false), i.viewport().minimized.unwrap_or(false), i.viewport().monitor_size));
        let show = match action.as_str() {
            "show" => true,
            "hide" => false,
            _ => minimized || !focused,
        };
        if show {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            let terminal = &self.config_state.config.terminal;
            if let (true, Some(monitor)) = (terminal.quick_dropdown, monitor) {
                let height = monitor.y * (terminal.quick_height_percent.clamp(20, 100) as f32 / 100.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(monitor.x, height)));
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    /// Handles copy-mode keys (taking them away from the prompt) and draws its panel.
    fn update_copy_mode(&mut self, ctx: &egui::Context) {
        let Some(mut mode) = self.copy_mode.take() else { return };
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        let keys: Vec<crate::copy_mode::CopyKey> = ctx.input_mut(|i| {
            let mut keys = Vec::new();
            i.events.retain(|event| match event {
                egui::Event::Key { key, pressed, modifiers, .. } => {
                    if let Some(k) = crate::copy_mode::key_for(*key, *modifiers) {
                        if *pressed {
                            keys.push(k);
                        }
                        false
                    } else {
                        true
                    }
                }
                // Typed letters are copy-mode keys, not prompt text.
                egui::Event::Text(_) => false,
                _ => true,
            });
            keys
        });
        for key in keys {
            match mode.apply(key, &tab.blocks) {
                crate::copy_mode::CopyOutcome::Stay => {}
                crate::copy_mode::CopyOutcome::Copy(text) => {
                    ctx.copy_text(text);
                    return;
                }
                crate::copy_mode::CopyOutcome::Exit => return,
            }
        }
        crate::copy_mode::render(ctx, &mode, &tab.blocks);
        self.copy_mode = Some(mode);
    }

    /// `:assist`: a multi-step investigation with MRT's read-only MCP tools (see
    /// `mrt_ai::assist`). Each tool call streams into the block; the answer and any
    /// suggested command land in the block's AI panel, whose "Apply fix" goes through
    /// the Safety Gate like typed input.
    fn spawn_assist(&mut self, tab_id: Uuid, cwd: PathBuf, shell_label: String, raw_input: String, question: String) {
        // The tools read the MCP mirror, so bring it up to date first.
        self.sync_mcp_from_gui();
        let state = self.mcp_host.local_server_state.clone();
        let mut running = CommandBlock::new_completed(&raw_input, "mrt-ai", cwd.display().to_string(), "", 0, 0);
        running.exit_code = None;
        let block_id = running.id;
        let timestamp = running.timestamp;
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.blocks.push(running);
        }
        let output = std::sync::Arc::new(std::sync::Mutex::new(format!("Question: {question}\n")));
        self.running_output.insert(block_id, (tab_id, output.clone()));
        let tx = self.tx.clone();
        self.busy_count += 1;
        self.runtime.spawn(async move {
            let started = Instant::now();
            let mut trace = format!("Question: {question}\n");
            let outcome = mrt_ai::run_assist(
                &question,
                &shell_label,
                &cwd.display().to_string(),
                |tool, arguments| {
                    let state = state.clone();
                    async move {
                        let request = mrt_mcp::JsonRpcRequest {
                            jsonrpc: "2.0".into(),
                            id: Some(serde_json::json!(1)),
                            method: "tools/call".into(),
                            params: Some(serde_json::json!({ "name": tool, "arguments": arguments })),
                        };
                        match mrt_mcp::handle_mcp_jsonrpc(&state, request).await.and_then(|r| r.result) {
                            Some(result) => result["content"]
                                .as_array()
                                .map(|items| items.iter().filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n"))
                                .unwrap_or_default(),
                            None => "The tool returned no result.".to_string(),
                        }
                    }
                },
                |step| {
                    let line = format!("→ {} {}\n", step.tool, step.arguments);
                    trace.push_str(&line);
                    if let Ok(mut pending) = output.lock() {
                        pending.push_str(&line);
                    }
                },
            )
            .await;
            trace.push_str(&format!("\nAnswered by {} after {} tool call(s).\n", outcome.provider, outcome.steps.len()));
            let mut block = CommandBlock::new_completed(
                &raw_input,
                "mrt-ai",
                cwd.display().to_string(),
                trace,
                0,
                started.elapsed().as_millis() as u64,
            );
            block.id = block_id;
            block.timestamp = timestamp;
            block.ai_explanation = Some(outcome.answer);
            block.ai_suggested_fix = outcome.command;
            let _ = tx.send(AsyncWorkerEvent::BlockCompleted { tab_id, block, updated_cwd: None });
        });
    }

    /// Opens an agent sandbox tab rooted at `root` (`:sandbox [folder]`).
    pub(crate) fn open_sandbox_tab(&mut self, root: PathBuf) {
        let root = match std::fs::canonicalize(&root) {
            Ok(path) if path.is_dir() => dunce_path(path),
            _ => {
                let cwd = self.tabs[self.active_tab].cwd.clone();
                let tab_id = self.tabs[self.active_tab].id;
                self.push_block(tab_id, Self::process_error_block(":sandbox", &cwd, format!("{} is not a folder.", root.display())));
                return;
            }
        };
        let shell_idx = self.tabs.get(self.active_tab).map(|t| t.active_shell_idx).unwrap_or(0);
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        let mut tab = TerminalTab::new(format!("sandbox {name}"), shell_idx, root.clone());
        tab.sandbox_root = Some(root);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
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
            cwd.clone(),
        ));
        self.active_tab = self.tabs.len() - 1;
        self.fire_hooks(crate::hooks::HookEvent {
            event: "tab_opened",
            cwd: cwd.display().to_string(),
            tab_title: self.tabs[self.active_tab].title.clone(),
            ..Default::default()
        });
    }

    /// Runs every key press through the keymap. Keys that complete a binding or
    /// are mid-chain are removed from the input, so neither egui widgets nor the
    /// TUI PTY see them; unbound keys pass through untouched.
    fn handle_keybindings(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let context = KeyContext { tui_active: self.focused_tui().is_some() };
        self.key_resolver.tick(now);
        let mut actions = Vec::new();
        let resolver = &mut self.key_resolver;
        ctx.input_mut(|i| {
            // egui-winit emits `Text` right after the `Key` that typed it; drop
            // that text too when the key was consumed (e.g. the `c` of `ctrl+a > c`).
            let mut drop_next_text = false;
            i.events.retain(|event| match event {
                egui::Event::Key { key, pressed: true, repeat, modifiers, .. } => {
                    match resolver.feed(KeyChord::from_egui(*key, *modifiers), *repeat, now, context) {
                        Resolution::Matched(action) => {
                            actions.push(action);
                            drop_next_text = true;
                            false
                        }
                        Resolution::Pending => {
                            drop_next_text = true;
                            false
                        }
                        Resolution::NoMatch => {
                            drop_next_text = false;
                            true
                        }
                    }
                }
                egui::Event::Text(_) if drop_next_text => {
                    drop_next_text = false;
                    false
                }
                _ => true,
            });
        });
        for action in actions {
            self.dispatch_action(action);
        }
        if let Some(wait) = self.key_resolver.next_deadline(now) {
            ctx.request_repaint_after(wait);
        }
    }

    /// Performs a keymap action (from a key binding or the Command Palette).
    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::ToggleCommandPalette => self.show_command_palette = !self.show_command_palette,
            Action::ExitTui => self.tui_exit_requested = true,
            Action::NewTab => {
                let shell_idx = self.tabs.get(self.active_tab).map(|t| t.active_shell_idx).unwrap_or(0);
                if shell_idx < self.shells.len() {
                    self.open_new_tab(shell_idx);
                }
            }
            Action::CloseTab => {
                if let Some(id) = self.tabs.get(self.active_tab).map(|t| t.id) {
                    self.close_terminal(id);
                }
            }
            Action::NextTab => {
                if !self.tabs.is_empty() {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                }
            }
            Action::PreviousTab => {
                if !self.tabs.is_empty() {
                    self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                }
            }
            Action::SplitHorizontal | Action::SplitVertical | Action::Unsplit | Action::ClearBlocks => {
                let shells_count = self.shells.len();
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    match action {
                        Action::SplitHorizontal => tab.split(SplitDirection::Horizontal, shells_count),
                        Action::SplitVertical => tab.split(SplitDirection::Vertical, shells_count),
                        Action::Unsplit => tab.close_split(),
                        _ => {
                            tab.blocks.clear();
                            tab.comparison = None;
                        }
                    }
                }
            }
            Action::ToggleExplorer => self.show_explorer = !self.show_explorer,
            Action::ToggleHistory => {
                self.show_history = !self.show_history;
                if self.show_history {
                    self.refresh_history();
                }
            }
            Action::ToggleRunbooks => {
                self.runbooks_state.show_panel = !self.runbooks_state.show_panel;
                if self.runbooks_state.show_panel {
                    self.refresh_runbooks();
                    self.refresh_history();
                }
            }
            Action::ToggleMcpHub => self.side_state.show_mcp_panel = !self.side_state.show_mcp_panel,
            Action::ToggleToolbox => {
                self.side_state.show_toolbox_panel = !self.side_state.show_toolbox_panel;
            }
            Action::ToggleMosaic => self.mosaic = !self.mosaic,
            Action::OpenTaskManager => self.open_task_manager(None),
            Action::Find => self.find.open(),
            Action::CopyMode => {
                self.copy_mode = self.tabs.get(self.active_tab).and_then(|t| crate::copy_mode::CopyMode::new(&t.blocks));
            }
            Action::ToggleBroadcast => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.broadcast = !tab.broadcast;
                }
            }
            Action::RunNamed(name) => self.run_named_action(&name),
            // The resolver itself tracks the active key table.
            Action::EnterTable(_) | Action::ExitTable => {}
        }
    }

    /// Small bottom-right badge while a chain is half-typed or a key table is active.
    fn render_key_indicator(&self, ctx: &egui::Context) {
        let Some(label) = self.key_resolver.indicator() else {
            return;
        };
        egui::Area::new(egui::Id::new("keymap_pending_indicator"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -16.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(Palette::BG_ELEVATED())
                    .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT()))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new(format!("⌨ {label}")).monospace().size(12.0).color(Palette::TEXT_BODY()));
                    });
            });
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
                    .fill(Palette::BG_SIDEBAR())
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
                                .color(Palette::TEXT_PRIMARY()),
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
                                .color(Palette::TEXT_PRIMARY()),
                        )
                        .fill(Color32::from_rgb(26, 26, 30))
                        .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE()))
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
                let folder_of = |tab: &TerminalTab| {
                    tab.cwd
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| tab.cwd.display().to_string())
                };
                let mut name_counts: HashMap<String, usize> = HashMap::new();
                for tab in &self.tabs {
                    if tab.title.starts_with("Tab ") || tab.title.starts_with("Terminal ") {
                        *name_counts.entry(folder_of(tab)).or_default() += 1;
                    }
                }
                for (idx, tab) in self.tabs.iter().enumerate() {
                    let shell = self
                        .shells
                        .get(tab.active_shell_idx)
                        .map(|s| s.kind.short_label())
                        .unwrap_or("shell");
                    let folder = folder_of(tab);
                    let generic = tab.title.starts_with("Tab ") || tab.title.starts_with("Terminal ");
                    let title = if !generic {
                        tab.title.clone()
                    } else if name_counts.get(&folder).copied().unwrap_or(0) > 1 {
                        format!("{folder} · {}", idx + 1)
                    } else {
                        folder.clone()
                    };
                    let status = Self::tab_status(tab);
                    let subtitle = if generic {
                        format!("{shell} · {}", status.state.label())
                    } else {
                        format!("{shell} · {folder}")
                    };
                    let badge_top = crate::notifications::row_top(ui);
                    let row = tab_row(
                        ui,
                        &title,
                        &subtitle,
                        &status,
                        idx == self.active_tab,
                        closable,
                        compact,
                    );
                    self.notifier.paint_badge(ui, badge_top, tab.id, compact);
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
                if nav_row(ui, "🧰", "Toolbox", None, self.side_state.show_toolbox_panel, compact).clicked() {
                    self.side_state.show_toolbox_panel = !self.side_state.show_toolbox_panel;
                }
                if nav_row(ui, "⊞", "Mosaic", None, self.mosaic, compact).clicked() {
                    self.mosaic = !self.mosaic;
                }
                if nav_row(ui, "🔔", "Notifications", None, self.notifier.show_settings, compact).clicked() {
                    self.notifier.show_settings = !self.notifier.show_settings;
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
                        .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT()))
                        .rounding(egui::Rounding::same(12.0))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 4.0;
                            let gate_on = self.config_state.config.safety.enabled;
                            ui.horizontal(|ui| {
                                let dot = if gate_on { Palette::ACCENT_GREEN() } else { Palette::ACCENT_AMBER() };
                                let label = if gate_on { "Safety Gate on" } else { "Safety Gate off (config)" };
                                ui.label(RichText::new("●").size(10.0).color(dot));
                                ui.label(RichText::new(label).size(13.0).color(Palette::TEXT_PRIMARY()));
                            });
                            let bridge = match self.bridge_port {
                                Some(port) => format!("MCP bridge · port {port}"),
                                None => "MCP bridge offline".to_string(),
                            };
                            ui.label(RichText::new(bridge).size(12.0).color(Palette::TEXT_MUTED()));
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
                    .fill(Palette::BG_BASE())
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
                        ui.label(RichText::new(parent).monospace().size(13.0).color(Palette::TEXT_SECONDARY()))
                            .on_hover_text(&full);
                        ui.label(RichText::new(leaf).monospace().size(13.0).color(Palette::TEXT_PRIMARY()))
                            .on_hover_text(&full);
                        ui.spacing_mut().item_spacing.x = 8.0;
                    }
                    if self.mosaic {
                        ui.add_space(10.0);
                        chip(ui, "Mosaic", Palette::BG_OVERLAY(), Palette::TEXT_BODY());
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
                        let palette_keys = self
                            .key_resolver
                            .keymap()
                            .bindings_for(&Action::ToggleCommandPalette)
                            .into_iter()
                            .next()
                            .unwrap_or_default();
                        if search_pill(ui, width, "Commands, tabs, tools…", &palette_keys).clicked() {
                            self.show_command_palette = !self.show_command_palette;
                        }
                    });
                });
            });
    }
}

impl eframe::App for MrtGuiApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_layout(true);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.persist_layout(false);
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

        self.update_config(ctx);
        self.process_async_events();
        self.process_history_events();
        self.process_mcp_ui_commands();
        self.handle_keybindings(ctx);
        self.update_copy_mode(ctx);
        self.apply_window_action(ctx);
        self.process_tui_session(ctx);
        self.maybe_spawn_process_tick();
        if let Some(idx) = self.notifier.update(ctx, &self.tabs, self.active_tab, self.mosaic) {
            self.active_tab = idx;
        }

        // Running commands stream output through shared buffers (not the channel), so
        // poll while busy; otherwise wait for an event or the idle tick.
        ctx.request_repaint_after(if self.busy_count > 0 || self.history_pending > 0 || !self.running_output.is_empty() {
            std::time::Duration::from_millis(50)
        } else {
            IDLE_REPAINT
        });

        self.render_sidebar(ctx);
        self.render_header(ctx);
        self.render_config_notice(ctx);

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

        // Right-hand Side Panels (MCP Hub & Toolbox)
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
        if self.side_state.show_toolbox_panel {
            egui::SidePanel::right("mrt_toolbox_panel").frame(panel_frame())
                .default_width(380.0)
                .resizable(true)
                .show(ctx, |ui| {
                    if let Some(ev) = render_toolbox_side_panel(ui, &mut self.side_state) {
                        panel_event = Some(ev);
                    }
                });
        }
        if let Some(ev) = panel_event {
            self.handle_side_panel_event(ev);
        }

        focus_prompt |= previous_tab != self.tabs[self.active_tab].id || (palette_was_open && !self.show_command_palette);
        let show_composer = !self.mosaic || self.tabs[self.active_tab].ai_proposal.is_some();

        // Floating composer: AI suggestion card + command input card
        egui::TopBottomPanel::bottom("mrt_bottom_prompt")
            .show_separator_line(false)
            .frame(egui::Frame::none().fill(Palette::BG_BASE()).inner_margin(egui::Margin {
                left: 20.0,
                right: 20.0,
                top: 6.0,
                bottom: 18.0,
            }))
            .show_animated(ctx, show_composer, |ui| {
                centered_column(ui, |ui| {
                    if !self.show_history {
                        if let Some(error) = &self.history_error {
                            ui.colored_label(Palette::ACCENT_RED(), error);
                        }
                    }

                    // AI command proposal card
                    let mut run_ai_cmd = None;
                    let mut dismiss_ai = false;
                    if let Some(proposal) = &self.tabs[self.active_tab].ai_proposal {
                        egui::Frame::none()
                            .fill(Palette::AI_FILL())
                            .stroke(egui::Stroke::new(1.0_f32, Palette::AI_BORDER()))
                            .rounding(egui::Rounding::same(14.0))
                            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("✨").size(14.0).color(Palette::ACCENT()));
                                    ui.label(
                                        RichText::new("Suggested command")
                                            .size(13.0)
                                            .strong()
                                            .color(Palette::ACCENT_TEXT()),
                                    );
                                    ui.label(
                                        RichText::new(&proposal.provider_used)
                                            .size(12.0)
                                            .color(Palette::TEXT_SECONDARY()),
                                    );
                                });
                                if !proposal.explanation.trim().is_empty() {
                                    ui.label(
                                        RichText::new(&proposal.explanation)
                                            .size(13.5)
                                            .color(Palette::TEXT_BODY()),
                                    );
                                }
                                ui.add_space(4.0);
                                egui::Frame::none()
                                    .fill(Palette::BG_INSET())
                                    .rounding(egui::Rounding::same(Radius::BUTTON))
                                    .inner_margin(egui::Margin { left: 12.0, right: 8.0, top: 6.0, bottom: 6.0 })
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(&proposal.proposed_command)
                                                    .monospace()
                                                    .size(13.0)
                                                    .color(Palette::TEXT_PRIMARY()),
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
                        && self.focused_tui().is_none();
                    let tui_active = self.focused_tui().is_some();
                    let shells = &self.shells;
                    if let Some(tab) = self.tabs.get_mut(active_idx).filter(|_| !mosaic) {
                        egui::Frame::none()
                            .fill(Palette::BG_ELEVATED())
                            .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_STRONG()))
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
                                                .color(Palette::TEXT_BODY()),
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
                                    ui.label(RichText::new("🛡").size(12.0).color(Palette::ACCENT_GREEN()));
                                    ui.label(RichText::new("Safety Gate on").size(12.0).color(Palette::TEXT_MUTED()));

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let has_input = !tab.input_line.trim().is_empty();
                                        let run = egui::Button::new(
                                            RichText::new("→")
                                                .size(17.0)
                                                .strong()
                                                .color(if has_input { Palette::ON_ACCENT() } else { Palette::TEXT_MUTED() }),
                                        )
                                        .fill(if has_input { Palette::ACCENT() } else { Palette::BG_OVERLAY() })
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
                    .fill(Palette::BG_BASE())
                    .inner_margin(egui::Margin { left: 20.0, right: 20.0, top: 12.0, bottom: 4.0 }),
            )
            .show(ctx, |ui| {
            self.render_find_bar(ui);
            // While the find bar is open, block lists stop sticking to the end so a match
            // above can be scrolled into view (sticking overrides programmatic scrolls).
            let stick_to_end = !self.find.open;
            if self.mosaic {
                self.render_mosaic(ui);
                return;
            }
            let mut pending_block_action = None;
            // Inline TUI sessions in split panes take the keyboard only when no overlay or
            // modal is open.
            let pane_focus_allowed =
                self.active_tui.is_none() && !self.show_command_palette && self.pending_safety.is_none();
            let window_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
            let exit_keys = self
                .key_resolver
                .keymap()
                .bindings_for(&Action::ExitTui)
                .into_iter()
                .next()
                .unwrap_or_default();

            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                if tab.split_direction == SplitDirection::Horizontal && tab.secondary_pane.is_some() {
                    ui.columns(2, |cols| {
                        // Left column: Primary pane
                        cols[0].group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Pane 1 (Primary)").strong().color(
                                    if tab.active_pane_idx == 0 { Palette::ACCENT_BLUE() } else { Palette::TEXT_MUTED() }
                                ));
                                if tab.active_pane_idx != 0 && ui.small_button("Focus").clicked() {
                                    tab.active_pane_idx = 0;
                                }
                            });
                            if let Some(clicked) = draw_pane_tui(ctx, ui, &mut self.pane_tuis, tab.id, 0, None, pane_focus_allowed && tab.active_pane_idx == 0, window_focused, &exit_keys, tab.broadcast) {
                                if clicked {
                                    tab.active_pane_idx = 0;
                                }
                            } else {
                            egui::ScrollArea::vertical()
                                .id_salt("split_pane_primary")
                                .auto_shrink([false, false])
                                .stick_to_bottom(stick_to_end)
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
                            }
                        });

                        // Right column: Secondary pane
                        cols[1].group(|ui| {
                            let sec = tab.secondary_pane.as_mut().unwrap();
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Pane 2 (Split)").strong().color(
                                    if tab.active_pane_idx == 1 { Palette::ACCENT_GREEN() } else { Palette::TEXT_MUTED() }
                                ));
                                if tab.active_pane_idx != 1 && ui.small_button("Focus").clicked() {
                                    tab.active_pane_idx = 1;
                                }
                            });
                            if let Some(clicked) = draw_pane_tui(ctx, ui, &mut self.pane_tuis, tab.id, 1, None, pane_focus_allowed && tab.active_pane_idx == 1, window_focused, &exit_keys, tab.broadcast) {
                                if clicked {
                                    tab.active_pane_idx = 1;
                                }
                            } else {
                            egui::ScrollArea::vertical()
                                .id_salt("split_pane_secondary")
                                .auto_shrink([false, false])
                                .stick_to_bottom(stick_to_end)
                                .show(ui, |ui| {
                                    if sec.blocks.is_empty() {
                                        ui.label(RichText::new("Secondary pane active. Commands run here when focused.").italics().color(Palette::TEXT_MUTED()));
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
                            }
                        });
                    });
                } else if tab.split_direction == SplitDirection::Vertical && tab.secondary_pane.is_some() {
                    let half_h = (ui.available_height() / 2.0 - 10.0).max(100.0);
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Pane 1 (Top)").strong().color(
                                if tab.active_pane_idx == 0 { Palette::ACCENT_BLUE() } else { Palette::TEXT_MUTED() }
                            ));
                            if tab.active_pane_idx != 0 && ui.small_button("Focus").clicked() {
                                tab.active_pane_idx = 0;
                            }
                        });
                        if let Some(clicked) = draw_pane_tui(ctx, ui, &mut self.pane_tuis, tab.id, 0, Some(half_h), pane_focus_allowed && tab.active_pane_idx == 0, window_focused, &exit_keys, tab.broadcast) {
                                if clicked {
                                    tab.active_pane_idx = 0;
                                }
                            } else {
                            egui::ScrollArea::vertical()
                            .id_salt("split_v_primary")
                            .max_height(half_h)
                            .stick_to_bottom(stick_to_end)
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
                            }
                    });

                    ui.separator();

                    ui.group(|ui| {
                        let sec = tab.secondary_pane.as_mut().unwrap();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Pane 2 (Bottom)").strong().color(
                                if tab.active_pane_idx == 1 { Palette::ACCENT_GREEN() } else { Palette::TEXT_MUTED() }
                            ));
                            if tab.active_pane_idx != 1 && ui.small_button("Focus").clicked() {
                                tab.active_pane_idx = 1;
                            }
                        });
                        if let Some(clicked) = draw_pane_tui(ctx, ui, &mut self.pane_tuis, tab.id, 1, Some(half_h), pane_focus_allowed && tab.active_pane_idx == 1, window_focused, &exit_keys, tab.broadcast) {
                                if clicked {
                                    tab.active_pane_idx = 1;
                                }
                            } else {
                            egui::ScrollArea::vertical()
                            .id_salt("split_v_secondary")
                            .max_height(half_h)
                            .stick_to_bottom(stick_to_end)
                            .show(ui, |ui| {
                                if sec.blocks.is_empty() {
                                    ui.label(RichText::new("Secondary pane active. Commands run here when focused.").italics().color(Palette::TEXT_MUTED()));
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
                            }
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(stick_to_end)
                        .show(ui, |ui| centered_column(ui, |ui| {
                            ui.add_space(8.0);
                            if tab.blocks.is_empty() {
                                ui.add_space((ui.available_height() * 0.28).max(48.0));
                                ui.vertical_centered(|ui| {
                                    brand_mark(ui, 40.0);
                                    ui.add_space(14.0);
                                    ui.label(RichText::new("Ready when you are").size(22.0).strong().color(Palette::TEXT_PRIMARY()));
                                    ui.add_space(4.0);
                                    ui.label(RichText::new("Run any command below. Output lands here as blocks you can search, diff and diagnose.").size(14.0).color(Palette::TEXT_SECONDARY()));
                                    ui.add_space(18.0);
                                    for (keys, hint) in [
                                        ("#", "Describe what you want and AI writes the command"),
                                        (":http  :dns  :ports  :web", "Built-in network tools"),
                                        (":ps  :top  :kill", "Task Manager and process tools"),
                                        ("Ctrl K", "Command palette"),
                                    ] {
                                        ui.horizontal(|ui| {
                                            let total = 420.0_f32.min(ui.available_width());
                                            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                                            chip(ui, keys, Palette::BG_OVERLAY(), Palette::TEXT_BODY());
                                            ui.label(RichText::new(hint).size(13.0).color(Palette::TEXT_MUTED()));
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
                let is_kill = pending.kill.is_some();
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
                            .fill(Palette::DANGER_FILL())
                            .stroke(egui::Stroke::new(1.0_f32, Palette::DANGER_BORDER()))
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
                                        RichText::new(match (is_kill, critical) {
                                            (true, true) => "Stopping this can destabilise your system",
                                            (true, false) => "Stop these processes?",
                                            (false, true) => "This command can’t be undone",
                                            (false, false) => "Check this command before it runs",
                                        })
                                        .size(16.0)
                                        .strong()
                                        .color(Palette::TEXT_PRIMARY()),
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
                            .fill(Palette::BG_INSET())
                            .rounding(egui::Rounding::same(12.0))
                            .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(
                                    RichText::new(&pending.assessment.command)
                                        .monospace()
                                        .size(13.5)
                                        .color(Palette::TEXT_PRIMARY()),
                                );
                            });
                        ui.add_space(6.0);
                        let context_line = match &pending.kill {
                            Some(kill) => format!(
                                "Task Manager · {} target(s) · {}{}",
                                kill.pids.len(),
                                if kill.force { "force" } else { "graceful" },
                                if kill.tree { " · whole tree" } else { "" }
                            ),
                            None => format!("{} · {}", pending.shell.name, pending.cwd.display()),
                        };
                        ui.label(
                            RichText::new(context_line)
                                .size(12.0)
                                .monospace()
                                .color(Color32::from_rgb(189, 179, 172)),
                        );
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Esc to cancel").size(12.0).color(Color32::from_rgb(189, 179, 172)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let run = egui::Button::new(
                                    RichText::new(if is_kill { "Stop process" } else { "Run anyway" })
                                        .strong()
                                        .color(Color32::from_rgb(31, 10, 10)),
                                )
                                .fill(Palette::ACCENT_RED())
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
            let mut action_to_run: Option<Action> = None;
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
                        .fill(Palette::BG_ELEVATED())
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
                                ui.label(RichText::new("🔍").size(15.0).color(Palette::TEXT_SECONDARY()));
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
                                chip(ui, "Esc", Palette::BG_OVERLAY(), Palette::TEXT_SECONDARY());
                            });
                        });
                    ui.painter().hline(
                        ui.min_rect().x_range(),
                        ui.min_rect().bottom(),
                        egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT()),
                    );

                    let q = self.palette_query.trim().to_lowercase();
                    let matches = |title: &str, hint: &str| {
                        q.is_empty() || title.to_lowercase().contains(&q) || hint.to_lowercase().contains(&q)
                    };
                    let mut first_cmd: Option<String> = None;
                    let mut first_action: Option<Action> = None;
                    let mut first_shell: Option<usize> = None;
                    let mut results = 0usize;

                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if !self.keymap_issues.is_empty() {
                                    section_label(ui, "   Keybinding problems");
                                    for issue in &self.keymap_issues {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.add_space(12.0);
                                            ui.label(RichText::new(format!("⚠ {issue}")).size(12.0).color(Palette::ACCENT_AMBER()));
                                        });
                                    }
                                    ui.add_space(6.0);
                                }
                                // Bindable actions, each showing its current key binding
                                // (or its action name, ready to bind in `[keybindings]`).
                                let keymap = self.key_resolver.keymap();
                                let named = self.named_palette_actions();
                                let action_rows: Vec<(Action, String, String)> = PALETTE_ACTIONS
                                    .iter()
                                    .map(|action| {
                                        let keys = keymap.bindings_for(action).join(" / ");
                                        let hint = if keys.is_empty() { action.name() } else { keys };
                                        (action.clone(), action.title(), hint)
                                    })
                                    .chain(named)
                                    .filter(|(action, title, hint)| matches(title, &format!("{hint} {}", action.name())))
                                    .collect();
                                if !action_rows.is_empty() {
                                    section_label(ui, "   Actions");
                                    for (action, title, hint) in action_rows {
                                        results += 1;
                                        if first_cmd.is_none() && first_action.is_none() && first_shell.is_none() {
                                            first_action = Some(action.clone());
                                        }
                                        if palette_row(ui, "‖", &title, &hint).clicked() {
                                            action_to_run = Some(action);
                                            close_palette = true;
                                        }
                                    }
                                }
                                let sections: [(&str, &str, &[(&str, &str)]); 3] = [
                                    (
                                        "Tools",
                                        "🌐",
                                        &[
                                            ("Open Toolbox network tools", ":toolbox network"),
                                            ("Scan listening TCP/UDP ports", ":ports"),
                                            ("DNS and latency trace", ":dns google.com"),
                                            ("HTTP GET sample JSON API", ":http GET https://httpbin.org/json"),
                                            ("Read a web page as Markdown", ":web https://www.rust-lang.org"),
                                            ("Test the Safety Gate", "rm -rf ./important_production_data"),
                                        ],
                                    ),
                                    (
                                        "Processes",
                                        "📈",
                                        &[
                                            ("List processes by CPU", ":ps"),
                                            ("List processes by memory", ":ps --sort mem"),
                                            ("Processes started by MRT", ":ps --mrt --tree"),
                                            ("Live process monitor", ":top"),
                                            ("Kill process…", ":kill"),
                                            ("List process watches", ":watch-proc list"),
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
                                        if first_cmd.is_none() && first_action.is_none() && first_shell.is_none() {
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
                                        if first_cmd.is_none() && first_action.is_none() && first_shell.is_none() {
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
                                        ui.label(RichText::new("No matches. Press Enter to run it as a command.").size(13.0).color(Palette::TEXT_MUTED()));
                                    });
                                    ui.add_space(18.0);
                                }
                                ui.add_space(4.0);
                            });
                        });

                    if enter_pressed {
                        if let Some(action) = first_action.take() {
                            action_to_run = Some(action);
                        } else if let Some(cmd) = first_cmd.take() {
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
                        egui::Stroke::new(1.0_f32, Palette::BORDER_FAINT()),
                    );
                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(18.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 18.0;
                                for hint in ["Enter  Run first match", "Click  Run", "Esc  Close"] {
                                    ui.label(RichText::new(hint).size(12.0).color(Palette::TEXT_MUTED()));
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format!("{results} results"))
                                            .size(12.0)
                                            .color(Palette::TEXT_MUTED()),
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
            if let Some(action) = action_to_run {
                self.palette_query.clear();
                self.dispatch_action(action);
            }
            if close_palette || ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.show_command_palette = false;
                if self.pending_safety.is_none() {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new(("command_input", self.tabs[self.active_tab].id))));
                }
            }
        }

        self.notifier.render_settings(ctx);
        self.render_tui_overlay(ctx);
        self.render_key_indicator(ctx);
        self.sync_mcp_from_gui();
        // Keep spinners turning and let quiet-period heuristics flip to "waiting".
        if self.tabs.iter().any(|t| t.activity.running_commands() > 0 || is_animated(&Self::tab_status(t))) {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mrt_core::{ShellKind, ShellSession};

    fn key_event(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn keybindings_consume_bound_keys_and_pass_the_rest_through() {
        let mut app = MrtGuiApp::new();
        let ctx = egui::Context::default();
        let mut ctrl = egui::Modifiers::NONE;
        ctrl.ctrl = true;
        ctrl.command = !cfg!(target_os = "macos");
        let raw = egui::RawInput {
            events: vec![
                key_event(Key::K, ctrl),
                key_event(Key::A, egui::Modifiers::NONE),
                egui::Event::Text("a".into()),
                key_event(Key::Q, ctrl),
            ],
            ..Default::default()
        };
        let mut remaining = Vec::new();
        let _ = ctx.run(raw, |ctx| {
            app.handle_keybindings(ctx);
            remaining = ctx.input(|i| i.events.clone());
        });
        assert!(app.show_command_palette, "Ctrl+K toggles the palette");
        // `a` (and its text) plus Ctrl+Q outside TUI mode reach the widgets/shell.
        assert_eq!(remaining.len(), 3, "{remaining:?}");
        assert!(!app.tui_exit_requested);
    }

    #[test]
    fn keymap_actions_drive_tabs_and_splits() {
        let mut app = MrtGuiApp::new();
        app.dispatch_action(Action::NewTab);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
        app.dispatch_action(Action::NextTab);
        assert_eq!(app.active_tab, 0);
        app.dispatch_action(Action::PreviousTab);
        assert_eq!(app.active_tab, 1);
        app.dispatch_action(Action::SplitVertical);
        assert_eq!(app.tabs[1].split_direction, SplitDirection::Vertical);
        app.dispatch_action(Action::Unsplit);
        assert_eq!(app.tabs[1].split_direction, SplitDirection::None);
        app.dispatch_action(Action::CloseTab);
        assert_eq!(app.tabs.len(), 1);
        app.dispatch_action(Action::ToggleMosaic);
        assert!(app.mosaic);
        app.dispatch_action(Action::ExitTui);
        assert!(app.tui_exit_requested);
        // Keyed actions do not pollute the per-tab command history.
        assert!(app.tabs[0].command_history.is_empty());
    }

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
            kill: None,
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
            kill: None,
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
    fn tab_activity_reaches_the_sidebar_status_and_mcp() {
        let mut app = MrtGuiApp::new();
        let now = Instant::now();
        let tab = &mut app.tabs[0];
        tab.activity.command_started("claude", now);
        tab.activity.feed_output("\x1b]678;status=waiting;message=Approve edit\x07", now);
        assert_eq!(
            MrtGuiApp::tab_status(&app.tabs[0]).state,
            mrt_core::ActivityState::WaitingForInput
        );
        app.sync_mcp_from_gui();
        let host = app.mcp_host.clone();
        let result = app
            .runtime
            .block_on(host.call_tool("mrt-native-mcp", "mrt_list_tabs", json!({})))
            .unwrap();
        let tabs: serde_json::Value = serde_json::from_str(&result.content[0].text).unwrap();
        let activity = &tabs[0]["activity"];
        assert_eq!(activity["state"], "waiting_for_input");
        assert_eq!(activity["agent"], "claude");
        assert_eq!(activity["detail"], "Approve edit");

        app.tabs[0].activity.command_finished(0, Instant::now());
        app.sync_mcp_from_gui();
        let result = app
            .runtime
            .block_on(host.call_tool("mrt-native-mcp", "mrt_get_session_state", json!({})))
            .unwrap();
        let state: serde_json::Value = serde_json::from_str(&result.content[0].text).unwrap();
        assert_eq!(state["tabs"][0]["activity"]["state"], "succeeded");
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
                    "mosaic_layout": "focus",
                    "explorer": true
                }),
            )
            .await
            .unwrap();
        });
        app.process_mcp_ui_commands();
        assert!(app.mosaic);
        assert_eq!(app.mosaic_columns, 3);
        assert_eq!(app.mosaic_layout, MosaicLayout::Focus);
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

    #[test]
    fn tui_in_a_split_tab_runs_inside_the_active_pane() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(":split".to_string()), false);
        assert!(app.tabs[0].secondary_pane.is_some());
        app.tabs[0].active_pane_idx = 1;

        app.submit_input(Some(":tui echo pane_tui_test".to_string()), false);
        assert!(app.active_tui.is_none(), "a split tab must not open the overlay");
        assert_eq!(app.pane_tuis.len(), 1);
        assert_eq!(app.pane_tuis[0].pane, Some(1));
        assert_eq!(app.focused_tui(), Some(FocusedTui::Pane(0)));

        let ctx = egui::Context::default();
        let start = Instant::now();
        while !app.pane_tuis.is_empty() && start.elapsed() < Duration::from_secs(10) {
            app.process_tui_session(&ctx);
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(app.pane_tuis.is_empty(), "the session did not end");
        // The block lands in the pane the program ran in.
        let secondary = app.tabs[0].secondary_pane.as_ref().unwrap();
        assert!(secondary.blocks.iter().any(|b| b.command.contains("pane_tui_test")));
        assert!(!app.tabs[0].blocks.iter().any(|b| b.command.contains("pane_tui_test")));
    }

    /// A remote tab's commands go to the SSH session and never run on this machine: with
    /// an unreachable host the first block reports the connection failure instead of the
    /// local `hostname`.
    #[test]
    fn suspend_resume_and_priority_commands_change_a_child_process() {
        let mut app = MrtGuiApp::new();
        let mut child = spawn_sleeper();
        let pid = child.id();
        for (command, expected) in [
            (format!(":suspend {pid}"), "Suspended"),
            (format!(":resume {pid}"), "Resumed"),
            (format!(":priority low {pid}"), "Priority set to below normal"),
        ] {
            app.submit_input(Some(command.clone()), false);
            pump_one(&mut app);
            let block = app.tabs[0].blocks.last().unwrap();
            assert_eq!(block.command, command);
            assert!(block.plain_output.contains(expected), "{command}: {}", block.plain_output);
            assert_eq!(block.exit_code, Some(0), "{}", block.plain_output);
        }
        let _ = child.kill();
        let _ = child.wait();
        app.submit_input(Some(format!(":suspend {}", std::process::id())), false);
        pump_one(&mut app);
        let block = app.tabs[0].blocks.last().unwrap();
        assert_eq!(block.exit_code, Some(1), "MRT itself is refused: {}", block.plain_output);
    }

    #[test]
    fn assist_investigates_the_failed_block_with_mcp_tools() {
        // The built-in plan only; never call a provider from tests.
        mrt_ai::set_ai_preferences(mrt_ai::AiPreferences { provider: Some("offline".into()), ..Default::default() });
        let mut app = MrtGuiApp::new();
        let ok = CommandBlock::new_completed("cargo test", "bash", "/src", "test a ... ok
", 0, 10);
        let failed = CommandBlock::new_completed("cargo test", "bash", "/src", "test a ... FAILED
error: test failed
", 101, 10);
        let failed_id = failed.id;
        app.tabs[0].blocks.push(ok);
        app.tabs[0].blocks.push(failed);
        app.submit_input(Some(":assist".into()), false);
        assert_eq!(app.tabs[0].blocks.last().unwrap().exit_code, None, "running while it investigates");
        pump_one(&mut app);
        let block = app.tabs[0].blocks.last().unwrap();
        assert_eq!(block.shell_name, "mrt-ai");
        assert!(block.plain_output.contains("→ mrt_compare_blocks"), "{}", block.plain_output);
        assert!(block.plain_output.contains(&failed_id.to_string()), "uses the ids it found");
        let answer = block.ai_explanation.as_deref().unwrap_or("");
        assert!(answer.contains("`cargo test` failed (exit code 101)"), "{answer}");
        assert!(answer.contains("earlier run"), "{answer}");
    }

    #[test]
    fn sandbox_tabs_pin_their_folder_and_ask_before_anything_not_safe() {
        let mut app = MrtGuiApp::new();
        app.config_state.config.safety.enabled = false; // the sandbox asks anyway
        let root = std::env::temp_dir().join(format!("mrt-sandbox-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        app.tabs[0].cwd = root.clone();
        app.submit_input(Some(":sandbox sub".into()), false);
        assert_eq!(app.tabs.len(), 2);
        let sandbox = app.tabs[1].sandbox_root.clone().expect("sandbox tab");
        assert!(sandbox.ends_with("sub"));
        assert_eq!(app.tabs[1].title, "sandbox sub");

        app.submit_input(Some("del notes.txt".into()), false);
        let pending = app.pending_safety.take().expect("Moderate commands wait for approval in a sandbox");
        assert_eq!(pending.cwd, sandbox);

        app.submit_input(Some(":sandbox does-not-exist".into()), false);
        assert_eq!(app.tabs.len(), 2, "no tab for a missing folder");
        app.sync_mcp_from_gui();
        let mirrored = app.mcp_host.local_server_state.tabs.lock().unwrap()[1].sandbox_root.clone();
        assert_eq!(mirrored, Some(sandbox.clone()));
        let layout = app.capture_layout();
        assert_eq!(layout.tabs[1].sandbox.as_ref(), Some(&sandbox));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn named_actions_broadcast_and_copy_mode() {
        let mut app = MrtGuiApp::new();
        app.config_state.config.actions = vec![mrt_core::NamedActionEntry {
            name: "Pair-Up".into(),
            title: Some("Split and broadcast".into()),
            command: None,
            commands: vec![":split-h".into(), ":broadcast on".into()],
            key: Some("ctrl+alt+p".into()),
        }];
        app.apply_config_side_effects();
        let palette = app.named_palette_actions();
        assert_eq!(palette.len(), 1);
        assert_eq!(palette[0].0, Action::RunNamed("pair_up".into()));
        assert_eq!(palette[0].1, "Split and broadcast");
        assert!(palette[0].2.to_lowercase().contains("alt"), "the action's key is bound: {}", palette[0].2);

        app.dispatch_action(Action::RunNamed("pair_up".into()));
        assert!(app.tabs[0].secondary_pane.is_some(), "MRT commands in an action run in order");
        assert!(app.tabs[0].broadcast);
        app.submit_input(Some(":broadcast".into()), false);
        assert!(!app.tabs[0].broadcast, "toggles");

        app.dispatch_action(Action::RunNamed("missing".into()));
        assert!(app.runbooks_state.replay_notice.as_deref().unwrap_or("").contains("missing"));

        app.dispatch_action(Action::CopyMode);
        assert!(app.copy_mode.is_none(), "no blocks, nothing to select");
        app.tabs[0].blocks.push(CommandBlock::new_completed("echo hi", "sh", "/", "hi
", 0, 0));
        app.dispatch_action(Action::CopyMode);
        assert_eq!(app.copy_mode, Some(crate::copy_mode::CopyMode { block: 0, line: 0, anchor: None }));
    }

    #[test]
    fn forwarding_channel_keeps_order_and_ends_with_its_senders() {
        let (tx, rx) = forwarding_channel::<u32>();
        for i in 0..100 {
            tx.send(i).unwrap();
        }
        let got: Vec<u32> = (0..100).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(got, (0..100).collect::<Vec<_>>());
        drop(tx);
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_err(), "closed once the sender is gone");
    }

    #[test]
    fn remote_tab_never_runs_commands_locally() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(":ssh mrt-test-host.invalid".to_string()), false);
        let tab = app.tabs.last().unwrap();
        assert_eq!(tab.remote.as_deref(), Some(&["mrt-test-host.invalid".to_string()][..]));
        assert!(tab.title.contains("mrt-test-host.invalid"));
        let tab_id = tab.id;
        let start = Instant::now();
        let block = loop {
            let tab = app.tabs.iter().find(|t| t.id == tab_id).unwrap();
            if let Some(block) = tab.blocks.iter().find(|b| b.exit_code.is_some()) {
                break block.clone();
            }
            assert!(start.elapsed() < Duration::from_secs(60), "the remote command did not finish");
            pump_one(&mut app);
        };
        assert_eq!(block.command, "hostname");
        assert_eq!(block.shell_name, "ssh");
        assert_ne!(block.exit_code, Some(0));
        let local = std::process::Command::new("hostname").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        assert!(
            local.is_empty() || !block.plain_output.trim().eq_ignore_ascii_case(&local),
            "the command ran locally: {:?}",
            block.plain_output
        );
        assert!(block.plain_output.contains("MRT could not start shell integration over SSH"), "{}", block.plain_output);
    }

    #[test]
    fn closing_the_split_ends_its_pane_session() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(":split".to_string()), false);
        app.tabs[0].active_pane_idx = 1;
        let long = if cfg!(windows) { ":tui ping -n 30 127.0.0.1" } else { ":tui sleep 30" };
        app.submit_input(Some(long.to_string()), false);
        assert_eq!(app.pane_tuis.len(), 1);
        // The other pane is focused, so the session does not get the keyboard.
        app.tabs[0].active_pane_idx = 0;
        assert_eq!(app.focused_tui(), None);

        app.submit_input(Some(":unsplit".to_string()), false);
        let ctx = egui::Context::default();
        app.process_tui_session(&ctx);
        assert!(app.pane_tuis.is_empty(), "removing the split must end its session");
    }

    /// Waits for the next worker event and feeds it back through the GUI event loop.
    fn pump_one(app: &mut MrtGuiApp) {
        let event = app
            .rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("worker event");
        app.tx.send(event).unwrap();
        app.process_async_events();
    }

    fn spawn_sleeper() -> std::process::Child {
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("60");
            c
        };
        cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        cmd.spawn().expect("spawn sleeper")
    }

    #[test]
    fn command_args_matches_whole_words_only() {
        assert_eq!(command_args(":ps node", ":ps"), Some(" node"));
        assert_eq!(command_args(":ps", ":ps"), Some(""));
        assert_eq!(command_args(":psql", ":ps"), None);
    }

    #[test]
    fn ps_command_renders_a_process_table_block() {
        let mut app = MrtGuiApp::new();
        let me = std::process::id();
        app.submit_input(Some(format!(":ps {me}")), false);
        pump_one(&mut app);
        let block = app.tabs[0].blocks.last().unwrap();
        assert_eq!(block.shell_name, PROCESS_BLOCK_SHELL);
        assert_eq!(block.exit_code, Some(0));
        match &block.structured {
            StructuredOutput::Table { headers, rows } => {
                assert_eq!(headers[0], "PID");
                assert!(rows.iter().any(|r| r[0] == me.to_string()));
            }
            other => panic!("expected table, got {other:?}"),
        }
        assert_eq!(app.busy_count, 0);
    }

    #[test]
    fn proc_command_renders_details() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(format!(":proc {}", std::process::id())), false);
        pump_one(&mut app);
        let block = app.tabs[0].blocks.last().unwrap();
        assert_eq!(block.exit_code, Some(0), "{}", block.plain_output);
        assert!(block.plain_output.contains("Listening ports"));
    }

    #[test]
    fn killing_mrt_itself_is_critical_and_waits_for_approval() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(format!(":kill {}", std::process::id())), false);
        pump_one(&mut app);
        let pending = app.pending_safety.as_ref().expect("Safety Gate opened");
        assert_eq!(pending.assessment.risk, RiskLevel::Critical);
        assert!(pending.kill.is_some());
        // Cancelling leaves MRT running and adds no block.
        app.pending_safety = None;
        assert!(app.tabs[0].blocks.is_empty());
    }

    #[test]
    fn approved_kill_stops_a_child_process() {
        let mut app = MrtGuiApp::new();
        let mut child = spawn_sleeper();
        let pid = child.id();
        app.submit_input(Some(format!(":kill {pid} --force")), false);
        pump_one(&mut app);
        let pending = app.pending_safety.as_ref().expect("Safety Gate opened");
        assert_eq!(pending.assessment.risk, RiskLevel::Moderate, "{}", pending.assessment.reason);
        assert!(pending.assessment.requires_approval);
        app.approve_pending_command();
        pump_one(&mut app);
        let block = app.tabs[0].blocks.last().unwrap();
        assert_eq!(block.exit_code, Some(0), "{}", block.plain_output);
        assert!(block.plain_output.contains("stopped"));
        let status = child.wait().expect("child reaped");
        assert!(!status.success());
    }

    #[test]
    fn gui_kill_request_from_mcp_opens_the_safety_gate() {
        let mut app = MrtGuiApp::new();
        let mut child = spawn_sleeper();
        let gui_tx = app.mcp_host.local_server_state.gui_tx.clone().unwrap();
        gui_tx
            .send(GuiUiCommand::RequestProcessKill { pids: vec![child.id()], force: true, tree: false })
            .unwrap();
        app.process_mcp_ui_commands();
        pump_one(&mut app);
        assert!(app.pending_safety.as_ref().is_some_and(|p| p.kill.is_some()));
        app.pending_safety = None;
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn stop_button_kills_a_running_command_tree() {
        let mut app = MrtGuiApp::new();
        let cmd = if cfg!(target_os = "windows") { "ping -n 60 127.0.0.1" } else { "sleep 60" };
        let block_id = app.spawn_shell_command(
            app.tabs[0].id,
            app.shells[0].clone(),
            app.tabs[0].cwd.clone(),
            cmd.to_string(),
        );
        let start = Instant::now();
        while app.running_pids.lock().unwrap().get(&block_id).is_none() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = *app.running_pids.lock().unwrap().get(&block_id).expect("shell pid recorded");
        assert!(app.owned_processes().contains_key(&pid));
        app.handle_block_action(BlockAction::StopBlock(block_id));
        let start = Instant::now();
        loop {
            pump_one(&mut app);
            let done = app.tabs[0]
                .blocks
                .iter()
                .find(|b| b.id == block_id)
                .is_some_and(|b| b.exit_code.is_some());
            if done || start.elapsed() > Duration::from_secs(20) {
                break;
            }
        }
        let block = app.tabs[0].blocks.iter().find(|b| b.id == block_id).unwrap();
        assert!(block.exit_code.is_some_and(|c| c != 0), "stopped command should report failure");
        assert!(app.running_pids.lock().unwrap().is_empty());
    }

    /// Commands for the persistent-shell tests in the syntax of `kind`:
    /// (set a variable, print it, print the working directory, wait a minute).
    fn session_test_commands(kind: &ShellKind) -> (&'static str, &'static str, &'static str, &'static str) {
        match kind {
            ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => (
                "$env:MRT_SESSION_TEST = 'kept-between-commands'",
                "Write-Output \"value=$env:MRT_SESSION_TEST\"",
                "(Get-Location).Path",
                "ping -n 60 127.0.0.1",
            ),
            ShellKind::CmdBatch => (
                "set MRT_SESSION_TEST=kept-between-commands",
                "echo value=%MRT_SESSION_TEST%",
                "cd",
                "ping -n 60 127.0.0.1",
            ),
            ShellKind::Fish => (
                "set -gx MRT_SESSION_TEST kept-between-commands",
                "echo value=$MRT_SESSION_TEST",
                "pwd",
                "sleep 60",
            ),
            _ => (
                "export MRT_SESSION_TEST=kept-between-commands",
                "echo value=$MRT_SESSION_TEST",
                "pwd",
                "sleep 60",
            ),
        }
    }

    /// Runs `cmd` in tab 0 with its first shell and waits for the block to finish.
    fn run_to_completion(app: &mut MrtGuiApp, cmd: &str) -> CommandBlock {
        let shell = app.shells[0].clone();
        run_in_shell(app, &shell, cmd)
    }

    /// Runs `cmd` in tab 0 with `shell` and waits for the block to finish.
    fn run_in_shell(app: &mut MrtGuiApp, shell: &ShellProfile, cmd: &str) -> CommandBlock {
        let block_id = app.spawn_shell_command(app.tabs[0].id, shell.clone(), app.tabs[0].cwd.clone(), cmd.to_string());
        let start = Instant::now();
        loop {
            if let Some(block) = app.tabs[0].blocks.iter().find(|b| b.id == block_id && b.exit_code.is_some()) {
                return block.clone();
            }
            assert!(start.elapsed() < Duration::from_secs(60), "`{cmd}` did not finish");
            pump_one(app);
        }
    }

    /// Every installed shell with session support keeps state between commands, starting
    /// from its very first command (the session starts while that command waits).
    #[test]
    fn persistent_shell_keeps_variables_and_directory_between_commands() {
        let mut app = MrtGuiApp::new();
        app.shell_pool.set_enabled(true);
        let shells: Vec<ShellProfile> = app.shells.iter().filter(|s| ShellSession::supports(&s.kind)).cloned().collect();
        for shell in shells {
            let (set_var, print_var, print_cwd, _) = session_test_commands(&shell.kind);

            run_in_shell(&mut app, &shell, set_var);
            let block = run_in_shell(&mut app, &shell, print_var);
            assert!(
                block.plain_output.contains("value=kept-between-commands"),
                "{:?}: variable lost between commands (session unavailable: {:?}); output: {:?}",
                shell.kind,
                app.shell_pool.unavailable_reason(app.tabs[0].id, &shell.id),
                block.plain_output
            );

            let root = std::env::temp_dir().join(format!("mrt-session-test-{}", Uuid::new_v4()));
            let target = root.join("mrt session cd");
            std::fs::create_dir_all(&target).unwrap();
            let cd = crate::shell_pool::cd_command(&shell.kind, &target).unwrap();
            let block = run_in_shell(&mut app, &shell, &cd);
            assert_eq!(block.exit_code, Some(0), "{:?}: cd failed: {:?}", shell.kind, block.plain_output);
            assert!(
                app.tabs[0].cwd.to_string_lossy().contains("mrt session cd"),
                "{:?}: tab cwd not updated from the shell: {:?}",
                shell.kind,
                app.tabs[0].cwd
            );
            let block = run_in_shell(&mut app, &shell, print_cwd);
            assert!(block.plain_output.contains("mrt session cd"), "{:?}: pwd output: {:?}", shell.kind, block.plain_output);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn stop_interrupts_a_persistent_shell_command_and_keeps_the_shell() {
        let mut app = MrtGuiApp::new();
        app.shell_pool.set_enabled(true);
        let shell = app.shells[0].clone();
        if !ShellSession::supports(&shell.kind) {
            return;
        }
        let (set_var, print_var, _, long) = session_test_commands(&shell.kind);
        run_to_completion(&mut app, set_var);

        let block_id = app.spawn_shell_command(app.tabs[0].id, shell.clone(), app.tabs[0].cwd.clone(), long.to_string());
        let start = Instant::now();
        while app.running_pids.lock().unwrap().get(&block_id).is_none() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        // Give the command a moment to start inside the shell before interrupting it.
        std::thread::sleep(Duration::from_millis(1500));
        app.handle_block_action(BlockAction::StopBlock(block_id));
        let start = Instant::now();
        while !app.tabs[0].blocks.iter().any(|b| b.id == block_id && b.exit_code.is_some()) {
            assert!(start.elapsed() < Duration::from_secs(30), "interrupted command did not finish");
            pump_one(&mut app);
        }
        assert!(start.elapsed() < Duration::from_secs(30));

        // The shell survived the Stop, so the variable is still set.
        let block = run_to_completion(&mut app, print_var);
        assert!(
            block.plain_output.contains("value=kept-between-commands"),
            "shell state lost after Stop: {:?}",
            block.plain_output
        );
    }

    #[test]
    fn watches_and_toolbox_commands() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(":toolbox task-manager".into()), false);
        assert!(app.side_state.show_toolbox_panel);
        assert_eq!(app.side_state.toolbox_tab, ToolboxTab::TaskManager);
        assert!(app.force_process_tick);

        app.submit_input(Some(":watch-proc node --mem 2GB".into()), false);
        assert_eq!(app.process_watches.len(), 1);
        app.submit_input(Some(":watch-proc list".into()), false);
        assert!(app.tabs[0].blocks.last().unwrap().plain_output.contains("mem >"));
        app.submit_input(Some(":watch-proc --cpu".into()), false);
        assert_eq!(app.tabs[0].blocks.last().unwrap().exit_code, Some(1));
        app.submit_input(Some(":unwatch-proc 1".into()), false);
        assert!(app.process_watches.is_empty());

        // A snapshot tick feeds the Task Manager without touching busy_count.
        app.maybe_spawn_process_tick();
        assert!(app.process_tick_in_flight);
        pump_one(&mut app);
        assert!(!app.process_tick_in_flight);
        assert!(app.side_state.task_manager.snapshot.is_some());
        assert_eq!(app.busy_count, 0);
    }

    #[test]
    fn live_top_updates_in_place_and_stops() {
        let mut app = MrtGuiApp::new();
        app.submit_input(Some(":top --limit 5".into()), false);
        let block_id = app.live_top.as_ref().unwrap().block_id;
        app.maybe_spawn_process_tick();
        pump_one(&mut app);
        let block = app.tabs[0].blocks.iter().find(|b| b.id == block_id).unwrap();
        assert!(block.exit_code.is_none());
        match &block.structured {
            StructuredOutput::Table { rows, .. } => assert!(!rows.is_empty() && rows.len() <= 5),
            other => panic!("expected table, got {other:?}"),
        }
        app.handle_block_action(BlockAction::StopBlock(block_id));
        assert!(app.live_top.is_none());
        assert_eq!(app.tabs[0].blocks.iter().find(|b| b.id == block_id).unwrap().exit_code, Some(0));
    }
}