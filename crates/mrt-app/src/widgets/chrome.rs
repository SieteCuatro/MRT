//! App chrome primitives for the "Graphite Calm" layout: sidebar rows, the
//! command-palette search pill, the brand mark and the centered stream column.

use crate::theme::{Palette, Radius, STREAM_MAX_WIDTH};
use crate::widgets::activity::{activity_color, activity_short_label, paint_activity_indicator};
use mrt_core::ActivitySnapshot;
use egui::{Align2, Color32, FontId, Rect, Response, Rounding, Sense, Stroke, Ui};

/// Accent square holding a prompt glyph.
pub fn brand_mark(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(Radius::ICON), Palette::ACCENT());
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "›_",
        FontId::monospace(size * 0.5),
        Palette::ON_ACCENT(),
    );
}

fn row_background(ui: &Ui, rect: Rect, response: &Response, selected: bool) {
    let fill = if selected {
        Palette::BG_SELECTED()
    } else if response.hovered() {
        Color32::from_rgb(28, 28, 32)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(rect, Rounding::same(Radius::BUTTON), fill);
}

/// A sidebar navigation row: icon, label and optional trailing hint.
/// In `compact` mode only the icon is drawn and the label becomes hover text.
pub fn nav_row(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    trailing: Option<&str>,
    selected: bool,
    compact: bool,
) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 36.0), Sense::click());
    row_background(ui, rect, &response, selected);
    let text_color = if selected {
        Palette::TEXT_PRIMARY()
    } else {
        Palette::TEXT_BODY()
    };
    let painter = ui.painter();
    if compact {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(16.0),
            text_color,
        );
        return response.on_hover_text(label);
    }
    painter.text(
        egui::pos2(rect.left() + 22.0, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(15.0),
        Palette::TEXT_SECONDARY(),
    );
    painter.text(
        egui::pos2(rect.left() + 42.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.0),
        text_color,
    );
    if let Some(trailing) = trailing {
        painter.text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            Align2::RIGHT_CENTER,
            trailing,
            FontId::proportional(12.0),
            Palette::TEXT_MUTED(),
        );
    }
    response
}

/// Outcome of interacting with a tab row.
pub struct TabRowResponse {
    pub selected: bool,
    pub close: bool,
    /// The row started being dragged (to reorder the tabs).
    pub drag_started: bool,
    pub rect: Rect,
}

/// A two-line sidebar row for a terminal tab with a status dot and a close
/// affordance that appears on hover.
pub fn tab_row(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    status: &ActivitySnapshot,
    active: bool,
    closable: bool,
    compact: bool,
) -> TabRowResponse {
    let width = ui.available_width();
    let height = if compact { 40.0 } else { 46.0 };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click_and_drag());
    let drag_started = response.drag_started();
    row_background(ui, rect, &response, active);
    let time = ui.input(|i| i.time);
    let painter = ui.painter();

    if compact {
        let initial: String = title.chars().next().unwrap_or('?').to_uppercase().collect();
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            initial,
            FontId::proportional(14.0),
            if active {
                Palette::TEXT_PRIMARY()
            } else {
                Palette::TEXT_SECONDARY()
            },
        );
        paint_activity_indicator(
            painter,
            egui::pos2(rect.right() - 10.0, rect.top() + 10.0),
            3.5,
            status,
            time,
        );
        let response = response.on_hover_text(format!("{title} · {subtitle}
{}", status.tooltip()));
        return TabRowResponse {
            selected: response.clicked(),
            close: false,
            drag_started,
            rect,
        };
    }

    paint_activity_indicator(
        painter,
        egui::pos2(rect.left() + 16.0, rect.center().y),
        4.5,
        status,
        time,
    );
    let text_left = rect.left() + 32.0;
    let max_text = (rect.width() - 72.0).max(40.0);
    let title_galley = painter.layout(
        title.to_owned(),
        FontId::proportional(14.0),
        if active {
            Palette::TEXT_PRIMARY()
        } else {
            Palette::TEXT_BODY()
        },
        max_text,
    );
    painter.galley(
        egui::pos2(text_left, rect.top() + 6.0),
        title_galley,
        Palette::TEXT_PRIMARY(),
    );
    let subtitle_galley = painter.layout(
        subtitle.to_owned(),
        FontId::proportional(12.0),
        if active {
            Palette::TEXT_SECONDARY()
        } else {
            Palette::TEXT_MUTED()
        },
        max_text,
    );
    painter.galley(
        egui::pos2(text_left, rect.top() + 25.0),
        subtitle_galley,
        Palette::TEXT_MUTED(),
    );

    let close_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 18.0, rect.center().y),
        egui::vec2(24.0, 24.0),
    );
    let mut close = false;
    if !(closable && response.hovered()) {
        if let Some(label) = activity_short_label(status) {
            painter.text(
                egui::pos2(rect.right() - 10.0, rect.center().y),
                Align2::RIGHT_CENTER,
                label,
                FontId::proportional(11.0),
                activity_color(status),
            );
        }
    }
    if closable && response.hovered() {
        let pointer_on_close = ui
            .ctx()
            .pointer_hover_pos()
            .map(|p| close_rect.contains(p))
            .unwrap_or(false);
        if pointer_on_close {
            painter.rect_filled(close_rect, Rounding::same(6.0), Palette::BG_OVERLAY());
        }
        painter.text(
            close_rect.center(),
            Align2::CENTER_CENTER,
            "×",
            FontId::proportional(16.0),
            Palette::TEXT_SECONDARY(),
        );
        if response.clicked() && pointer_on_close {
            close = true;
        }
    }
    let response = response.on_hover_text(status.tooltip());
    TabRowResponse {
        selected: response.clicked() && !close,
        close,
        drag_started,
        rect,
    }
}

/// Search-field look-alike button that opens the command palette.
pub fn search_pill(ui: &mut Ui, width: f32, placeholder: &str, shortcut: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), Sense::click());
    let painter = ui.painter();
    painter.rect(
        rect,
        Rounding::same(Radius::BUTTON),
        if response.hovered() {
            Palette::BG_ELEVATED()
        } else {
            Palette::BG_PANEL()
        },
        Stroke::new(1.0_f32, Palette::BORDER_SUBTLE()),
    );
    painter.text(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        Align2::CENTER_CENTER,
        "🔍",
        FontId::proportional(12.0),
        Palette::TEXT_MUTED(),
    );
    painter.text(
        egui::pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        placeholder,
        FontId::proportional(13.0),
        Palette::TEXT_MUTED(),
    );
    let kbd = painter.layout_no_wrap(
        shortcut.to_owned(),
        FontId::proportional(11.0),
        Palette::TEXT_SECONDARY(),
    );
    let kbd_rect = Rect::from_min_size(
        egui::pos2(
            rect.right() - kbd.size().x - 20.0,
            rect.center().y - kbd.size().y / 2.0 - 2.0,
        ),
        kbd.size() + egui::vec2(12.0, 4.0),
    );
    painter.rect_filled(kbd_rect, Rounding::same(6.0), Palette::BG_OVERLAY());
    painter.galley(
        kbd_rect.min + egui::vec2(6.0, 2.0),
        kbd,
        Palette::TEXT_SECONDARY(),
    );
    response.on_hover_text("Open the command palette")
}

/// A command-palette result row: icon, title and a right-aligned monospace hint.
pub fn palette_row(ui: &mut Ui, icon: &str, title: &str, hint: &str) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 40.0), Sense::click());
    let highlighted = response.hovered() || response.has_focus();
    if highlighted {
        ui.painter().rect_filled(
            rect,
            Rounding::same(Radius::BUTTON),
            Color32::from_rgb(42, 44, 58),
        );
    }
    let painter = ui.painter();
    painter.text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(14.0),
        if highlighted {
            Palette::ACCENT_TEXT()
        } else {
            Palette::TEXT_SECONDARY()
        },
    );
    let hint_galley =
        painter.layout_no_wrap(hint.to_owned(), FontId::monospace(12.0), Palette::TEXT_MUTED());
    let hint_width = hint_galley.size().x;
    painter.galley(
        egui::pos2(
            rect.right() - 12.0 - hint_width,
            rect.center().y - hint_galley.size().y / 2.0,
        ),
        hint_galley,
        Palette::TEXT_MUTED(),
    );
    let title_galley = painter.layout(
        title.to_owned(),
        FontId::proportional(14.0),
        if highlighted {
            Palette::TEXT_PRIMARY()
        } else {
            Palette::TEXT_BODY()
        },
        (rect.width() - 80.0 - hint_width).max(60.0),
    );
    painter.galley(
        egui::pos2(rect.left() + 40.0, rect.center().y - title_galley.size().y / 2.0),
        title_galley,
        Palette::TEXT_PRIMARY(),
    );
    response
}

/// Lays `add_contents` out in a column no wider than [`STREAM_MAX_WIDTH`],
/// centered in the available width.
pub fn centered_column<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let available = ui.available_width();
    let width = available.min(STREAM_MAX_WIDTH);
    let pad = ((available - width) / 2.0).max(0.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.x = 8.0;
            add_contents(ui)
        })
        .inner
    })
    .inner
}
