use crate::theme::{chip, ghost_button, icon_button, primary_button, secondary_button, Palette, Radius};
use egui::{Color32, RichText, Ui};
use mrt_core::{CommandBlock, HttpExchangeSummary, StructuredOutput, ViewMode};
use mrt_tools::{compare_blocks_structured, ChangeType, StructuredDiffKind};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum BlockAction {
    Compare(uuid::Uuid, uuid::Uuid),
    Rerun(String),
    DiagnoseError(uuid::Uuid),
    ApplySuggestedFix(String),
    CopyBugReport(uuid::Uuid),
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Left indent that aligns block bodies with the command text after the status icon.
const BODY_INDENT: f32 = 26.0;

pub fn render_command_block(
    ui: &mut Ui,
    block: &mut CommandBlock,
    table_filter: &mut String,
    previous_id: Option<uuid::Uuid>,
) -> Option<BlockAction> {
    let mut triggered_action = None;
    let failed = matches!(block.exit_code, Some(c) if c != 0);

    egui::Frame::none()
        .fill(Palette::BG_SURFACE)
        .stroke(egui::Stroke::new(
            1.0_f32,
            if failed {
                Palette::FAILED_BORDER
            } else {
                Palette::BORDER_FAINT
            },
        ))
        .rounding(egui::Rounding::same(Radius::BLOCK))
        .inner_margin(egui::Margin {
            left: 16.0,
            right: 12.0,
            top: 10.0,
            bottom: 14.0,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // Header: status · command · chips/meta · actions
            // Right side is laid out first so the command gets the remaining width and truncates.
            ui.horizontal(|ui| {

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if failed
                        && ui
                            .add(ghost_button("Report"))
                            .on_hover_text("Copy a redacted Markdown bug report")
                            .clicked()
                    {
                        triggered_action = Some(BlockAction::CopyBugReport(block.id));
                    }
                    if let Some(previous_id) = previous_id.filter(|_| block.exit_code.is_some()) {
                        if ui
                            .add(ghost_button("Diff"))
                            .on_hover_text("Compare with the previous run")
                            .clicked()
                        {
                            triggered_action = Some(BlockAction::Compare(previous_id, block.id));
                        }
                    }
                    if icon_button(ui, "🔁", "Rerun").clicked() {
                        triggered_action = Some(BlockAction::Rerun(block.command.clone()));
                    }
                    if ui.add(ghost_button("Copy")).on_hover_text("Copy output").clicked() {
                        ui.output_mut(|o| o.copied_text = block.plain_output.clone());
                    }
                    if block.structured.is_structured() {
                        let toggle_label = match block.view_mode {
                            ViewMode::Structured => "Raw",
                            ViewMode::RawAnsi => "Structured",
                        };
                        if ui.add(ghost_button(toggle_label)).clicked() {
                            block.view_mode = match block.view_mode {
                                ViewMode::Structured => ViewMode::RawAnsi,
                                ViewMode::RawAnsi => ViewMode::Structured,
                            };
                        }
                    }
                    if failed
                        && block.ai_explanation.is_none()
                        && ui
                            .add(
                                egui::Button::new(
                                    RichText::new("✨ Diagnose")
                                        .size(12.5)
                                        .color(Palette::ACCENT_TEXT),
                                )
                                .frame(false)
                                .min_size(egui::vec2(0.0, 28.0)),
                            )
                            .on_hover_text("Ask AI why this failed")
                            .clicked()
                    {
                        triggered_action = Some(BlockAction::DiagnoseError(block.id));
                    }

                    ui.add_space(8.0);
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let meta = match block.exit_code {
                        Some(_) => format!(
                            "{} · {}",
                            format_duration(block.duration_ms as u64),
                            block.timestamp.format("%H:%M")
                        ),
                        None => "running".to_string(),
                    };
                    ui.label(RichText::new(meta).size(12.0).color(Palette::TEXT_MUTED))
                        .on_hover_text(format!(
                            "{} · {} · {}",
                            block.shell_name,
                            block.cwd,
                            block.timestamp.format("%Y-%m-%d %H:%M:%S")
                        ));
                    if let Some(code) = block.exit_code.filter(|c| *c != 0) {
                        chip(
                            ui,
                            &format!("exit {code}"),
                            Color32::from_rgb(58, 31, 34),
                            Color32::from_rgb(252, 165, 165),
                        );
                    }
                    if block.structured.is_structured() && matches!(block.view_mode, ViewMode::Structured) {
                        let badge = block
                            .structured
                            .badge_label()
                            .split_whitespace()
                            .next()
                            .unwrap_or("Data")
                            .to_owned();
                        chip(ui, &badge, Color32::from_rgb(34, 38, 58), Palette::ACCENT_TEXT);
                    }

                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        match block.exit_code {
                            Some(0) => {
                                ui.label(RichText::new("✔").size(14.0).color(Palette::ACCENT_GREEN))
                                    .on_hover_text("Succeeded");
                            }
                            Some(_) => {
                                ui.label(RichText::new("✖").size(14.0).color(Palette::ACCENT_RED))
                                    .on_hover_text("Failed");
                            }
                            None => {
                                ui.add(egui::Spinner::new().size(14.0).color(Palette::ACCENT));
                            }
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(&block.command)
                                    .monospace()
                                    .size(13.5)
                                    .color(Palette::TEXT_PRIMARY),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&block.command);
                    });
                });
            });

            // Body
            egui::Frame::none()
                .inner_margin(egui::Margin {
                    left: BODY_INDENT,
                    right: 4.0,
                    top: 2.0,
                    bottom: 0.0,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    match (&block.view_mode, &block.structured) {
                        (ViewMode::Structured, StructuredOutput::Json(val)) => {
                            egui::Frame::none()
                                .fill(Palette::BG_INSET)
                                .rounding(egui::Rounding::same(Radius::BUTTON))
                                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    render_json_tree(ui, "root", val, true);
                                });
                        }
                        (ViewMode::Structured, StructuredOutput::Table { headers, rows }) => {
                            render_structured_table(ui, block.id, headers, rows, table_filter);
                        }
                        (ViewMode::Structured, StructuredOutput::HttpResult(http)) => {
                            render_http_exchange(ui, http);
                        }
                        (ViewMode::Structured, StructuredOutput::Markdown(md)) => {
                            render_markdown_block(ui, md);
                        }
                        _ => {
                            render_ansi_spans(ui, block);
                        }
                    }

                    // Inline AI diagnosis
                    if let Some(ref explanation) = block.ai_explanation {
                        ui.add_space(10.0);
                        egui::Frame::none()
                            .fill(Palette::AI_FILL)
                            .stroke(egui::Stroke::new(1.0_f32, Palette::AI_BORDER))
                            .rounding(egui::Rounding::same(12.0))
                            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("✨").size(14.0).color(Palette::ACCENT));
                                    ui.label(
                                        RichText::new("Diagnosis")
                                            .size(13.0)
                                            .strong()
                                            .color(Palette::ACCENT_TEXT),
                                    );
                                });
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(explanation)
                                        .size(14.0)
                                        .color(Palette::TEXT_BODY),
                                );
                                if let Some(ref fix_cmd) = block.ai_suggested_fix {
                                    ui.add_space(6.0);
                                    egui::Frame::none()
                                        .fill(Palette::BG_INSET)
                                        .rounding(egui::Rounding::same(Radius::BUTTON))
                                        .inner_margin(egui::Margin {
                                            left: 12.0,
                                            right: 8.0,
                                            top: 6.0,
                                            bottom: 6.0,
                                        })
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new(fix_cmd)
                                                        .monospace()
                                                        .size(13.0)
                                                        .color(Palette::TEXT_PRIMARY),
                                                );
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(egui::Align::Center),
                                                    |ui| {
                                                        if ui.add(primary_button("Run")).clicked() {
                                                            triggered_action = Some(
                                                                BlockAction::ApplySuggestedFix(
                                                                    fix_cmd.clone(),
                                                                ),
                                                            );
                                                        }
                                                        if ui.add(secondary_button("Copy")).clicked() {
                                                            ui.output_mut(|o| {
                                                                o.copied_text = fix_cmd.clone()
                                                            });
                                                        }
                                                    },
                                                );
                                            });
                                        });
                                }
                            });
                    }
                });
        });

    triggered_action
}

fn render_ansi_spans(ui: &mut Ui, block: &CommandBlock) {
    if block.ansi_spans.is_empty() {
        ui.label(
            RichText::new("(no output)")
                .italics()
                .color(Palette::TEXT_MUTED),
        );
        return;
    }

    let mut job = egui::text::LayoutJob::default();
    for span in &block.ansi_spans {
        let fg = span
            .fg
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Palette::TEXT_BODY);
        let bg = span
            .bg
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Color32::TRANSPARENT);

        let font_id = egui::FontId::monospace(13.0);
        let mut format = egui::TextFormat {
            font_id,
            color: fg,
            background: bg,
            italics: span.italic,
            ..Default::default()
        };
        if span.underline {
            format.underline = egui::Stroke::new(1.0_f32, fg);
        }
        job.append(&span.text, 0.0, format);
    }

    ui.label(job);
}

fn render_json_tree(ui: &mut Ui, key: &str, value: &Value, default_open: bool) {
    match value {
        Value::Object(map) => {
            let header = format!("{}: {{ {} keys }}", key, map.len());
            egui::CollapsingHeader::new(
                RichText::new(header)
                    .monospace()
                    .color(Palette::ACCENT_TEXT),
            )
            .default_open(default_open)
            .show(ui, |ui| {
                for (k, v) in map {
                    render_json_tree(ui, k, v, false);
                }
            });
        }
        Value::Array(arr) => {
            let header = format!("{}: [ {} items ]", key, arr.len());
            egui::CollapsingHeader::new(
                RichText::new(header)
                    .monospace()
                    .color(Color32::from_rgb(215, 165, 250)),
            )
            .default_open(default_open)
            .show(ui, |ui| {
                for (idx, item) in arr.iter().enumerate().take(100) {
                    render_json_tree(ui, &format!("[{idx}]"), item, false);
                }
            });
        }
        Value::String(s) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{key}:"))
                        .monospace()
                        .color(Palette::TEXT_MUTED),
                );
                ui.label(
                    RichText::new(format!("\"{s}\""))
                        .monospace()
                        .color(Color32::from_rgb(134, 239, 172)),
                );
            });
        }
        Value::Number(n) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{key}:"))
                        .monospace()
                        .color(Palette::TEXT_MUTED),
                );
                ui.label(
                    RichText::new(n.to_string())
                        .monospace()
                        .color(Color32::from_rgb(252, 211, 77)),
                );
            });
        }
        Value::Bool(b) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{key}:"))
                        .monospace()
                        .color(Palette::TEXT_MUTED),
                );
                ui.label(
                    RichText::new(b.to_string())
                        .monospace()
                        .color(Color32::from_rgb(100, 210, 235)),
                );
            });
        }
        Value::Null => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{key}: null"))
                        .monospace()
                        .color(Color32::from_rgb(135, 140, 150)),
                );
            });
        }
    }
}

fn render_structured_table(
    ui: &mut Ui,
    block_id: uuid::Uuid,
    headers: &[String],
    rows: &[Vec<String>],
    filter: &mut String,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("📊 {} rows", rows.len()))
                .size(11.5)
                .strong()
                .color(Color32::from_rgb(150, 200, 250)),
        );
        ui.label("Filter:");
        ui.add(
            egui::TextEdit::singleline(filter)
                .hint_text("Type to filter rows...")
                .desired_width(200.0),
        );
    });
    ui.add_space(4.0);

    let q = filter.trim().to_lowercase();
    egui::Grid::new(("mrt_table", block_id))
        .striped(true)
        .min_col_width(80.0)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for h in headers {
                ui.label(
                    RichText::new(h)
                        .strong()
                        .monospace()
                        .color(Color32::from_rgb(135, 205, 255)),
                );
            }
            ui.end_row();

            for row in rows {
                if !q.is_empty() && !row.iter().any(|c| c.to_lowercase().contains(&q)) {
                    continue;
                }
                for cell in row {
                    ui.label(
                        RichText::new(cell)
                            .monospace()
                            .color(Color32::from_rgb(220, 225, 235)),
                    );
                }
                ui.end_row();
            }
        });
}

fn render_http_exchange(ui: &mut Ui, http: &HttpExchangeSummary) {
    ui.horizontal_wrapped(|ui| {
        let status_color = if http.status_code < 300 {
            Color32::from_rgb(120, 235, 160)
        } else if http.status_code < 400 {
            Color32::from_rgb(245, 205, 115)
        } else {
            Color32::from_rgb(255, 130, 140)
        };

        ui.label(
            RichText::new(format!("{} {}", http.method, http.url))
                .monospace()
                .strong()
                .color(Color32::from_rgb(160, 210, 255)),
        );
        ui.label(
            RichText::new(format!(
                "• Status: {} {} • Latency: {} ms",
                http.status_code, http.status_text, http.duration_ms
            ))
            .strong()
            .color(status_color),
        );
    });

    egui::CollapsingHeader::new(format!(
        "Response Headers ({})",
        http.response_headers.len()
    ))
    .default_open(false)
    .show(ui, |ui| {
        for (k, v) in &http.response_headers {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{k}:"))
                        .monospace()
                        .strong()
                        .color(Color32::from_rgb(155, 195, 235)),
                );
                ui.label(RichText::new(v).monospace());
            });
        }
    });

    ui.add_space(4.0);
    if let Some(ref json_val) = http.body_json {
        render_json_tree(ui, "response_body", json_val, true);
    } else {
        ui.label(RichText::new(&http.body_raw).monospace());
    }
}

fn render_markdown_block(ui: &mut Ui, md: &str) {
    for line in md.lines() {
        let trimmed = line.trim_end();
        if let Some(h1) = trimmed.strip_prefix("# ") {
            ui.add_space(4.0);
            ui.label(
                RichText::new(h1)
                    .size(17.5)
                    .strong()
                    .color(Color32::from_rgb(140, 210, 255)),
            );
        } else if let Some(h2) = trimmed.strip_prefix("## ") {
            ui.add_space(3.0);
            ui.label(
                RichText::new(h2)
                    .size(15.0)
                    .strong()
                    .color(Color32::from_rgb(175, 220, 255)),
            );
        } else if let Some(h3) = trimmed.strip_prefix("### ") {
            ui.label(
                RichText::new(h3)
                    .size(13.5)
                    .strong()
                    .color(Color32::from_rgb(200, 225, 250)),
            );
        } else if let Some(bullet) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("  •").color(Color32::from_rgb(120, 195, 255)));
                ui.label(RichText::new(bullet).color(Color32::from_rgb(225, 230, 238)));
            });
        } else {
            ui.label(
                RichText::new(trimmed)
                    .monospace()
                    .color(Color32::from_rgb(210, 216, 224)),
            );
        }
    }
}


/// Returns true when the comparison is dismissed.
pub fn render_comparison(ui: &mut Ui, previous: &CommandBlock, current: &CommandBlock) -> bool {
    let mut close = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Output comparison");
            ui.monospace(&current.command);
            if ui.button("Close comparison").clicked() {
                close = true;
            }
        });
        ui.label(format!(
            "Previous {} · Current {}",
            previous.timestamp.format("%H:%M:%S"),
            current.timestamp.format("%H:%M:%S")
        ));

        let report = compare_blocks_structured(previous, current);

        match report.kind {
            StructuredDiffKind::Identical { summary } => {
                ui.label(RichText::new(summary).color(Palette::ACCENT_GREEN));
            }
            StructuredDiffKind::Json {
                added_paths,
                removed_paths,
                modified_paths,
                diff_lines,
            } => {
                ui.horizontal_wrapped(|ui| {
                    if !added_paths.is_empty() {
                        ui.label(
                            RichText::new(format!("+{} added keys", added_paths.len()))
                                .size(11.5)
                                .color(Palette::ACCENT_GREEN),
                        );
                    }
                    if !removed_paths.is_empty() {
                        ui.label(
                            RichText::new(format!("-{} removed keys", removed_paths.len()))
                                .size(11.5)
                                .color(Palette::ACCENT_RED),
                        );
                    }
                    if !modified_paths.is_empty() {
                        ui.label(
                            RichText::new(format!("~{} changed values", modified_paths.len()))
                                .size(11.5)
                                .color(Palette::ACCENT_BLUE),
                        );
                    }
                });
                egui::ScrollArea::horizontal()
                    .id_salt(("comparison_json", current.id))
                    .show(ui, |ui| {
                        for (line, tag) in diff_lines {
                            let (prefix, color) = match tag {
                                ChangeType::Remove => ("- ", Color32::from_rgb(255, 160, 165)),
                                ChangeType::Add => ("+ ", Color32::from_rgb(132, 224, 188)),
                                ChangeType::Equal => ("  ", Color32::from_rgb(175, 189, 202)),
                            };
                            ui.label(RichText::new(format!("{prefix}{line}")).monospace().color(color));
                        }
                    });
            }
            StructuredDiffKind::PortsDelta {
                added_listeners,
                removed_listeners,
                unchanged_count,
            } => {
                ui.label(
                    RichText::new(format!(
                        "Listening Ports Delta · {unchanged_count} unchanged"
                    ))
                    .size(11.5)
                    .italics()
                    .color(Palette::TEXT_SECONDARY),
                );
                for added in added_listeners {
                    ui.label(
                        RichText::new(format!("+ {added}"))
                            .monospace()
                            .color(Palette::ACCENT_GREEN),
                    );
                }
                for removed in removed_listeners {
                    ui.label(
                        RichText::new(format!("- {removed}"))
                            .monospace()
                            .color(Palette::ACCENT_RED),
                    );
                }
            }
            StructuredDiffKind::Text { diff_lines } => {
                egui::ScrollArea::horizontal()
                    .id_salt(("comparison", current.id))
                    .show(ui, |ui| {
                        for (line, tag) in diff_lines {
                            let (prefix, color) = match tag {
                                ChangeType::Remove => ("- ", Color32::from_rgb(255, 160, 165)),
                                ChangeType::Add => ("+ ", Color32::from_rgb(132, 224, 188)),
                                ChangeType::Equal => ("  ", Color32::from_rgb(175, 189, 202)),
                            };
                            ui.label(RichText::new(format!("{prefix}{line}")).monospace().color(color));
                        }
                    });
            }
        }
    });
    close
}
