use crate::history::{Runbook, RunbookStep};
use crate::theme::Palette;
use chrono::Utc;
use egui::{RichText, Ui};
use mrt_core::CommandBlock;
use uuid::Uuid;

#[derive(Default)]
pub struct RunbooksUiState {
    pub show_panel: bool,
    pub runbooks: Vec<Runbook>,
    pub preview_id: Option<Uuid>,
    pub editing: Option<Runbook>,
    pub validation_error: Option<String>,
    pub replay_notice: Option<String>,
}

pub struct ReplayStatusView<'a> {
    pub runbook_name: &'a str,
    pub current_step: usize,
    pub total_steps: usize,
}

pub enum RunbookAction {
    StartReplay(Runbook),
    StopReplay,
    SaveRunbook(Runbook),
    DeleteRunbook(Uuid),
    SearchHistory,
}

pub fn render_runbooks_panel(
    ui: &mut Ui,
    state: &mut RunbooksUiState,
    replay_status: Option<ReplayStatusView<'_>>,
    history_query: &mut String,
    history_results: &[CommandBlock],
    available_shells: &[String],
    default_shell: &str,
    default_cwd: &str,
) -> Option<RunbookAction> {
    let mut action = None;

    ui.heading("Runbooks");
    ui.label(
        RichText::new("Save command sequences and replay them step-by-step through the Safety Gate.")
            .size(11.5)
            .color(Palette::TEXT_MUTED),
    );
    ui.add_space(6.0);

    if let Some(ref status) = replay_status {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new(format!(
                        "Running '{}' ({}/{})",
                        status.runbook_name, status.current_step, status.total_steps
                    ))
                    .strong()
                    .color(Palette::ACCENT_BLUE),
                );
                if ui.button("Stop").clicked() {
                    action = Some(RunbookAction::StopReplay);
                }
            });
        });
        ui.add_space(6.0);
    }

    if let Some(notice) = state.replay_notice.clone() {
        ui.group(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(notice).size(12.0));
                if ui.small_button("Dismiss").clicked() {
                    state.replay_notice = None;
                }
            });
        });
        ui.add_space(6.0);
    }

    if let Some(mut draft) = state.editing.take() {
        let mut keep_editing = true;
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.add(
                egui::TextEdit::singleline(&mut draft.name)
                    .hint_text("e.g. Check this project")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(6.0);

        if let Some(err) = &state.validation_error {
            ui.colored_label(Palette::ACCENT_RED, err);
            ui.add_space(4.0);
        }

        ui.label(RichText::new("Steps (executed in order)").strong());
        let mut move_up = None;
        let mut move_down = None;
        let mut remove_idx = None;
        let step_count = draft.steps.len();

        egui::ScrollArea::vertical()
            .id_salt("runbook_editor_steps")
            .max_height(240.0)
            .show(ui, |ui| {
                if draft.steps.is_empty() {
                    ui.label(
                        RichText::new("No steps yet. Add a step below or pick from history.")
                            .size(11.5)
                            .color(Palette::TEXT_MUTED),
                    );
                }
                for (idx, step) in draft.steps.iter_mut().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("#{}", idx + 1)).strong());
                            ui.add(
                                egui::TextEdit::singleline(&mut step.command)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("Command")
                                    .desired_width(180.0),
                            );
                            if ui.add_enabled(idx > 0, egui::Button::new("^").small()).clicked() {
                                move_up = Some(idx);
                            }
                            if ui
                                .add_enabled(idx + 1 < step_count, egui::Button::new("v").small())
                                .clicked()
                            {
                                move_down = Some(idx);
                            }
                            if ui.small_button("x").clicked() {
                                remove_idx = Some(idx);
                            }
                        });
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(format!("rb_shell_{idx}"))
                                .selected_text(&step.shell_name)
                                .width(80.0)
                                .show_ui(ui, |ui| {
                                    for shell in available_shells {
                                        ui.selectable_value(
                                            &mut step.shell_name,
                                            shell.clone(),
                                            shell,
                                        );
                                    }
                                });
                            ui.add(
                                egui::TextEdit::singleline(&mut step.cwd)
                                    .hint_text("Working directory")
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });
                }
            });

        if let Some(idx) = move_up {
            draft.steps.swap(idx, idx - 1);
        } else if let Some(idx) = move_down {
            draft.steps.swap(idx, idx + 1);
        } else if let Some(idx) = remove_idx {
            draft.steps.remove(idx);
        }

        ui.horizontal(|ui| {
            if ui.button("+ Add blank step").clicked() {
                draft.steps.push(RunbookStep {
                    command: String::new(),
                    shell_name: default_shell.to_string(),
                    cwd: default_cwd.to_string(),
                });
            }
            if ui.button("Save runbook").clicked() {
                let name = draft.name.trim().to_string();
                let has_empty_cmd = draft.steps.iter().any(|s| s.command.trim().is_empty());
                if name.is_empty() {
                    state.validation_error = Some("Runbook name cannot be empty.".into());
                } else if draft.steps.is_empty() {
                    state.validation_error = Some("Add at least one step.".into());
                } else if has_empty_cmd {
                    state.validation_error = Some("Step commands cannot be empty.".into());
                } else {
                    draft.name = name;
                    for step in &mut draft.steps {
                        step.command = step.command.trim().to_string();
                        if step.cwd.trim().is_empty() {
                            step.cwd = default_cwd.to_string();
                        }
                    }
                    draft.updated_at = Utc::now();
                    state.validation_error = None;
                    state.preview_id = Some(draft.id);
                    action = Some(RunbookAction::SaveRunbook(draft.clone()));
                    keep_editing = false;
                }
            }
            if ui.button("Cancel").clicked() {
                state.validation_error = None;
                keep_editing = false;
            }
        });

        if keep_editing {
            ui.separator();
            ui.label(RichText::new("Add from command history").strong());
            if ui
                .add(
                    egui::TextEdit::singleline(history_query)
                        .hint_text("Filter history commands"),
                )
                .changed()
            {
                action = Some(RunbookAction::SearchHistory);
            }
            egui::ScrollArea::vertical()
                .id_salt("runbook_history_picker")
                .show(ui, |ui| {
                    if history_results.is_empty() {
                        ui.label(
                            RichText::new("No matching history commands.")
                                .size(11.5)
                                .color(Palette::TEXT_MUTED),
                        );
                    }
                    for block in history_results {
                        ui.horizontal(|ui| {
                            if ui.small_button("+ Add").clicked() {
                                draft.steps.push(RunbookStep {
                                    command: block.command.clone(),
                                    shell_name: block.shell_name.clone(),
                                    cwd: block.cwd.clone(),
                                });
                            }
                            ui.vertical(|ui| {
                                ui.monospace(&block.command);
                                ui.small(format!("{} · {}", block.shell_name, block.cwd));
                            });
                        });
                    }
                });
            state.editing = Some(draft);
        }
        return action;
    }

    if let Some(preview_id) = state.preview_id {
        if let Some(runbook) = state.runbooks.iter().find(|rb| rb.id == preview_id).cloned() {
            ui.horizontal(|ui| {
                ui.heading(&runbook.name);
            });
            ui.small(format!(
                "{} step(s) · updated {}",
                runbook.steps.len(),
                runbook.updated_at.format("%Y-%m-%d %H:%M")
            ));
            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .id_salt("runbook_preview_steps")
                .max_height(360.0)
                .show(ui, |ui| {
                    for (idx, step) in runbook.steps.iter().enumerate() {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{}.", idx + 1)).strong());
                                ui.monospace(&step.command);
                            });
                            ui.small(format!("{} · {}", step.shell_name, step.cwd));
                        });
                    }
                });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_run = replay_status.is_none() && !runbook.steps.is_empty();
                if ui
                    .add_enabled(can_run, egui::Button::new("Run in active tab"))
                    .clicked()
                {
                    action = Some(RunbookAction::StartReplay(runbook.clone()));
                }
                if ui.button("Edit").clicked() {
                    state.validation_error = None;
                    state.editing = Some(runbook);
                }
                if ui.button("Back").clicked() {
                    state.preview_id = None;
                }
            });
            return action;
        } else {
            state.preview_id = None;
        }
    }

    if ui.button("+ New runbook").clicked() {
        state.validation_error = None;
        state.editing = Some(Runbook {
            id: Uuid::new_v4(),
            name: String::new(),
            steps: Vec::new(),
            updated_at: Utc::now(),
        });
        return action;
    }
    ui.add_space(6.0);

    if state.runbooks.is_empty() {
        ui.label(
            RichText::new("No saved runbooks yet. Create one or click '+ Runbook' on any item in History.")
                .size(12.0)
                .color(Palette::TEXT_MUTED),
        );
    } else {
        egui::ScrollArea::vertical()
            .id_salt("runbook_list")
            .show(ui, |ui| {
                for runbook in &state.runbooks {
                    ui.group(|ui| {
                        ui.label(RichText::new(&runbook.name).strong());
                        ui.small(format!(
                            "{} step(s) · {}",
                            runbook.steps.len(),
                            runbook.updated_at.format("%Y-%m-%d %H:%M")
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("Preview / Run").clicked() {
                                state.preview_id = Some(runbook.id);
                            }
                            if ui.button("Edit").clicked() {
                                state.validation_error = None;
                                state.editing = Some(runbook.clone());
                            }
                            if ui.button("Delete").clicked() {
                                action = Some(RunbookAction::DeleteRunbook(runbook.id));
                            }
                        });
                    });
                }
            });
    }

    action
}
