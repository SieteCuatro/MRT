//! Several windows: the main window plus extra ones (egui viewports), each showing
//! its own tabs. Every tab lives in `MrtGuiApp::tabs` and `TerminalTab::window` says
//! which window draws it, so moving a tab to another window only changes that number:
//! the tab keeps its shell, blocks and running commands.

use crate::app::{ExtraWindow, MrtGuiApp, TerminalTab};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

/// The egui viewport of extra window `id`.
pub(crate) fn viewport_id(id: u32) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("mrt_window", id))
}

fn window_icon() -> Option<Arc<egui::IconData>> {
    static ICON: OnceLock<Option<Arc<egui::IconData>>> = OnceLock::new();
    ICON.get_or_init(|| eframe::icon_data::from_png_bytes(include_bytes!("../assets/mrt-256.png")).ok().map(Arc::new))
        .clone()
}

impl MrtGuiApp {
    /// The window showing tab `tab_id` (0, the main window, for unknown tabs).
    pub(crate) fn window_of(&self, tab_id: Uuid) -> u32 {
        self.tabs.iter().find(|t| t.id == tab_id).map(|t| t.window).unwrap_or(0)
    }

    /// Indices into `tabs` of the tabs window `window` shows, in sidebar order.
    pub(crate) fn window_tab_indices(&self, window: u32) -> Vec<usize> {
        self.tabs.iter().enumerate().filter(|(_, t)| t.window == window).map(|(i, _)| i).collect()
    }

    /// Tabs on screen in the extra windows: each one's active tab, or all its tabs
    /// in Mosaic (they count as seen for the unseen-results badges).
    pub(crate) fn visible_in_extra_windows(&self) -> Vec<Uuid> {
        self.windows
            .iter()
            .flat_map(|w| {
                self.tabs.iter().filter(move |t| t.window == w.id && (w.mosaic || t.id == w.active)).map(|t| t.id)
            })
            .collect()
    }

    /// The window the Safety Gate shows in: the one of the tab the command is for.
    pub(crate) fn safety_window(&self) -> u32 {
        self.pending_safety.as_ref().map(|p| self.window_of(p.tab_id)).unwrap_or(0)
    }

    /// Shows tab `idx` (a clicked notification) in its window and brings that window
    /// to the front.
    pub(crate) fn show_tab(&mut self, ctx: &egui::Context, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else { return };
        let (id, window) = (tab.id, tab.window);
        if window == 0 {
            self.active_tab = idx;
        } else if let Some(extra) = self.windows.iter_mut().find(|w| w.id == window) {
            extra.active = id;
            ctx.send_viewport_cmd_to(viewport_id(window), egui::ViewportCommand::Focus);
        }
    }

    /// Opens a window for each window number found in the tabs (a restored layout).
    pub(crate) fn reopen_saved_windows(&mut self) {
        let mut ids: Vec<u32> = self.tabs.iter().map(|t| t.window).filter(|&w| w != 0).collect();
        ids.sort_unstable();
        ids.dedup();
        self.windows = ids
            .iter()
            .filter_map(|&id| {
                let active = self.tabs.iter().find(|t| t.window == id)?.id;
                Some(ExtraWindow { id, active, mosaic: false, fresh: true })
            })
            .collect();
        self.next_window_id = ids.last().map_or(1, |last| last + 1);
        self.normalize_windows();
    }

    /// Keeps the windows consistent after tabs opened, closed or moved: tabs of a
    /// window that is gone go to the main window, empty extra windows close, each
    /// window shows one of its own tabs, and the main window always has a tab.
    pub(crate) fn normalize_windows(&mut self) {
        let known: HashSet<u32> = self.windows.iter().map(|w| w.id).collect();
        for tab in &mut self.tabs {
            if tab.window != 0 && !known.contains(&tab.window) {
                tab.window = 0;
            }
        }
        let tabs = &self.tabs;
        self.windows.retain(|w| tabs.iter().any(|t| t.window == w.id));
        for window in &mut self.windows {
            if !tabs.iter().any(|t| t.id == window.active && t.window == window.id) {
                if let Some(first) = tabs.iter().find(|t| t.window == window.id) {
                    window.active = first.id;
                }
            }
        }
        if !self.tabs.iter().any(|t| t.window == 0) {
            // The main window lost its last tab (closed through MCP, say): a new one.
            let (shell_idx, cwd) = self
                .tabs
                .first()
                .map(|t| (t.active_shell_idx, t.cwd.clone()))
                .unwrap_or_else(|| (0, std::env::current_dir().unwrap_or_else(|_| ".".into())));
            let label = self.shells.get(shell_idx).map(|s| s.kind.short_label()).unwrap_or("shell");
            self.tabs.push(TerminalTab::new(format!("Tab {} ({label})", self.tabs.len() + 1), shell_idx, cwd));
        }
        if self.tabs.get(self.active_tab).is_none_or(|t| t.window != 0) {
            self.active_tab = self.tabs.iter().position(|t| t.window == 0).unwrap_or(0);
        }
        if self.palette_window != 0 && !self.windows.iter().any(|w| w.id == self.palette_window) {
            self.palette_window = 0;
        }
    }

    /// Draws every extra window. Immediate viewports run inside this frame with full
    /// access to the app; while one is drawn, `active_tab` and `mosaic` are that
    /// window's, then the main window's are put back.
    pub(crate) fn render_extra_windows(&mut self, ctx: &egui::Context) {
        if self.windows.is_empty() {
            return;
        }
        let main_active = self.tabs.get(self.active_tab).map(|t| t.id);
        let main_mosaic = self.mosaic;
        for slot in 0..self.windows.len() {
            let window = self.windows[slot].clone();
            let Some(idx) = self.tabs.iter().position(|t| t.id == window.active) else { continue };
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("Modern Rust Terminal · window {}", window.id + 1))
                .with_app_id("mrt")
                .with_inner_size([1100.0, 720.0])
                .with_min_inner_size([640.0, 420.0]);
            if let Some(icon) = window_icon() {
                builder = builder.with_icon(icon);
            }
            let (active, mosaic) = ctx.show_viewport_immediate(viewport_id(window.id), builder, |vctx, class| {
                if class == egui::ViewportClass::Embedded {
                    // This backend cannot open windows: the tabs go back to the main one.
                    self.pending_window_closes.push(window.id);
                    return (window.active, window.mosaic);
                }
                if vctx.input(|i| i.viewport().close_requested()) {
                    self.pending_window_closes.push(window.id);
                }
                self.current_window = window.id;
                self.active_tab = idx;
                self.mosaic = window.mosaic;
                self.window_frame(vctx, window.fresh);
                let active = self
                    .tabs
                    .get(self.active_tab)
                    .filter(|t| t.window == window.id)
                    .map(|t| t.id)
                    .unwrap_or(window.active);
                (active, self.mosaic)
            });
            if let Some(extra) = self.windows.get_mut(slot) {
                extra.active = active;
                extra.mosaic = mosaic;
                extra.fresh = false;
            }
        }
        self.current_window = 0;
        self.mosaic = main_mosaic;
        self.active_tab = main_active.and_then(|id| self.tabs.iter().position(|t| t.id == id)).unwrap_or(0);
    }

    /// Applies the window changes asked for while the windows were drawn: closed
    /// windows give their tabs back to the main window, and queued tab moves happen.
    /// A tab only moves to a new window when its window keeps other tabs, and the
    /// main window always keeps one.
    pub(crate) fn apply_window_changes(&mut self) {
        for id in std::mem::take(&mut self.pending_window_closes) {
            for tab in &mut self.tabs {
                if tab.window == id {
                    tab.window = 0;
                }
            }
            self.windows.retain(|w| w.id != id);
        }
        for (tab_id, target) in std::mem::take(&mut self.pending_tab_moves) {
            let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else { continue };
            let source = self.tabs[idx].window;
            let source_tabs = self.tabs.iter().filter(|t| t.window == source).count();
            if source_tabs < 2 && (source == 0 || target.is_none()) {
                continue;
            }
            let target = match target {
                Some(window) if window == 0 || self.windows.iter().any(|w| w.id == window) => window,
                Some(_) => continue,
                None => {
                    let id = self.next_window_id;
                    self.next_window_id += 1;
                    self.windows.push(ExtraWindow { id, active: tab_id, mosaic: false, fresh: true });
                    id
                }
            };
            if target == source {
                continue;
            }
            self.tabs[idx].window = target;
            if target == 0 {
                self.active_tab = idx;
            } else if let Some(window) = self.windows.iter_mut().find(|w| w.id == target) {
                window.active = tab_id;
            }
        }
        self.normalize_windows();
    }
}
