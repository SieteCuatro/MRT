//! Desktop notifications and unseen-command badges for commands that finish in the background.
//!
//! [`CommandNotifier`] looks at every tab's blocks once per frame. When a block finishes it
//! decides, with [`should_notify`], whether to send a desktop notification (the command ran
//! longer than the threshold and MRT is unfocused or the block's tab is hidden) and counts it
//! as unseen when its tab is hidden, so the sidebar can show a badge until the tab is viewed.
//! Watching block state instead of hooking each completion path also covers runbook steps and
//! the TUI overlay, whose block is added already finished.

use crate::app::TerminalTab;
use crate::theme::Palette;
use egui::{Align2, Color32, FontId, RichText};
use mrt_core::CommandBlock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use uuid::Uuid;

/// Longest command text shown in a notification.
const MAX_COMMAND_CHARS: usize = 80;

/// User preferences for command-finished notifications. Edited in the Notifications window;
/// the `[notifications]` section of the config file sets it on load and reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    /// Send desktop notifications at all (the sidebar badge is always on).
    pub enabled: bool,
    /// Minimum command duration, in seconds, before a notification is sent.
    pub threshold_secs: u64,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_secs: 10,
        }
    }
}

/// Decides whether a finished command deserves a desktop notification.
pub fn should_notify(settings: &NotificationSettings, duration_ms: u64, window_focused: bool, block_visible: bool) -> bool {
    settings.enabled && duration_ms >= settings.threshold_secs.saturating_mul(1000) && (!window_focused || !block_visible)
}

/// What the user can currently see.
#[derive(Debug, Clone, Copy)]
pub struct ViewState {
    /// The MRT window has keyboard focus and is not minimized.
    pub window_focused: bool,
    pub active_tab: usize,
    /// Mosaic mode shows every tab at once.
    pub mosaic: bool,
}

impl ViewState {
    fn tab_visible(&self, idx: usize) -> bool {
        self.mosaic || idx == self.active_tab
    }
}

/// A finished command to announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub tab_id: Uuid,
    pub tab_name: String,
    pub command: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

impl Notice {
    pub fn summary(&self) -> String {
        if self.exit_code == 0 {
            "✓ Command finished".to_string()
        } else {
            format!("✗ Command failed (exit {})", self.exit_code)
        }
    }

    pub fn body(&self) -> String {
        format!(
            "{}\n{} · {}",
            truncate(self.command.trim(), MAX_COMMAND_CHARS),
            format_duration(self.duration_ms),
            self.tab_name
        )
    }
}

fn truncate(text: &str, max: usize) -> String {
    let first_line = text.lines().next().unwrap_or("");
    let multi_line = text.lines().nth(1).is_some();
    if first_line.chars().count() <= max && !multi_line {
        first_line.to_string()
    } else {
        let mut out: String = first_line.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// `850ms`, `12.3s`, `2m 05s`, `1h 02m`.
pub fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if ms < 1000 {
        format!("{ms}ms")
    } else if secs < 60 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// The name shown for a tab: its title, plus the folder for generic `Tab N` titles.
fn tab_name(tab: &TerminalTab) -> String {
    if tab.title.starts_with("Tab ") || tab.title.starts_with("Terminal ") {
        let folder = tab
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| tab.cwd.display().to_string());
        format!("{} · {folder}", tab.title)
    } else {
        tab.title.clone()
    }
}

fn all_blocks(tab: &TerminalTab) -> impl Iterator<Item = &CommandBlock> {
    tab.blocks
        .iter()
        .chain(tab.secondary_pane.iter().flat_map(|pane| pane.blocks.iter()))
}

/// Tracks block completion across frames and delivers notifications.
pub struct CommandNotifier {
    pub settings: NotificationSettings,
    /// Finished blocks already handled.
    finished: HashSet<Uuid>,
    /// Finished commands per tab that the user has not seen yet.
    unseen: HashMap<Uuid, usize>,
    initialized: bool,
    total_blocks: usize,
    /// Desktop delivery is off in unit tests.
    desktop: bool,
    click_tx: Sender<Uuid>,
    click_rx: Receiver<Uuid>,
    pub show_settings: bool,
}

impl CommandNotifier {
    pub fn new() -> Self {
        Self::with_settings(NotificationSettings::default(), !cfg!(test))
    }

    fn with_settings(settings: NotificationSettings, desktop: bool) -> Self {
        let (click_tx, click_rx) = channel();
        Self {
            settings,
            finished: HashSet::new(),
            unseen: HashMap::new(),
            initialized: false,
            total_blocks: 0,
            desktop,
            click_tx,
            click_rx,
            show_settings: false,
        }
    }

    /// Unseen finished commands in a tab.
    pub fn unseen(&self, tab_id: Uuid) -> usize {
        self.unseen.get(&tab_id).copied().unwrap_or(0)
    }

    /// [`observe_with`](Self::observe_with) for a single window.
    #[cfg(test)]
    pub fn observe(&mut self, tabs: &[TerminalTab], view: ViewState) -> Vec<Notice> {
        self.observe_with(tabs, view, &[])
    }

    /// Records blocks that finished since the last call, updates badges, and returns
    /// the notifications to send. Blocks present on the first call are never announced.
    /// `also_visible` are tabs on screen in MRT's other windows.
    pub fn observe_with(&mut self, tabs: &[TerminalTab], view: ViewState, also_visible: &[Uuid]) -> Vec<Notice> {
        let mut notices = Vec::new();
        let mut total = 0;
        for (idx, tab) in tabs.iter().enumerate() {
            let visible = view.tab_visible(idx) || also_visible.contains(&tab.id);
            for block in all_blocks(tab) {
                total += 1;
                let Some(exit_code) = block.exit_code else {
                    // Running (or restarted, like a live `:top`): announce it again when it ends.
                    self.finished.remove(&block.id);
                    continue;
                };
                if !self.finished.insert(block.id) || !self.initialized {
                    continue;
                }
                if !visible {
                    *self.unseen.entry(tab.id).or_default() += 1;
                }
                if should_notify(&self.settings, block.duration_ms, view.window_focused, visible) {
                    notices.push(Notice {
                        tab_id: tab.id,
                        tab_name: tab_name(tab),
                        command: block.command.clone(),
                        exit_code,
                        duration_ms: block.duration_ms,
                    });
                }
            }
            if visible && view.window_focused {
                self.unseen.remove(&tab.id);
            }
        }
        self.unseen.retain(|id, _| tabs.iter().any(|t| t.id == *id));
        if total < self.total_blocks {
            // Blocks were cleared or a tab closed: forget ids that no longer exist.
            let present: HashSet<Uuid> = tabs.iter().flat_map(all_blocks).map(|b| b.id).collect();
            self.finished.retain(|id| present.contains(id));
        }
        self.total_blocks = total;
        self.initialized = true;
        notices
    }

    /// Per-frame hook: sends notifications for newly finished commands and returns the
    /// index of a tab to switch to when the user clicked one.
    /// `also_visible`: tabs shown in MRT's other windows.
    pub fn update(&mut self, ctx: &egui::Context, tabs: &[TerminalTab], active_tab: usize, mosaic: bool, also_visible: &[Uuid]) -> Option<usize> {
        // Focused: any MRT window has the keyboard and is not minimized.
        let window_focused = ctx.input(|i| {
            let viewport = i.viewport();
            let main = viewport.focused.unwrap_or(true) && !viewport.minimized.unwrap_or(false);
            main || i.raw.viewports.values().any(|v| v.focused == Some(true) && !v.minimized.unwrap_or(false))
        });
        let view = ViewState {
            window_focused,
            active_tab,
            mosaic,
        };
        for notice in self.observe_with(tabs, view, also_visible) {
            self.deliver(ctx, notice);
        }
        let clicked = self.click_rx.try_iter().last()?;
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        tabs.iter().position(|t| t.id == clicked)
    }

    fn deliver(&self, ctx: &egui::Context, notice: Notice) {
        if !self.desktop {
            return;
        }
        let clicks = self.click_tx.clone();
        let ctx = ctx.clone();
        // Showing a notification can block (D-Bus round trips, waiting for a click).
        std::thread::spawn(move || send_desktop_notification(&notice, clicks, ctx));
    }

    /// The Notifications settings window.
    pub fn render_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut test = false;
        egui::Window::new("Notifications")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.checkbox(&mut self.settings.enabled, "Notify when a long command finishes")
                    .on_hover_text("Only while MRT is unfocused or the command's tab is hidden");
                ui.add_enabled_ui(self.settings.enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Longer than");
                        ui.add(egui::DragValue::new(&mut self.settings.threshold_secs).range(1..=86_400).suffix(" s"));
                    });
                });
                ui.label(
                    RichText::new("Tabs with unseen finished commands show a badge in the sidebar.")
                        .size(12.0)
                        .color(Palette::TEXT_MUTED()),
                );
                if ui.button("Send test notification").clicked() {
                    test = true;
                }
            });
        self.show_settings = open;
        if test {
            self.deliver(
                ctx,
                Notice {
                    tab_id: Uuid::nil(),
                    tab_name: "MRT".into(),
                    command: "echo notifications work".into(),
                    exit_code: 0,
                    duration_ms: self.settings.threshold_secs * 1000,
                },
            );
        }
    }

    /// Paints the unseen-commands badge on a sidebar tab row that started at `top`
    /// (from [`row_top`]) and has just been laid out.
    pub fn paint_badge(&self, ui: &egui::Ui, top: egui::Rect, tab_id: Uuid, compact: bool) {
        let count = self.unseen(tab_id);
        if count == 0 {
            return;
        }
        let bottom = ui.cursor().min.y - ui.spacing().item_spacing.y;
        let row = egui::Rect::from_min_max(top.min, egui::pos2(top.max.x, bottom.max(top.min.y)));
        let text = if count > 99 { "99+".to_string() } else { count.to_string() };
        let center = if compact {
            egui::pos2(row.left() + 10.0, row.top() + 10.0)
        } else {
            egui::pos2(row.right() - 46.0, row.center().y)
        };
        let radius = if compact { 7.0 } else { 9.0 };
        let width = (text.chars().count() as f32 * if compact { 5.0 } else { 6.5 }).max(radius * 2.0);
        let rect = egui::Rect::from_center_size(center, egui::vec2(width + 4.0, radius * 2.0));
        let painter = ui.painter();
        painter.rect_filled(rect, egui::Rounding::same(radius), Palette::ACCENT());
        painter.text(
            center,
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(if compact { 9.5 } else { 11.0 }),
            Color32::WHITE,
        );
    }
}

/// Where the next sidebar row will start; pass it to [`CommandNotifier::paint_badge`].
pub fn row_top(ui: &egui::Ui) -> egui::Rect {
    egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 0.0))
}

fn send_desktop_notification(notice: &Notice, clicks: Sender<Uuid>, ctx: egui::Context) {
    let mut notification = notify_rust::Notification::new();
    notification
        .appname("Modern Rust Terminal")
        .summary(&notice.summary())
        .body(&notice.body());

    // Only the freedesktop backend reports clicks; Windows and macOS just show the toast.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        notification.action("default", "Show in MRT");
        match notification.show() {
            Ok(handle) => {
                let tab_id = notice.tab_id;
                handle.wait_for_action(|action| {
                    if action == "default" {
                        let _ = clicks.send(tab_id);
                        ctx.request_repaint();
                    }
                });
            }
            Err(error) => tracing::warn!("desktop notification failed: {error}"),
        }
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (clicks, ctx);
        if let Err(error) = notification.show() {
            tracing::warn!("desktop notification failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tab(title: &str) -> TerminalTab {
        TerminalTab::new(title.into(), 0, PathBuf::from("/work/project"))
    }

    fn running(command: &str) -> CommandBlock {
        let mut block = CommandBlock::new_completed(command, "bash", "/work/project", "", 0, 0);
        block.exit_code = None;
        block
    }

    fn finish(block: &mut CommandBlock, exit_code: i32, duration_ms: u64) {
        block.exit_code = Some(exit_code);
        block.duration_ms = duration_ms;
    }

    fn view(window_focused: bool, active_tab: usize) -> ViewState {
        ViewState {
            window_focused,
            active_tab,
            mosaic: false,
        }
    }

    fn notifier() -> CommandNotifier {
        CommandNotifier::with_settings(NotificationSettings::default(), false)
    }

    #[test]
    fn decision_needs_threshold_and_background() {
        let s = NotificationSettings::default();
        assert!(!should_notify(&s, 9_999, false, false), "below threshold");
        assert!(should_notify(&s, 10_000, false, true), "unfocused window");
        assert!(should_notify(&s, 10_000, true, false), "hidden tab");
        assert!(!should_notify(&s, 60_000, true, true), "user is looking at it");
        let off = NotificationSettings { enabled: false, ..s };
        assert!(!should_notify(&off, 60_000, false, false), "disabled");
        let short = NotificationSettings { threshold_secs: 2, ..s };
        assert!(should_notify(&short, 2_000, false, true));
    }

    #[test]
    fn long_command_in_unfocused_window_is_announced_once() {
        let mut n = notifier();
        let mut tabs = vec![tab("Build")];
        tabs[0].blocks.push(running("cargo build --release"));
        assert!(n.observe(&tabs, view(false, 0)).is_empty());

        finish(&mut tabs[0].blocks[0], 101, 42_000);
        let notices = n.observe(&tabs, view(false, 0));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].summary(), "✗ Command failed (exit 101)");
        assert_eq!(notices[0].body(), "cargo build --release\n42.0s · Build");
        assert!(n.observe(&tabs, view(false, 0)).is_empty(), "no repeats");
        assert_eq!(n.unseen(tabs[0].id), 0, "visible tab gets no badge");
    }

    #[test]
    fn hidden_tab_notifies_and_badges_until_viewed() {
        let mut n = notifier();
        let mut tabs = vec![tab("Main"), tab("Tests")];
        tabs[1].blocks.push(running("cargo test"));
        n.observe(&tabs, view(true, 0));

        finish(&mut tabs[1].blocks[0], 0, 15_000);
        let notices = n.observe(&tabs, view(true, 0));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].summary(), "✓ Command finished");
        assert_eq!(notices[0].tab_id, tabs[1].id);
        assert_eq!(n.unseen(tabs[1].id), 1);

        // Short commands in a hidden tab still count as unseen but do not notify.
        tabs[1].blocks.push(CommandBlock::new_completed("ls", "bash", "/", "", 0, 20));
        assert!(n.observe(&tabs, view(true, 0)).is_empty());
        assert_eq!(n.unseen(tabs[1].id), 2);

        n.observe(&tabs, view(true, 1));
        assert_eq!(n.unseen(tabs[1].id), 0, "switching to the tab clears the badge");
    }

    #[test]
    fn short_or_watched_commands_stay_quiet() {
        let mut n = notifier();
        let mut tabs = vec![tab("Main")];
        tabs[0].blocks.push(running("sleep 3"));
        n.observe(&tabs, view(false, 0));
        finish(&mut tabs[0].blocks[0], 0, 3_000);
        assert!(n.observe(&tabs, view(false, 0)).is_empty(), "under the threshold");

        tabs[0].blocks.push(running("sleep 30"));
        n.observe(&tabs, view(true, 0));
        finish(&mut tabs[0].blocks[1], 0, 30_000);
        assert!(n.observe(&tabs, view(true, 0)).is_empty(), "focused and visible");
    }

    #[test]
    fn mosaic_makes_every_tab_visible() {
        let mut n = notifier();
        let mut tabs = vec![tab("A"), tab("B")];
        tabs[1].blocks.push(running("make"));
        let mosaic = ViewState {
            window_focused: true,
            active_tab: 0,
            mosaic: true,
        };
        n.observe(&tabs, mosaic);
        finish(&mut tabs[1].blocks[0], 0, 60_000);
        assert!(n.observe(&tabs, mosaic).is_empty());
        assert_eq!(n.unseen(tabs[1].id), 0);

        // A tab shown in another window is visible too.
        tabs.push(tab("C"));
        tabs[2].blocks.push(running("cargo test"));
        let main_only = view(true, 0);
        n.observe_with(&tabs, main_only, &[tabs[2].id]);
        finish(&mut tabs[2].blocks[0], 0, 60_000);
        assert!(n.observe_with(&tabs, main_only, &[tabs[2].id]).is_empty());
        assert_eq!(n.unseen(tabs[2].id), 0);
    }

    #[test]
    fn blocks_added_already_finished_are_announced_but_existing_ones_are_not() {
        let mut n = notifier();
        let mut tabs = vec![tab("Main")];
        tabs[0].blocks.push(CommandBlock::new_completed("old", "bash", "/", "", 0, 99_000));
        assert!(n.observe(&tabs, view(false, 0)).is_empty(), "present before the first frame");

        // The TUI overlay and some built-ins push a block that is already finished.
        tabs[0].blocks.push(CommandBlock::new_completed("vim notes.md", "bash", "/", "", 0, 120_000));
        let notices = n.observe(&tabs, view(false, 0));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].command, "vim notes.md");
    }

    #[test]
    fn secondary_pane_blocks_are_watched() {
        let mut n = notifier();
        let mut tabs = vec![tab("Main"), tab("Split")];
        tabs[1].split(crate::app::SplitDirection::Vertical, 1);
        tabs[1].secondary_pane.as_mut().unwrap().blocks.push(running("npm run build"));
        n.observe(&tabs, view(true, 0));
        finish(&mut tabs[1].secondary_pane.as_mut().unwrap().blocks[0], 1, 11_000);
        let notices = n.observe(&tabs, view(true, 0));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].tab_id, tabs[1].id);
    }

    #[test]
    fn restarted_block_is_announced_again_and_closed_tabs_are_forgotten() {
        let mut n = notifier();
        let mut tabs = vec![tab("Main"), tab("Bg")];
        tabs[1].blocks.push(running(":top"));
        n.observe(&tabs, view(true, 0));
        finish(&mut tabs[1].blocks[0], 0, 12_000);
        assert_eq!(n.observe(&tabs, view(true, 0)).len(), 1);
        tabs[1].blocks[0].exit_code = None;
        n.observe(&tabs, view(true, 0));
        finish(&mut tabs[1].blocks[0], 0, 24_000);
        assert_eq!(n.observe(&tabs, view(true, 0)).len(), 1);
        assert_eq!(n.unseen(tabs[1].id), 2);

        let closed = tabs.remove(1);
        n.observe(&tabs, view(true, 0));
        assert_eq!(n.unseen(closed.id), 0);
        assert!(n.finished.is_empty());
    }

    #[test]
    fn notice_text_is_truncated_and_durations_are_readable() {
        let long = "x".repeat(200);
        assert_eq!(truncate(&long, 10).chars().count(), 10);
        assert_eq!(truncate("echo a\necho b", 80), "echo a…");
        assert_eq!(format_duration(850), "850ms");
        assert_eq!(format_duration(12_340), "12.3s");
        assert_eq!(format_duration(125_000), "2m 05s");
        assert_eq!(format_duration(3_720_000), "1h 02m");
        let generic = tab("Tab 1 (bash)");
        assert_eq!(tab_name(&generic), "Tab 1 (bash) · project");
    }

    #[test]
    fn settings_parse_with_defaults_for_missing_fields() {
        let parsed: NotificationSettings = serde_json::from_str(r#"{"enabled": false}"#).unwrap();
        assert_eq!(parsed, NotificationSettings { enabled: false, threshold_secs: 10 });
    }
}
