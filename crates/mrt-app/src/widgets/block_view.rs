use egui::{Color32, RichText, Ui};
use mrt_core::{CommandBlock, HttpExchangeSummary, StructuredOutput, ViewMode};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum BlockAction {
    Compare(uuid::Uuid, uuid::Uuid),
    Rerun(String),
    DiagnoseError(uuid::Uuid),
    ApplySuggestedFix(String),
}

pub fn render_command_block(
    ui: &mut Ui,
    block: &mut CommandBlock,
    table_filter: &mut String,
    previous_id: Option<uuid::Uuid>,
) -> Option<BlockAction> {
    let mut triggered_action = None;

    let border_color = match block.exit_code {
        Some(0) => Color32::from_rgb(55, 95, 72),
        Some(_) => Color32::from_rgb(135, 58, 64),
        None => Color32::from_rgb(70, 80, 95),
    };

    let frame = egui::Frame::group(ui.style())
        .fill(Color32::from_rgb(22, 25, 32))
        .stroke(egui::Stroke::new(1.2_f32, border_color))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::same(10.0));

    frame.show(ui, |ui| {
        // Block Header Row
        ui.horizontal_wrapped(|ui| {
            // Shell Badge
            let shell_badge = egui::Frame::none()
                .fill(Color32::from_rgb(38, 52, 74))
                .rounding(egui::Rounding::same(4.0))
                .inner_margin(egui::Margin::symmetric(6.0, 2.0));
            shell_badge.show(ui, |ui| {
                ui.label(
                    RichText::new(&block.shell_name)
                        .size(11.5)
                        .strong()
                        .color(Color32::from_rgb(130, 195, 255)),
                );
            });

            // Command Text
            ui.label(
                RichText::new(format!("$ {}", block.command))
                    .monospace()
                    .strong()
                    .size(13.5)
                    .color(Color32::from_rgb(235, 240, 248)),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Exit code badge
                if let Some(code) = block.exit_code {
                    let (bg, fg, text) = if code == 0 {
                        (
                            Color32::from_rgb(32, 78, 50),
                            Color32::from_rgb(130, 235, 165),
                            format!("✓ 0 ({}ms)", block.duration_ms),
                        )
                    } else {
                        (
                            Color32::from_rgb(95, 34, 40),
                            Color32::from_rgb(255, 150, 155),
                            format!("✗ exit {} ({}ms)", code, block.duration_ms),
                        )
                    };
                    egui::Frame::none()
                        .fill(bg)
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).size(11.5).strong().color(fg));
                        });
                }

                if let Some(previous_id) = previous_id {
                    if ui.small_button("Compare with previous run").clicked() {
                        triggered_action = Some(BlockAction::Compare(previous_id, block.id));
                    }
                }
                // Action buttons
                if ui.small_button("↻ Rerun").clicked() {
                    triggered_action = Some(BlockAction::Rerun(block.command.clone()));
                }

                if ui.small_button("📋 Copy").clicked() {
                    ui.output_mut(|o| o.copied_text = block.plain_output.clone());
                }

                if block.structured.is_structured() {
                    let toggle_label = match block.view_mode {
                        ViewMode::Structured => format!("⇄ Raw ANSI ({})", block.structured.badge_label()),
                        ViewMode::RawAnsi => format!("⇄ {}", block.structured.badge_label()),
                    };
                    if ui.small_button(toggle_label).clicked() {
                        block.view_mode = match block.view_mode {
                            ViewMode::Structured => ViewMode::RawAnsi,
                            ViewMode::RawAnsi => ViewMode::Structured,
                        };
                    }
                }

                if matches!(block.exit_code, Some(c) if c != 0) && block.ai_explanation.is_none() {
                    if ui
                        .small_button(
                            RichText::new("✨ Diagnose Error")
                                .color(Color32::from_rgb(255, 200, 110)),
                        )
                        .clicked()
                    {
                        triggered_action = Some(BlockAction::DiagnoseError(block.id));
                    }
                }
            });
        });

        // Subtle metadata line (CWD & Timestamp)
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "📁 {}  •  🕒 {}",
                    block.cwd,
                    block.timestamp.format("%H:%M:%S")
                ))
                .size(11.0)
                .color(Color32::from_rgb(130, 138, 152)),
            );
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

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
            ui.add_space(6.0);
            egui::Frame::none()
                .fill(Color32::from_rgb(48, 38, 24))
                .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(210, 155, 65)))
                .rounding(egui::Rounding::same(5.0))
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("🤖 AI Error Triage:")
                                .strong()
                                .color(Color32::from_rgb(255, 210, 120)),
                        );
                        ui.label(RichText::new(explanation).color(Color32::from_rgb(235, 225, 210)));
                    });
                    if let Some(ref fix_cmd) = block.ai_suggested_fix {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Suggested Fix: `{fix_cmd}`"))
                                    .monospace()
                                    .color(Color32::from_rgb(150, 235, 175)),
                            );
                            if ui.small_button("▶ Run Fix").clicked() {
                                triggered_action =
                                    Some(BlockAction::ApplySuggestedFix(fix_cmd.clone()));
                            }
                        });
                    }
                });
        }
    });

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
            if ui.button("Close comparison").clicked() { close = true; }
        });
        ui.label(format!("Previous {} · Current {}",
            previous.timestamp.format("%H:%M:%S"), current.timestamp.format("%H:%M:%S")));
        if previous.plain_output == current.plain_output {
            ui.label("Outputs are identical.");
            return;
        }
        egui::ScrollArea::horizontal().id_salt(("comparison", current.id)).show(ui, |ui| {
            for change in similar::TextDiff::from_lines(&previous.plain_output, &current.plain_output).iter_all_changes() {
                let (prefix, color) = match change.tag() {
                    similar::ChangeTag::Delete => ("- ", Color32::from_rgb(255, 160, 165)),
                    similar::ChangeTag::Insert => ("+ ", Color32::from_rgb(132, 224, 188)),
                    similar::ChangeTag::Equal => ("  ", Color32::from_rgb(175, 189, 202)),
                };
                ui.label(RichText::new(format!("{prefix}{change}").trim_end_matches('\n')).monospace().color(color));
            }
        });
    });
    close
}
