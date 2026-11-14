use crate::theme::Palette;
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

pub fn render_command_block(
    ui: &mut Ui,
    block: &mut CommandBlock,
    table_filter: &mut String,
    previous_id: Option<uuid::Uuid>,
) -> Option<BlockAction> {
    let mut triggered_action = None;

    let accent_color = match block.exit_code {
        Some(0) => Palette::ACCENT_GREEN,
        Some(_) => Palette::ACCENT_RED,
        None => Palette::ACCENT_BLUE,
    };

    let frame = egui::Frame::none()
        .fill(Palette::BG_SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(14.0, 12.0));

    let resp = frame.show(ui, |ui| {
        // Block Header Row
        ui.horizontal_wrapped(|ui| {
            // Shell Badge
            egui::Frame::none()
                .fill(Palette::BG_OVERLAY)
                .stroke(egui::Stroke::new(1.0_f32, Palette::BORDER_SUBTLE))
                .rounding(egui::Rounding::same(4.0))
                .inner_margin(egui::Margin::symmetric(7.0, 2.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(&block.shell_name)
                            .size(11.0)
                            .monospace()
                            .strong()
                            .color(Palette::ACCENT_BLUE),
                    );
                });

            // Command Text
            ui.label(
                RichText::new(&block.command)
                    .monospace()
                    .strong()
                    .size(13.5)
                    .color(Palette::TEXT_PRIMARY),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Action buttons (right-aligned for clean scanning)
                if matches!(block.exit_code, Some(c) if c != 0) && block.ai_explanation.is_none() {
                    if ui
                        .small_button(
                            RichText::new("Diagnose")
                                .size(11.5)
                                .color(Palette::ACCENT_AMBER),
                        )
                        .clicked()
                    {
                        triggered_action = Some(BlockAction::DiagnoseError(block.id));
                    }
                }

                if block.structured.is_structured() {
                    let toggle_label = match block.view_mode {
                        ViewMode::Structured => "Raw",
                        ViewMode::RawAnsi => block.structured.badge_label(),
                    };
                    if ui
                        .small_button(RichText::new(toggle_label).size(11.5))
                        .clicked()
                    {
                        block.view_mode = match block.view_mode {
                            ViewMode::Structured => ViewMode::RawAnsi,
                            ViewMode::RawAnsi => ViewMode::Structured,
                        };
                    }
                }

                if ui.small_button(RichText::new("Copy").size(11.5)).clicked() {
                    ui.output_mut(|o| o.copied_text = block.plain_output.clone());
                }

                if ui.small_button(RichText::new("Rerun").size(11.5)).clicked() {
                    triggered_action = Some(BlockAction::Rerun(block.command.clone()));
                }

                if let Some(previous_id) = previous_id.filter(|_| block.exit_code.is_some()) {
                    if ui.small_button(RichText::new("Diff").size(11.5)).clicked() {
                        triggered_action = Some(BlockAction::Compare(previous_id, block.id));
                    }
                }

                if block.exit_code.map(|c| c != 0).unwrap_or(false) {
                    if ui.small_button(RichText::new("Report").size(11.5)).on_hover_text("Generate redacted GitHub markdown bug report").clicked() {
                        triggered_action = Some(BlockAction::CopyBugReport(block.id));
                    }
                }

                // Exit code pill
                if let Some(code) = block.exit_code {
                    let (bg, fg, text) = if code == 0 {
                        (
                            Color32::from_rgba_unmultiplied(63, 185, 80, 28),
                            Palette::ACCENT_GREEN,
                            format!("0 · {}ms", block.duration_ms),
                        )
                    } else {
                        (
                            Color32::from_rgba_unmultiplied(248, 81, 73, 28),
                            Palette::ACCENT_RED,
                            format!("exit {code} · {}ms", block.duration_ms),
                        )
                    };
                    egui::Frame::none()
                        .fill(bg)
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::symmetric(7.0, 2.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).size(11.0).monospace().strong().color(fg));
                        });
                } else {
                    ui.spinner();
                    ui.label(RichText::new("running").size(11.0).color(Palette::ACCENT_BLUE));
                }
            });
        });

        // Subtle metadata line (CWD & Timestamp)
        ui.label(
            RichText::new(format!(
                "{}  ·  {}",
                block.cwd,
                block.timestamp.format("%H:%M:%S")
            ))
            .size(11.0)
            .monospace()
            .color(Palette::TEXT_MUTED),
        );

        ui.add_space(6.0);

        // Body Content
        match (&block.view_mode, &block.structured) {
            (ViewMode::Structured, StructuredOutput::Json(val)) => {
                ui.label(
                    RichText::new("Interactive JSON Tree (click nodes to expand/collapse)")
                        .size(11.0)
                        .italics()
                        .color(Color32::from_rgb(140, 180, 220)),
                );
                render_json_tree(ui, "root", val, true);
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

        // AI Error Triage Box if present
        if let Some(ref explanation) = block.ai_explanation {
            ui.add_space(8.0);
            egui::Frame::none()
                .fill(Color32::from_rgba_unmultiplied(210, 153, 34, 20))
                .stroke(egui::Stroke::new(1.0_f32, Palette::ACCENT_AMBER))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("AI Triage")
                                .size(11.5)
                                .strong()
                                .color(Palette::ACCENT_AMBER),
                        );
                        ui.label(
                            RichText::new(explanation)
                                .size(12.0)
                                .color(Palette::TEXT_PRIMARY),
                        );
                    });
                    if let Some(ref fix_cmd) = block.ai_suggested_fix {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(fix_cmd)
                                    .monospace()
                                    .size(12.0)
                                    .color(Palette::ACCENT_GREEN),
                            );
                            if ui.small_button("Run Fix").clicked() {
                                triggered_action =
                                    Some(BlockAction::ApplySuggestedFix(fix_cmd.clone()));
                            }
                        });
                    }
                });
        }
    });

    // Crisp 3px left status accent bar
    let rect = resp.response.rect;
    let bar_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x, rect.min.y + 4.0),
        egui::pos2(rect.min.x + 3.0, rect.max.y - 4.0),
    );
    ui.painter().rect_filled(bar_rect, 2.0, accent_color);

    triggered_action
}

fn render_ansi_spans(ui: &mut Ui, block: &CommandBlock) {
    if block.ansi_spans.is_empty() {
        ui.label(
            RichText::new("(no output)")
                .italics()
                .color(Color32::from_rgb(120, 125, 135)),
        );
        return;
    }

    let mut job = egui::text::LayoutJob::default();
    for span in &block.ansi_spans {
        let fg = span
            .fg
            .map(|c| Color32::from_rgb(c.r, c.g, c.b))
            .unwrap_or(Color32::from_rgb(212, 218, 228));
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
                    .color(Color32::from_rgb(130, 195, 255)),
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
                        .color(Color32::from_rgb(170, 180, 195)),
                );
                ui.label(
                    RichText::new(format!("\"{s}\""))
                        .monospace()
                        .color(Color32::from_rgb(152, 215, 135)),
                );
            });
        }
        Value::Number(n) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{key}:"))
                        .monospace()
                        .color(Color32::from_rgb(170, 180, 195)),
                );
                ui.label(
                    RichText::new(n.to_string())
                        .monospace()
                        .color(Color32::from_rgb(240, 185, 110)),
                );
            });
        }
        Value::Bool(b) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("{key}:"))
                        .monospace()
                        .color(Color32::from_rgb(170, 180, 195)),
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
