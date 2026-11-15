//! Toolbox side panel: one panel with a tab per tool family. Adding a tool family means
//! adding a `ToolboxTab` variant and a render function in its own widget file.

use crate::theme::Palette;
use crate::widgets::side_panels::{render_network_tab, SidePanelEvent, SidePanelState};
use crate::widgets::task_manager::render_task_manager_tab;
use egui::{Color32, RichText, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolboxTab {
    #[default]
    Network,
    TaskManager,
}

impl ToolboxTab {
    pub const ALL: [ToolboxTab; 2] = [ToolboxTab::Network, ToolboxTab::TaskManager];

    pub fn label(self) -> &'static str {
        match self {
            ToolboxTab::Network => "Network",
            ToolboxTab::TaskManager => "Task Manager",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            ToolboxTab::Network => "🌐",
            ToolboxTab::TaskManager => "📈",
        }
    }

    /// Accepts the names used by `mrt_control_gui` and the Command Palette.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().replace(['-', ' '], "_").as_str() {
            "network" | "net" | "internet" => Some(ToolboxTab::Network),
            "task_manager" | "taskmanager" | "tasks" | "processes" | "process" => Some(ToolboxTab::TaskManager),
            _ => None,
        }
    }
}

pub fn render_toolbox_side_panel(ui: &mut Ui, state: &mut SidePanelState) -> Option<SidePanelEvent> {
    ui.label(
        RichText::new("Toolbox")
            .size(15.0)
            .strong()
            .color(Palette::TEXT_PRIMARY),
    );
    ui.add_space(4.0);

    // Tab strip
    egui::Frame::none()
        .fill(Palette::BG_INSET)
        .rounding(egui::Rounding::same(10.0))
        .inner_margin(egui::Margin::same(3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for tab in ToolboxTab::ALL {
                    let selected = state.toolbox_tab == tab;
                    let button = egui::Button::new(
                        RichText::new(format!("{}  {}", tab.icon(), tab.label()))
                            .size(12.5)
                            .color(if selected { Palette::TEXT_PRIMARY } else { Palette::TEXT_SECONDARY }),
                    )
                    .fill(if selected { Palette::BORDER_SUBTLE } else { Color32::TRANSPARENT })
                    .stroke(egui::Stroke::NONE)
                    .rounding(egui::Rounding::same(8.0))
                    .min_size(egui::vec2(0.0, 26.0));
                    if ui.add(button).clicked() {
                        state.toolbox_tab = tab;
                    }
                }
            });
        });
    ui.add_space(6.0);

    match state.toolbox_tab {
        ToolboxTab::Network => {
            let snapshot = state.task_manager.snapshot.clone();
            egui::ScrollArea::vertical()
                .id_salt("toolbox_network_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| render_network_tab(ui, state, snapshot.as_deref()))
                .inner
        }
        ToolboxTab::TaskManager => {
            let ports = std::mem::take(&mut state.last_ports);
            let event = render_task_manager_tab(ui, &mut state.task_manager, &ports);
            state.last_ports = ports;
            event.map(SidePanelEvent::TaskManager)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tab_names() {
        assert_eq!(ToolboxTab::parse("task_manager"), Some(ToolboxTab::TaskManager));
        assert_eq!(ToolboxTab::parse("Task Manager"), Some(ToolboxTab::TaskManager));
        assert_eq!(ToolboxTab::parse("internet"), Some(ToolboxTab::Network));
        assert_eq!(ToolboxTab::parse("bogus"), None);
    }

    #[test]
    fn renders_both_tabs() {
        let mut state = SidePanelState::default();
        let ctx = egui::Context::default();
        for tab in ToolboxTab::ALL {
            state.toolbox_tab = tab;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = render_toolbox_side_panel(ui, &mut state);
                });
            });
        }
    }
}
