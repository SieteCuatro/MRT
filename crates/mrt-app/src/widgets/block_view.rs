use crate::theme::{chip, ghost_button, icon_button, primary_button, secondary_button, Palette, Radius};
use crate::widgets::links::{render_spans_with_links, LinkRequest, SpanStyle};
use egui::{Color32, RichText, Ui};
use mrt_core::{AnsiSpan, ClipboardDecision, CommandBlock, HttpExchangeSummary, StructuredOutput, ViewMode};
use std::borrow::Cow;
use mrt_tools::{compare_blocks_structured, ChangeType, StructuredDiffKind};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum BlockAction {
    Compare(uuid::Uuid, uuid::Uuid),
    Rerun(String),
    DiagnoseError(uuid::Uuid),
    ApplySuggestedFix(String),
    CopyBugReport(uuid::Uuid),
    /// A PID cell was clicked in a table block (`:ports`, `:ps`): open it in the Task Manager.
    InspectProcess(u32),
    /// Stop a running block: kills the command's whole process tree, or ends a live `:top`.
    StopBlock(uuid::Uuid),
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
    // The block holding the current find match is scrolled into view first; when its
    // text is drawn as spans the exact match is scrolled to afterwards and wins.
    if let Some(h) = find_highlight(ui) {
        if h.scroll && h.current.is_some_and(|(id, _)| id == block.id) {
            ui.scroll_to_rect_animation(
                egui::Rect::from_min_size(ui.cursor().min, egui::vec2(1.0, 60.0)),
                Some(egui::Align::Center),
                egui::style::ScrollAnimation::none(),
            );
        }
    }

    egui::Frame::none()
        .fill(Palette::BG_SURFACE())
        .stroke(egui::Stroke::new(
            1.0_f32,
            if failed {
                Palette::FAILED_BORDER()
            } else {
                Palette::BORDER_FAINT()
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
                    if block.exit_code.is_none()
                        && ui
                            .add(ghost_button("■ Stop"))
                            .on_hover_text("Stop this command and every process it started")
                            .clicked()
                    {
                        triggered_action = Some(BlockAction::StopBlock(block.id));
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
                                        .color(Palette::ACCENT_TEXT()),
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
                    ui.label(RichText::new(meta).size(12.0).color(Palette::TEXT_MUTED()))
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
                        chip(ui, &badge, Color32::from_rgb(34, 38, 58), Palette::ACCENT_TEXT());
                    }

                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        match block.exit_code {
                            Some(0) => {
                                ui.label(RichText::new("✔").size(14.0).color(Palette::ACCENT_GREEN()))
                                    .on_hover_text("Succeeded");
                            }
                            Some(_) => {
                                ui.label(RichText::new("✖").size(14.0).color(Palette::ACCENT_RED()))
                                    .on_hover_text("Failed");
                            }
                            None => {
                                ui.add(egui::Spinner::new().size(14.0).color(Palette::ACCENT()));
                            }
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(&block.command)
                                    .monospace()
                                    .size(13.5)
                                    .color(Palette::TEXT_PRIMARY()),
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
                                .fill(Palette::BG_INSET())
                                .rounding(egui::Rounding::same(Radius::BUTTON))
                                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    render_json_tree(ui, "root", val, true);
                                });
                        }
                        (ViewMode::Structured, StructuredOutput::Table { headers, rows }) => {
                            if let Some(pid) = render_structured_table(ui, block.id, headers, rows, table_filter) {
                                triggered_action = Some(BlockAction::InspectProcess(pid));
                            }
                        }
                        (ViewMode::Structured, StructuredOutput::HttpResult(http)) => {
                            render_http_exchange(ui, http);
                        }
                        (ViewMode::Structured, StructuredOutput::Markdown(md)) => {
                            render_markdown_block(ui, md);
                        }
                        _ => {
                            if let Some(action) = render_ansi_spans(ui, block) {
                                triggered_action = Some(action);
                            }
                        }
                    }

                    render_clipboard_prompt(ui, block);

                    // Inline AI diagnosis
                    if let Some(ref explanation) = block.ai_explanation {
                        ui.add_space(10.0);
                        egui::Frame::none()
                            .fill(Palette::AI_FILL())
                            .stroke(egui::Stroke::new(1.0_f32, Palette::AI_BORDER()))
                            .rounding(egui::Rounding::same(12.0))
                            .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("✨").size(14.0).color(Palette::ACCENT()));
                                    ui.label(
                                        RichText::new("Diagnosis")
                                            .size(13.0)
                                            .strong()
                                            .color(Palette::ACCENT_TEXT()),
                                    );
                                });
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(explanation)
                                        .size(14.0)
                                        .color(Palette::TEXT_BODY()),
                                );
                                if let Some(ref fix_cmd) = block.ai_suggested_fix {
                                    ui.add_space(6.0);
                                    egui::Frame::none()
                                        .fill(Palette::BG_INSET())
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
                                                        .color(Palette::TEXT_PRIMARY()),
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

/// Terminal-style rendering for small Mosaic tiles: a prompt line with the
/// status, then the raw ANSI output. Failed commands keep a Diagnose action.
pub fn render_compact_block(ui: &mut Ui, block: &CommandBlock) -> Option<BlockAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        match block.exit_code {
            Some(0) => {
                ui.label(RichText::new("✔").size(12.0).color(Palette::ACCENT_GREEN()));
            }
            Some(_) => {
                ui.label(RichText::new("✖").size(12.0).color(Palette::ACCENT_RED()));
            }
            None => {
                ui.add(egui::Spinner::new().size(12.0).color(Palette::ACCENT()));
            }
        }
        ui.add(
            egui::Label::new(
                RichText::new(&block.command)
                    .monospace()
                    .size(12.5)
                    .color(Palette::TEXT_PRIMARY()),
            )
            .truncate(),
        )
        .on_hover_text(format!(
            "{}\n{} · {}",
            block.command,
            format_duration(block.duration_ms as u64),
            block.timestamp.format("%H:%M:%S")
        ));
    });
    if !block.plain_output.trim().is_empty() {
        let (_, spans) = tail_spans(&block.ansi_spans, block.plain_output.len(), COMPACT_LINES);
        if let Some(link_action) = render_spans_with_links(
            ui,
            &spans,
            &SpanStyle { font_size: 12.0, backgrounds: false },
            block_cwd(block),
            egui::Id::new(("link_error_compact", block.id)),
            None,
        ) {
            action = Some(link_request_action(link_action));
        }
    }
    let pending = block.pending_clipboard_requests();
    if pending > 0 {
        ui.label(
            RichText::new(format!("📋 {pending} clipboard request(s) — open the block outside Mosaic to review"))
                .size(11.0)
                .color(Palette::ACCENT_TEXT()),
        );
    }
    if let Some(code) = block.exit_code.filter(|c| *c != 0) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("exit {code}"))
                    .size(11.0)
                    .color(Color32::from_rgb(252, 165, 165)),
            );
            if block.ai_explanation.is_none()
                && ui
                    .add(
                        egui::Button::new(
                            RichText::new("✨ Diagnose")
                                .size(11.5)
                                .color(Palette::ACCENT_TEXT()),
                        )
                        .frame(false),
                    )
                    .clicked()
            {
                action = Some(BlockAction::DiagnoseError(block.id));
            }
        });
    }
    if let Some(explanation) = &block.ai_explanation {
        ui.label(
            RichText::new(format!("✨ {explanation}"))
                .size(12.0)
                .color(Palette::ACCENT_TEXT()),
        );
        if let Some(fix) = &block.ai_suggested_fix {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(format!("Run  {fix}"))
                            .monospace()
                            .size(11.5)
                            .color(Palette::TEXT_PRIMARY()),
                    )
                    .fill(Palette::BG_INSET()),
                )
                .clicked()
            {
                action = Some(BlockAction::ApplySuggestedFix(fix.clone()));
            }
        }
    }
    ui.add_space(8.0);
    action
}

/// Directory used to resolve relative file links: the cwd the command last
/// reported via OSC 7, else the directory it was started in.
fn block_cwd(block: &CommandBlock) -> &str {
    block.reported_cwd.as_deref().unwrap_or(&block.cwd)
}

fn link_request_action(request: LinkRequest) -> BlockAction {
    match request {
        LinkRequest::RunCommand(cmd) => BlockAction::Rerun(cmd),
    }
}

/// Lines of a block's output drawn at first; "Show more" adds this many again.
/// Laying out millions of lines every frame would freeze the window.
pub const RENDERED_LINES_STEP: usize = 2_000;

/// Lines drawn for a block in a Mosaic tile.
const COMPACT_LINES: usize = 200;

/// The last `lines` lines of `spans` (whose texts add up to `total_len` bytes),
/// and the byte length of the text before them. The search runs from the end,
/// so its cost depends on the lines kept, not on the size of the output.
pub fn tail_spans(spans: &[AnsiSpan], total_len: usize, lines: usize) -> (usize, Cow<'_, [AnsiSpan]>) {
    let mut newlines = 0;
    let mut after = 0; // bytes of the spans after the current one
    for (i, span) in spans.iter().enumerate().rev() {
        for (pos, _) in span.text.rmatch_indices('\n') {
            // A newline that ends the whole output does not start another line.
            if after == 0 && pos + 1 == span.text.len() {
                continue;
            }
            newlines += 1;
            if newlines == lines {
                let mut first = span.clone();
                first.text = span.text[pos + 1..].to_string();
                let mut tail = Vec::with_capacity(spans.len() - i);
                tail.push(first);
                tail.extend_from_slice(&spans[i + 1..]);
                let prefix = total_len - after - (span.text.len() - pos - 1);
                return (prefix, Cow::Owned(tail));
            }
        }
        after += span.text.len();
    }
    (0, Cow::Borrowed(spans))
}

fn format_bytes(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / (1u64 << 10) as f64),
        b => format!("{b} bytes"),
    }
}

fn render_ansi_spans(ui: &mut Ui, block: &CommandBlock) -> Option<BlockAction> {
    let highlight = find_highlight(ui);
    let shown_id = egui::Id::new(("block_shown_lines", block.id));
    let mut shown: usize = ui.ctx().data(|d| d.get_temp(shown_id)).unwrap_or(RENDERED_LINES_STEP);
    let mut current = highlight.as_ref().and_then(|h| h.current).filter(|(id, _)| *id == block.id).map(|(_, n)| n);
    let (mut prefix, mut spans) = tail_spans(&block.ansi_spans, block.plain_output.len(), shown);
    // The current find match may be in the hidden part: show enough lines to reach it.
    if let (Some(h), Some(n)) = (&highlight, current) {
        if prefix > 0 {
            let hidden = crate::find::match_ranges(&block.plain_output[..prefix], &h.query);
            if let Some(m) = hidden.get(n) {
                let extra = block.plain_output[m.start..prefix].matches('\n').count() + 1;
                shown += extra.div_ceil(RENDERED_LINES_STEP) * RENDERED_LINES_STEP;
                ui.ctx().data_mut(|d| d.insert_temp(shown_id, shown));
                (prefix, spans) = tail_spans(&block.ansi_spans, block.plain_output.len(), shown);
            }
            let before = crate::find::match_ranges(&block.plain_output[..prefix], &h.query).len();
            current = current.map(|n| n.saturating_sub(before));
        }
    }
    let find = highlight.as_ref().map(|h| crate::widgets::links::FindView {
        query: &h.query,
        current,
        scroll: h.scroll,
    });
    if block.truncated_bytes > 0 || prefix > 0 {
        ui.horizontal_wrapped(|ui| {
            if block.truncated_bytes > 0 {
                ui.label(
                    RichText::new(format!(
                        "{} of earlier output was not kept (limit: terminal.max_block_output_mb).",
                        format_bytes(block.truncated_bytes)
                    ))
                    .size(11.5)
                    .color(Palette::TEXT_MUTED()),
                );
            }
            if prefix > 0 {
                ui.label(
                    RichText::new(format!("Showing the last {shown} lines; {} above.", format_bytes(prefix as u64)))
                        .size(11.5)
                        .color(Palette::TEXT_MUTED()),
                );
                if ui.add(ghost_button(&format!("Show {RENDERED_LINES_STEP} more"))).clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(shown_id, shown + RENDERED_LINES_STEP));
                }
                if ui
                    .add(ghost_button("Show all"))
                    .on_hover_text("Laying out a very long output can take a while")
                    .clicked()
                {
                    ui.ctx().data_mut(|d| d.insert_temp(shown_id, usize::MAX));
                }
            }
        });
        ui.add_space(4.0);
    }
    let images: Vec<(usize, &mrt_core::InlineImage)> =
        block.images.iter().enumerate().filter(|(_, image)| image.anchor >= prefix).collect();
    if block.ansi_spans.is_empty() && images.is_empty() {
        ui.label(
            RichText::new("(no output)")
                .italics()
                .color(Palette::TEXT_MUTED()),
        );
        return None;
    }
    let style = SpanStyle { font_size: 13.0, backgrounds: true };
    if images.is_empty() {
        return render_spans_with_links(
            ui,
            &spans,
            &style,
            block_cwd(block),
            egui::Id::new(("link_error", block.id)),
            find.as_ref(),
        )
        .map(link_request_action);
    }

    // Text and images in output order: each text segment is its own galley.
    let cuts: Vec<usize> = images.iter().map(|(_, image)| image.anchor - prefix).collect();
    let segments = split_spans_at(&spans, &cuts);
    let mut action = None;
    let mut matches_before = 0;
    for (n, segment) in segments.iter().enumerate() {
        let text: String = segment.iter().map(|s| s.text.as_str()).collect();
        if !text.is_empty() {
            let count = find.as_ref().map(|f| crate::find::match_ranges(&text, f.query).len()).unwrap_or(0);
            let segment_find = find.as_ref().map(|f| crate::widgets::links::FindView {
                query: f.query,
                current: f.current.and_then(|c| c.checked_sub(matches_before)).filter(|c| *c < count),
                scroll: f.scroll,
            });
            matches_before += count;
            if let Some(request) = render_spans_with_links(
                ui,
                segment,
                &style,
                block_cwd(block),
                egui::Id::new(("link_error", block.id, n)),
                segment_find.as_ref(),
            ) {
                action = Some(link_request_action(request));
            }
        }
        if let Some((index, image)) = images.get(n) {
            render_inline_image(ui, block.id, *index, image);
        }
    }
    action
}

/// Splits `spans` at byte offsets `cuts` (sorted) of their joined text. Returns
/// `cuts.len() + 1` segments; a line break right before a cut is dropped, because
/// the image that follows starts on its own line anyway.
pub fn split_spans_at(spans: &[AnsiSpan], cuts: &[usize]) -> Vec<Vec<AnsiSpan>> {
    let mut segments: Vec<Vec<AnsiSpan>> = vec![Vec::new()];
    let mut offset = 0;
    let mut next_cut = cuts.iter().copied().peekable();
    for span in spans {
        let mut start = 0;
        let end = span.text.len();
        while let Some(&cut) = next_cut.peek() {
            if cut > offset + end {
                break;
            }
            let at = cut.saturating_sub(offset).max(start).min(end);
            if at > start {
                let mut piece = span.clone();
                piece.text = span.text[start..at].to_string();
                segments.last_mut().unwrap().push(piece);
            }
            start = at;
            segments.push(Vec::new());
            next_cut.next();
        }
        if start < end {
            let mut piece = span.clone();
            piece.text = span.text[start..].to_string();
            segments.last_mut().unwrap().push(piece);
        }
        offset += end;
    }
    // Cuts past the end of the text still get their (empty) segment.
    for _ in next_cut {
        segments.push(Vec::new());
    }
    let last = segments.len() - 1;
    for segment in &mut segments[..last] {
        if let Some(span) = segment.last_mut() {
            if span.text.ends_with('\n') {
                span.text.pop();
            }
        }
        segment.retain(|s| !s.text.is_empty());
    }
    segments
}

/// Display size of an inline image: the size the program asked for (cells,
/// pixels or percent), else its pixel size, fitted to the available width and at
/// most 600 points high, keeping the aspect ratio.
pub fn inline_image_size(
    image: &mrt_core::InlineImage,
    available_width: f32,
    cell: egui::Vec2,
    pixels_per_point: f32,
) -> egui::Vec2 {
    use mrt_core::ImageSize;
    let natural = egui::vec2(image.width as f32, image.height as f32) / pixels_per_point.max(0.5);
    let resolve = |size: Option<ImageSize>, per_cell: f32, full: f32| -> Option<f32> {
        match size? {
            ImageSize::Cells(n) => Some(n as f32 * per_cell),
            ImageSize::Pixels(px) => Some(px as f32 / pixels_per_point.max(0.5)),
            ImageSize::Percent(p) => Some(full * p as f32 / 100.0),
        }
    };
    let aspect = natural.y / natural.x.max(1.0);
    let mut size = match (
        resolve(image.requested_width, cell.x, available_width),
        resolve(image.requested_height, cell.y, 600.0),
    ) {
        (Some(w), Some(h)) => egui::vec2(w, h),
        (Some(w), None) => egui::vec2(w, w * aspect),
        (None, Some(h)) => egui::vec2(h / aspect.max(0.0001), h),
        (None, None) => natural,
    };
    let scale = (available_width / size.x.max(1.0)).min(600.0 / size.y.max(1.0)).min(1.0);
    size *= scale;
    size.max(egui::vec2(1.0, 1.0))
}

fn render_inline_image(ui: &mut Ui, block_id: uuid::Uuid, index: usize, image: &mrt_core::InlineImage) {
    let id = egui::Id::new(("block_image", block_id, index, image.anchor));
    let texture = match ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        Some(texture) => texture,
        None => {
            let pixels = egui::ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.rgba);
            let texture = ui.ctx().load_texture(format!("block-image-{block_id}-{index}"), pixels, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, texture.clone()));
            texture
        }
    };
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let cell = egui::vec2(font.size * 0.6, font.size * 1.35);
    let size = inline_image_size(image, ui.available_width(), cell, ui.ctx().pixels_per_point());
    ui.add(egui::Image::new((texture.id(), size)))
        .on_hover_text(format!("{}×{} image", image.width, image.height));
}

/// The find-in-output highlight for this frame, if the find bar is open.
fn find_highlight(ui: &Ui) -> Option<crate::find::Highlight> {
    ui.ctx().data(|d| d.get_temp::<crate::find::Highlight>(egui::Id::new(crate::find::HIGHLIGHT_ID)))
}

/// OSC 52: programs may ask to write the clipboard, but MRT only does it
/// after the user approves each request here.
fn render_clipboard_prompt(ui: &mut Ui, block: &mut CommandBlock) {
    let pending: Vec<usize> = block
        .clipboard_requests
        .iter()
        .enumerate()
        .filter(|(_, r)| r.decision == ClipboardDecision::Pending)
        .map(|(i, _)| i)
        .collect();
    let Some(&first) = pending.first() else { return };
    let request = &block.clipboard_requests[first];
    let chars = request.text.chars().count();
    let mut preview: String = request
        .text
        .chars()
        .take(160)
        .map(|c| match c {
            '\n' => '⏎',
            c if c.is_control() => '·',
            c => c,
        })
        .collect();
    if chars > 160 {
        preview.push('…');
    }

    let mut decision = None;
    let mut deny_all = false;
    ui.add_space(8.0);
    egui::Frame::none()
        .fill(Palette::BG_INSET())
        .stroke(egui::Stroke::new(1.0_f32, Palette::ACCENT()))
        .rounding(egui::Rounding::same(Radius::BUTTON))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(format!(
                    "📋 This command wants to copy {chars} character{} to your clipboard (OSC 52)",
                    if chars == 1 { "" } else { "s" }
                ))
                .size(12.5)
                .color(Palette::TEXT_PRIMARY()),
            );
            ui.label(RichText::new(preview).monospace().size(12.0).color(Palette::TEXT_BODY()));
            ui.horizontal(|ui| {
                if ui.add(primary_button("Allow")).on_hover_text("Write this text to the clipboard").clicked() {
                    decision = Some(ClipboardDecision::Approved);
                }
                if ui.add(secondary_button("Deny")).clicked() {
                    decision = Some(ClipboardDecision::Denied);
                }
                if pending.len() > 1 {
                    ui.label(
                        RichText::new(format!("+{} more", pending.len() - 1))
                            .size(11.5)
                            .color(Palette::TEXT_MUTED()),
                    );
                    if ui.add(ghost_button("Deny all")).clicked() {
                        deny_all = true;
                    }
                }
            });
        });

    if deny_all {
        for i in pending {
            block.clipboard_requests[i].decision = ClipboardDecision::Denied;
        }
    } else if let Some(decision) = decision {
        if decision == ClipboardDecision::Approved {
            let text = block.clipboard_requests[first].text.clone();
            ui.output_mut(|o| o.copied_text = text);
        }
        block.clipboard_requests[first].decision = decision;
    }
}

fn render_json_tree(ui: &mut Ui, key: &str, value: &Value, default_open: bool) {
    match value {
        Value::Object(map) => {
            let header = format!("{}: {{ {} keys }}", key, map.len());
            egui::CollapsingHeader::new(
                RichText::new(header)
                    .monospace()
                    .color(Palette::ACCENT_TEXT()),
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
                        .color(Palette::TEXT_MUTED()),
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
                        .color(Palette::TEXT_MUTED()),
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
                        .color(Palette::TEXT_MUTED()),
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

/// Renders a table block. Cells in a `PID` / `PPID` column are links; the clicked PID is
/// returned so the caller can open it in the Task Manager.
fn render_structured_table(
    ui: &mut Ui,
    block_id: uuid::Uuid,
    headers: &[String],
    rows: &[Vec<String>],
    filter: &mut String,
) -> Option<u32> {
    let mut clicked_pid = None;
    let pid_columns: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.eq_ignore_ascii_case("pid") || h.eq_ignore_ascii_case("ppid"))
        .map(|(i, _)| i)
        .collect();
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
                for (col, cell) in row.iter().enumerate() {
                    let pid = pid_columns
                        .contains(&col)
                        .then(|| cell.trim().parse::<u32>().ok())
                        .flatten()
                        .filter(|pid| *pid > 0);
                    if let Some(pid) = pid {
                        let link = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(cell).monospace().underline().color(Palette::ACCENT_TEXT()),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Open this process in the Task Manager")
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if link.clicked() {
                            clicked_pid = Some(pid);
                        }
                    } else {
                        ui.label(
                            RichText::new(cell)
                                .monospace()
                                .color(Color32::from_rgb(220, 225, 235)),
                        );
                    }
                }
                ui.end_row();
            }
        });
    clicked_pid
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
                ui.label(RichText::new(summary).color(Palette::ACCENT_GREEN()));
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
                                .color(Palette::ACCENT_GREEN()),
                        );
                    }
                    if !removed_paths.is_empty() {
                        ui.label(
                            RichText::new(format!("-{} removed keys", removed_paths.len()))
                                .size(11.5)
                                .color(Palette::ACCENT_RED()),
                        );
                    }
                    if !modified_paths.is_empty() {
                        ui.label(
                            RichText::new(format!("~{} changed values", modified_paths.len()))
                                .size(11.5)
                                .color(Palette::ACCENT_BLUE()),
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
                    .color(Palette::TEXT_SECONDARY()),
                );
                for added in added_listeners {
                    ui.label(
                        RichText::new(format!("+ {added}"))
                            .monospace()
                            .color(Palette::ACCENT_GREEN()),
                    );
                }
                for removed in removed_listeners {
                    ui.label(
                        RichText::new(format!("- {removed}"))
                            .monospace()
                            .color(Palette::ACCENT_RED()),
                    );
                }
            }
            StructuredDiffKind::ProcessesDelta {
                started,
                exited,
                memory_grown,
                unchanged_count,
            } => {
                ui.label(
                    RichText::new(format!("Process Snapshot Delta · {unchanged_count} unchanged"))
                        .size(11.5)
                        .italics()
                        .color(Palette::TEXT_SECONDARY()),
                );
                for line in started {
                    ui.label(RichText::new(format!("+ started  {line}")).monospace().color(Palette::ACCENT_GREEN()));
                }
                for line in exited {
                    ui.label(RichText::new(format!("- exited   {line}")).monospace().color(Palette::ACCENT_RED()));
                }
                for line in memory_grown {
                    ui.label(
                        RichText::new(format!("▲ memory   {line}"))
                            .monospace()
                            .color(Color32::from_rgb(253, 224, 138)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(parts: &[&str]) -> Vec<AnsiSpan> {
        parts.iter().map(|t| AnsiSpan { text: t.to_string(), ..AnsiSpan::default() }).collect()
    }

    fn text(spans: &[AnsiSpan]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn tail_spans_keeps_the_last_lines_across_spans() {
        let all = spans(&["a\nb", "\nc\n", "d\ne\n"]);
        let total = text(&all).len();
        let (prefix, tail) = tail_spans(&all, total, 2);
        assert_eq!(text(&tail), "d\ne\n");
        assert_eq!(prefix, "a\nb\nc\n".len());
        let (prefix, tail) = tail_spans(&all, total, 4);
        assert_eq!(text(&tail), "b\nc\nd\ne\n");
        assert_eq!(prefix, 2);
        let (prefix, tail) = tail_spans(&all, total, 10);
        assert_eq!(prefix, 0);
        assert_eq!(text(&tail), text(&all));
    }

    #[test]
    fn split_spans_at_cuts_inside_and_between_spans() {
        let all = spans(&["ab\n", "cd\nef"]);
        let parts = split_spans_at(&all, &[3, 5, 8]);
        let texts: Vec<String> = parts.iter().map(|p| text(p)).collect();
        assert_eq!(texts, vec!["ab", "cd", "\nef", ""]);
        let parts = split_spans_at(&all, &[0]);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].is_empty());
        assert_eq!(text(&parts[1]), "ab\ncd\nef");
    }

    #[test]
    fn inline_images_fit_the_width_and_honour_requests() {
        use mrt_core::{ImageSize, InlineImage};
        let image = |w, h, rw, rh| InlineImage {
            width: w,
            height: h,
            rgba: std::sync::Arc::new(Vec::new()),
            anchor: 0,
            requested_width: rw,
            requested_height: rh,
        };
        let cell = egui::vec2(8.0, 16.0);
        assert_eq!(inline_image_size(&image(200, 100, None, None), 1000.0, cell, 1.0), egui::vec2(200.0, 100.0));
        assert_eq!(inline_image_size(&image(200, 100, None, None), 1000.0, cell, 2.0), egui::vec2(100.0, 50.0));
        assert_eq!(inline_image_size(&image(2000, 1000, None, None), 500.0, cell, 1.0), egui::vec2(500.0, 250.0));
        assert_eq!(
            inline_image_size(&image(200, 100, Some(ImageSize::Cells(10)), None), 1000.0, cell, 1.0),
            egui::vec2(80.0, 40.0)
        );
        assert_eq!(
            inline_image_size(&image(200, 100, Some(ImageSize::Percent(50)), None), 1000.0, cell, 1.0),
            egui::vec2(500.0, 250.0)
        );
        assert_eq!(inline_image_size(&image(100, 4000, None, None), 1000.0, cell, 1.0).y, 600.0);
    }

    #[test]
    fn tail_spans_without_a_final_newline() {
        let all = spans(&["x\ny\nz"]);
        let (prefix, tail) = tail_spans(&all, 5, 1);
        assert_eq!(text(&tail), "z");
        assert_eq!(prefix, 4);
    }
}
