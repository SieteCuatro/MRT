//! Task Manager tab of the Toolbox panel: live process list, tree view, sparklines,
//! details of the selected process, and gated Stop / Force stop actions.

use crate::theme::{chip, ghost_button, secondary_button, Palette};
use egui::{Color32, RichText, Ui};
use mrt_tools::{
    format_bytes, query_processes, snapshot_summary, OwnedProcessMap, PortEntry, ProcessQuery,
    ProcessRow, ProcessSnapshot, ProcessSort,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Samples kept per process for the CPU / memory sparklines.
const HISTORY_LEN: usize = 30;
/// Rows drawn in the list; filtering narrows the rest.
const MAX_VISIBLE_ROWS: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub enum TaskManagerEvent {
    Refresh,
    /// Stop a process through the Safety Approval Gate.
    Stop { pid: u32, force: bool, tree: bool },
    /// Run `:proc <pid>` in the active tab.
    Inspect(u32),
    /// Run `:watch-proc <pid>` in the active tab.
    Watch(u32),
    /// Run a process-control command (`:suspend`, `:resume`, `:priority`) in the active tab.
    Control(String),
    /// Run `:ps` with the current filter in the active tab (optionally diffing it with the
    /// previous `:ps` block).
    SnapshotToTab { command: String, compare: bool },
    ScanPorts,
}

pub struct TaskManagerState {
    pub filter: String,
    pub sort: ProcessSort,
    pub tree: bool,
    pub only_mrt: bool,
    /// Refresh every couple of seconds while the tab is visible.
    pub live: bool,
    pub selected_pid: Option<u32>,
    pub snapshot: Option<Arc<ProcessSnapshot>>,
    pub owned: OwnedProcessMap,
    pub error: Option<String>,
    pub refreshing: bool,
    rows: Vec<ProcessRow>,
    rows_dirty: bool,
    last_query: Option<ProcessQuery>,
    cpu_history: HashMap<u32, VecDeque<f32>>,
    mem_history: HashMap<u32, VecDeque<u64>>,
}

impl Default for TaskManagerState {
    fn default() -> Self {
        Self {
            filter: String::new(),
            sort: ProcessSort::Cpu,
            tree: false,
            only_mrt: false,
            live: true,
            selected_pid: None,
            snapshot: None,
            owned: OwnedProcessMap::new(),
            error: None,
            refreshing: false,
            rows: Vec::new(),
            rows_dirty: true,
            last_query: None,
            cpu_history: HashMap::new(),
            mem_history: HashMap::new(),
        }
    }
}

impl TaskManagerState {
    pub fn query(&self) -> ProcessQuery {
        ProcessQuery {
            filter: Some(self.filter.trim().to_string()).filter(|f| !f.is_empty()),
            sort: self.sort,
            limit: None,
            tree: self.tree,
            only_mrt: self.only_mrt,
        }
    }

    /// The equivalent prompt command for the current view.
    pub fn ps_command(&self) -> String {
        let mut cmd = String::from(":ps");
        if !self.filter.trim().is_empty() {
            cmd.push(' ');
            cmd.push_str(self.filter.trim());
        }
        if self.sort != ProcessSort::Cpu {
            cmd.push_str(&format!(" --sort {}", self.sort.label()));
        }
        if self.tree {
            cmd.push_str(" --tree");
        }
        if self.only_mrt {
            cmd.push_str(" --mrt");
        }
        cmd
    }

    pub fn apply_snapshot(&mut self, snapshot: Arc<ProcessSnapshot>, owned: OwnedProcessMap) {
        for e in &snapshot.entries {
            let cpu = self.cpu_history.entry(e.pid).or_default();
            cpu.push_back(e.cpu_percent);
            if cpu.len() > HISTORY_LEN {
                cpu.pop_front();
            }
            let mem = self.mem_history.entry(e.pid).or_default();
            mem.push_back(e.memory_bytes);
            if mem.len() > HISTORY_LEN {
                mem.pop_front();
            }
        }
        self.cpu_history.retain(|pid, _| snapshot.get(*pid).is_some());
        self.mem_history.retain(|pid, _| snapshot.get(*pid).is_some());
        self.snapshot = Some(snapshot);
        self.owned = owned;
        self.error = None;
        self.refreshing = false;
        self.rows_dirty = true;
    }

    /// Selects a process (e.g. from a PID link) and clears filters that would hide it.
    pub fn focus_pid(&mut self, pid: u32) {
        self.selected_pid = Some(pid);
        let hidden = self
            .snapshot
            .as_ref()
            .map(|s| !query_processes(s, &self.query(), &self.owned).iter().any(|r| r.entry.pid == pid))
            .unwrap_or(false);
        if hidden {
            self.filter.clear();
            self.only_mrt = false;
            self.rows_dirty = true;
        }
    }

    fn rows(&mut self) -> &[ProcessRow] {
        let query = self.query();
        if self.rows_dirty || self.last_query.as_ref() != Some(&query) {
            self.rows = self
                .snapshot
                .as_ref()
                .map(|s| query_processes(s, &query, &self.owned))
                .unwrap_or_default();
            self.last_query = Some(query);
            self.rows_dirty = false;
        }
        &self.rows
    }

    pub fn visible_row_count(&mut self) -> usize {
        self.rows().len()
    }
}

fn sparkline(ui: &mut Ui, values: impl Iterator<Item = f32>, max: f32, size: egui::Vec2, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Palette::BG_INSET());
    let values: Vec<f32> = values.collect();
    if values.len() < 2 || max <= 0.0 {
        return;
    }
    let step = rect.width() / (HISTORY_LEN.saturating_sub(1)).max(1) as f32;
    let start_x = rect.right() - step * (values.len() - 1) as f32;
    let points: Vec<egui::Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let t = (v / max).clamp(0.0, 1.0);
            egui::pos2(start_x + step * i as f32, rect.bottom() - 1.0 - t * (rect.height() - 2.0))
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.2_f32, color)));
}

fn cpu_color(cpu: f32) -> Color32 {
    if cpu >= 50.0 {
        Palette::ACCENT_RED()
    } else if cpu >= 15.0 {
        Palette::ACCENT_AMBER()
    } else {
        Palette::ACCENT_GREEN()
    }
}

/// Renders the Task Manager tab. `ports` are the rows of the last Network-tab port scan,
/// used to show what the selected process listens on.
pub fn render_task_manager_tab(
    ui: &mut Ui,
    state: &mut TaskManagerState,
    ports: &[PortEntry],
) -> Option<TaskManagerEvent> {
    let mut event = None;

    ui.label(
        RichText::new("Running processes, CPU and memory. Stopping a process goes through the Safety Gate.")
            .size(11.5)
            .color(Palette::TEXT_SECONDARY()),
    );
    ui.add_space(4.0);

    let shown = state.visible_row_count();
    ui.horizontal_wrapped(|ui| {
        match &state.snapshot {
            Some(snap) => {
                ui.label(RichText::new(snapshot_summary(snap, shown)).size(12.0).color(Palette::TEXT_BODY()));
            }
            None => {
                ui.label(RichText::new("Loading processes…").size(12.0).color(Palette::TEXT_MUTED()));
            }
        }
        if state.refreshing {
            ui.add(egui::Spinner::new().size(12.0));
        }
    });
    if let Some(err) = &state.error {
        ui.colored_label(Palette::ACCENT_RED(), err);
    }

    ui.add(
        egui::TextEdit::singleline(&mut state.filter)
            .hint_text("Filter by name, PID or command line")
            .desired_width(f32::INFINITY),
    );
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("task_manager_sort")
            .selected_text(format!("Sort: {}", state.sort.label()))
            .width(90.0)
            .show_ui(ui, |ui| {
                for sort in [ProcessSort::Cpu, ProcessSort::Memory, ProcessSort::Pid, ProcessSort::Name] {
                    ui.selectable_value(&mut state.sort, sort, sort.label());
                }
            });
        ui.checkbox(&mut state.tree, "Tree").on_hover_text("Show parent/child structure");
        ui.checkbox(&mut state.only_mrt, "MRT only")
            .on_hover_text("Only processes started by MRT tabs, TUI overlays and PTYs");
        ui.checkbox(&mut state.live, "Live").on_hover_text("Refresh every 2 seconds");
        if ui.add(ghost_button("⟳ Refresh")).clicked() {
            event = Some(TaskManagerEvent::Refresh);
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .add(ghost_button("Snapshot to tab"))
            .on_hover_text("Run this view as a :ps block in the active tab")
            .clicked()
        {
            event = Some(TaskManagerEvent::SnapshotToTab { command: state.ps_command(), compare: false });
        }
        if ui
            .add(ghost_button("Compare with last snapshot"))
            .on_hover_text("Run :ps and diff it with the previous :ps block (started, exited, memory growth)")
            .clicked()
        {
            event = Some(TaskManagerEvent::SnapshotToTab { command: state.ps_command(), compare: true });
        }
    });
    ui.separator();

    let selected = state.selected_pid;
    let snapshot = state.snapshot.clone();
    let details_height = if selected.is_some() { 230.0 } else { 0.0 };
    let list_height = (ui.available_height() - details_height).max(140.0);

    // Process list
    let mut clicked_pid = None;
    {
        let cpu_history = state.cpu_history.clone();
        let rows = state.rows();
        egui::ScrollArea::vertical()
            .id_salt("task_manager_rows")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("task_manager_grid")
                    .striped(true)
                    .num_columns(5)
                    .spacing([8.0, 3.0])
                    .show(ui, |ui| {
                        for h in ["Name", "PID", "CPU", "", "Memory"] {
                            ui.label(RichText::new(h).size(11.5).strong().color(Palette::TEXT_MUTED()));
                        }
                        ui.end_row();
                        for row in rows.iter().take(MAX_VISIBLE_ROWS) {
                            let e = &row.entry;
                            let indent = if row.depth > 0 {
                                format!("{}└ ", "  ".repeat(row.depth - 1))
                            } else {
                                String::new()
                            };
                            let mut name = RichText::new(format!("{indent}{}", e.name)).size(12.5);
                            name = if row.owner.is_some() {
                                name.color(Palette::ACCENT_TEXT())
                            } else {
                                name.color(Palette::TEXT_PRIMARY())
                            };
                            let resp = ui
                                .selectable_label(selected == Some(e.pid), name)
                                .on_hover_text(format!(
                                    "{}{}",
                                    e.command_line,
                                    row.owner.as_ref().map(|o| format!("\nMRT: {o}")).unwrap_or_default()
                                ));
                            if resp.clicked() {
                                clicked_pid = Some(e.pid);
                            }
                            ui.label(RichText::new(e.pid.to_string()).monospace().size(12.0).color(Palette::TEXT_SECONDARY()));
                            ui.label(
                                RichText::new(format!("{:.1}%", e.cpu_percent))
                                    .monospace()
                                    .size(12.0)
                                    .color(cpu_color(e.cpu_percent)),
                            );
                            let history = cpu_history.get(&e.pid);
                            let peak = history
                                .map(|h| h.iter().cloned().fold(0.0_f32, f32::max))
                                .unwrap_or(0.0)
                                .max(5.0);
                            sparkline(
                                ui,
                                history.into_iter().flatten().cloned(),
                                peak,
                                egui::vec2(46.0, 14.0),
                                cpu_color(e.cpu_percent),
                            );
                            ui.label(RichText::new(format_bytes(e.memory_bytes)).monospace().size(12.0).color(Palette::TEXT_BODY()));
                            ui.end_row();
                        }
                    });
                let total = rows.len();
                if total > MAX_VISIBLE_ROWS {
                    ui.label(
                        RichText::new(format!("{} more — narrow the filter to see them", total - MAX_VISIBLE_ROWS))
                            .size(11.5)
                            .color(Palette::TEXT_MUTED()),
                    );
                }
            });
    }
    if let Some(pid) = clicked_pid {
        state.selected_pid = if selected == Some(pid) { None } else { Some(pid) };
    }

    // Details of the selected process
    if let (Some(pid), Some(snap)) = (state.selected_pid, snapshot.as_ref()) {
        ui.separator();
        match snap.get(pid) {
            None => {
                ui.label(RichText::new(format!("PID {pid} is no longer running.")).color(Palette::TEXT_MUTED()));
                if ui.small_button("Clear selection").clicked() {
                    state.selected_pid = None;
                }
            }
            Some(e) => {
                let owner = mrt_tools::owner_label(snap, pid, &state.owned);
                egui::ScrollArea::vertical()
                    .id_salt("task_manager_details")
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&e.name).size(14.0).strong().color(Palette::TEXT_PRIMARY()));
                            ui.label(RichText::new(format!("PID {}", e.pid)).monospace().size(12.0).color(Palette::TEXT_SECONDARY()));
                            if let Some(owner) = &owner {
                                chip(ui, &format!("MRT · {owner}"), Color32::from_rgb(34, 38, 58), Palette::ACCENT_TEXT());
                            }
                            if mrt_core::is_protected_process_name(&e.name) || e.pid <= 4 {
                                chip(ui, "OS-critical", Color32::from_rgb(74, 31, 31), Color32::from_rgb(252, 165, 165));
                            }
                        });
                        ui.label(
                            RichText::new(&e.command_line)
                                .monospace()
                                .size(11.5)
                                .color(Palette::TEXT_BODY()),
                        );
                        ui.horizontal_wrapped(|ui| {
                            let parent = e.parent_pid.and_then(|pp| snap.get(pp));
                            ui.label(RichText::new("Parent:").size(11.5).color(Palette::TEXT_MUTED()));
                            match (e.parent_pid, parent) {
                                (Some(pp), Some(p)) => {
                                    if ui.link(format!("{} ({pp})", p.name)).clicked() {
                                        state.selected_pid = Some(pp);
                                    }
                                }
                                (Some(pp), None) => {
                                    ui.label(RichText::new(pp.to_string()).size(11.5).color(Palette::TEXT_SECONDARY()));
                                }
                                _ => {
                                    ui.label(RichText::new("none").size(11.5).color(Palette::TEXT_SECONDARY()));
                                }
                            }
                            ui.label(
                                RichText::new(format!(
                                    "· User: {} · {} · up {}",
                                    e.user.as_deref().unwrap_or("?"),
                                    e.status,
                                    mrt_tools::process_inspector::format_duration(e.run_time_secs)
                                ))
                                .size(11.5)
                                .color(Palette::TEXT_SECONDARY()),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("CPU {:.1}%", e.cpu_percent))
                                    .size(11.5)
                                    .color(cpu_color(e.cpu_percent)),
                            );
                            let cpu = state.cpu_history.get(&pid);
                            let peak = cpu.map(|h| h.iter().cloned().fold(0.0_f32, f32::max)).unwrap_or(0.0).max(5.0);
                            sparkline(ui, cpu.into_iter().flatten().cloned(), peak, egui::vec2(90.0, 18.0), cpu_color(e.cpu_percent));
                            ui.label(
                                RichText::new(format!("Mem {}", format_bytes(e.memory_bytes)))
                                    .size(11.5)
                                    .color(Palette::TEXT_BODY()),
                            );
                            let mem = state.mem_history.get(&pid);
                            let mem_peak = mem.map(|h| h.iter().cloned().max().unwrap_or(0)).unwrap_or(0) as f32;
                            sparkline(
                                ui,
                                mem.into_iter().flatten().map(|v| *v as f32),
                                mem_peak,
                                egui::vec2(90.0, 18.0),
                                Palette::ACCENT(),
                            );
                        });
                        let children = snap.children_of(pid).len();
                        let listening: Vec<&PortEntry> = ports.iter().filter(|p| p.pid.trim() == pid.to_string()).collect();
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("{children} child process(es) · listening:"))
                                    .size(11.5)
                                    .color(Palette::TEXT_MUTED()),
                            );
                            if listening.is_empty() {
                                ui.label(
                                    RichText::new(if ports.is_empty() { "scan ports to check" } else { "none" })
                                        .size(11.5)
                                        .color(Palette::TEXT_SECONDARY()),
                                );
                            }
                            for p in &listening {
                                chip(
                                    ui,
                                    &format!("{} :{}", p.protocol, p.port),
                                    Palette::BG_OVERLAY(),
                                    Palette::TEXT_BODY(),
                                );
                            }
                            if ui.small_button("Scan ports").clicked() {
                                event = Some(TaskManagerEvent::ScanPorts);
                            }
                            if ui.small_button("Services").on_hover_text(":services — system services; :service start|stop|restart <name>").clicked() {
                                event = Some(TaskManagerEvent::Control(":services".into()));
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.add(secondary_button("Stop")).on_hover_text("Graceful stop (SIGTERM / taskkill)").clicked() {
                                event = Some(TaskManagerEvent::Stop { pid, force: false, tree: false });
                            }
                            if ui
                                .add(secondary_button("Force stop"))
                                .on_hover_text("SIGKILL / TerminateProcess")
                                .clicked()
                            {
                                event = Some(TaskManagerEvent::Stop { pid, force: true, tree: false });
                            }
                            if children > 0
                                && ui
                                    .add(secondary_button("Stop tree"))
                                    .on_hover_text("Force stop this process and all its children")
                                    .clicked()
                            {
                                event = Some(TaskManagerEvent::Stop { pid, force: true, tree: true });
                            }
                            if ui.add(ghost_button("Suspend")).on_hover_text(":suspend — pause it (resume later)").clicked() {
                                event = Some(TaskManagerEvent::Control(format!(":suspend {pid}")));
                            }
                            if ui.add(ghost_button("Resume")).on_hover_text(":resume — continue a suspended process").clicked() {
                                event = Some(TaskManagerEvent::Control(format!(":resume {pid}")));
                            }
                            ui.menu_button("Priority", |ui| {
                                for (label, level) in [("Idle", "idle"), ("Below normal", "low"), ("Normal", "normal"), ("Above normal", "above"), ("High", "high")] {
                                    if ui.button(label).clicked() {
                                        event = Some(TaskManagerEvent::Control(format!(":priority {level} {pid}")));
                                        ui.close_menu();
                                    }
                                }
                            });
                            if ui.add(ghost_button("Details")).on_hover_text(":proc in the active tab").clicked() {
                                event = Some(TaskManagerEvent::Inspect(pid));
                            }
                            if ui.add(ghost_button("Watch")).on_hover_text(":watch-proc — notify when it exits").clicked() {
                                event = Some(TaskManagerEvent::Watch(pid));
                            }
                            if ui.add(ghost_button("Copy PID")).clicked() {
                                ui.output_mut(|o| o.copied_text = pid.to_string());
                            }
                        });
                    });
            }
        }
    }

    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use mrt_tools::ProcessEntry;

    fn snap(pids: &[(u32, &str, f32)]) -> Arc<ProcessSnapshot> {
        Arc::new(ProcessSnapshot {
            taken_at_ms: 0,
            current_pid: 1,
            current_user: None,
            logical_cpus: 1,
            total_memory_bytes: 0,
            entries: pids
                .iter()
                .map(|(pid, name, cpu)| ProcessEntry {
                    pid: *pid,
                    parent_pid: None,
                    name: name.to_string(),
                    exe: None,
                    command_line: name.to_string(),
                    user: None,
                    cpu_percent: *cpu,
                    memory_bytes: 1024,
                    start_time: 0,
                    run_time_secs: 0,
                    status: "Run".into(),
                })
                .collect(),
        })
    }

    #[test]
    fn snapshots_feed_histories_and_filtered_rows() {
        let mut state = TaskManagerState::default();
        state.apply_snapshot(snap(&[(1, "mrt", 1.0), (2, "node", 30.0), (3, "vite", 2.0)]), OwnedProcessMap::new());
        state.apply_snapshot(snap(&[(1, "mrt", 2.0), (2, "node", 40.0)]), OwnedProcessMap::new());
        assert_eq!(state.cpu_history[&2].len(), 2);
        assert!(!state.cpu_history.contains_key(&3), "exited processes drop their history");
        assert_eq!(state.visible_row_count(), 2);
        state.filter = "node".into();
        assert_eq!(state.visible_row_count(), 1);
        assert_eq!(state.ps_command(), ":ps node");

        state.only_mrt = true;
        state.focus_pid(2);
        assert_eq!(state.selected_pid, Some(2));
        assert!(state.filter.is_empty() && !state.only_mrt, "filters hiding the focused PID are cleared");
    }

    #[test]
    fn renders_without_panicking() {
        let mut state = TaskManagerState::default();
        state.apply_snapshot(snap(&[(1, "mrt", 1.0), (2, "node", 30.0)]), OwnedProcessMap::new());
        state.selected_pid = Some(2);
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = render_task_manager_tab(ui, &mut state, &[]);
            });
        });
    }
}
