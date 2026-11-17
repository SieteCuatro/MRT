//! Tab activity indicator: a small dot, spinner or progress ring drawn on sidebar tab rows
//! and Mosaic tiles. The state itself comes from `mrt_core::ActivityTracker`.

use crate::theme::Palette;
use egui::{Color32, Painter, Pos2, Response, Sense, Shape, Stroke, Ui};
use mrt_core::{ActivitySnapshot, ActivityState, ProgressTone};
use std::f32::consts::TAU;

const IDLE: Color32 = Color32::from_rgb(63, 63, 70);

/// Main colour of the indicator.
pub fn activity_color(snapshot: &ActivitySnapshot) -> Color32 {
    match snapshot.tone {
        ProgressTone::Error if snapshot.state.is_running() => return Palette::ACCENT_RED,
        ProgressTone::Warning if snapshot.state.is_running() => return Palette::ACCENT_AMBER,
        _ => {}
    }
    match snapshot.state {
        ActivityState::Idle => IDLE,
        ActivityState::Running | ActivityState::Progress(_) => Palette::ACCENT,
        ActivityState::WaitingForInput => Palette::ACCENT_AMBER,
        ActivityState::Succeeded => Palette::ACCENT_GREEN,
        ActivityState::Failed => Palette::ACCENT_RED,
    }
}

/// True when the indicator animates and the UI should keep repainting.
pub fn is_animated(snapshot: &ActivitySnapshot) -> bool {
    matches!(snapshot.state, ActivityState::Running | ActivityState::WaitingForInput)
}

fn arc_points(center: Pos2, radius: f32, start: f32, sweep: f32) -> Vec<Pos2> {
    let steps = ((sweep.abs() / TAU) * 32.0).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = start + sweep * i as f32 / steps as f32;
            center + radius * egui::vec2(a.cos(), a.sin())
        })
        .collect()
}

/// Paints the indicator centred on `center`. `time` is `ui.input(|i| i.time)`.
pub fn paint_activity_indicator(painter: &Painter, center: Pos2, radius: f32, snapshot: &ActivitySnapshot, time: f64) {
    let color = activity_color(snapshot);
    match snapshot.state {
        ActivityState::Running => {
            painter.circle_stroke(center, radius, Stroke::new(1.5_f32, color.gamma_multiply(0.25)));
            let start = (time as f32 * 5.0) % TAU;
            painter.add(Shape::line(arc_points(center, radius, start, TAU * 0.3), Stroke::new(1.8_f32, color)));
        }
        ActivityState::Progress(p) => {
            painter.circle_stroke(center, radius, Stroke::new(1.5_f32, color.gamma_multiply(0.25)));
            let sweep = TAU * (p as f32 / 100.0);
            if sweep > 0.0 {
                painter.add(Shape::line(arc_points(center, radius, -TAU / 4.0, sweep), Stroke::new(1.8_f32, color)));
            }
        }
        ActivityState::WaitingForInput => {
            let pulse = 0.55 + 0.45 * ((time as f32 * 3.0).sin() * 0.5 + 0.5);
            painter.circle_filled(center, radius + 1.5, color.gamma_multiply(0.25 * pulse));
            painter.circle_filled(center, radius * 0.85, color.gamma_multiply(pulse));
        }
        _ => {
            painter.circle_filled(center, radius * 0.9, color);
        }
    }
}

/// Short text drawn next to the indicator where there is room (`42%`, `input`).
pub fn activity_short_label(snapshot: &ActivitySnapshot) -> Option<String> {
    match snapshot.state {
        ActivityState::Progress(p) => Some(format!("{p}%")),
        ActivityState::WaitingForInput => Some("input".into()),
        _ => None,
    }
}

/// Allocates a small square, paints the indicator and attaches the tooltip.
pub fn activity_indicator(ui: &mut Ui, snapshot: &ActivitySnapshot) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
    let time = ui.input(|i| i.time);
    paint_activity_indicator(ui.painter(), rect.center(), 4.5, snapshot, time);
    if let Some(label) = activity_short_label(snapshot) {
        ui.label(egui::RichText::new(label).size(11.0).color(activity_color(snapshot)));
    }
    response.on_hover_text(snapshot.tooltip())
}
