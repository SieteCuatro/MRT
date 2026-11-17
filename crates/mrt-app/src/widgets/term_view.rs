//! Paints a `mrt_core::TermSnapshot` (the terminal grid emulator's screen) cell by
//! cell: colors, bold, dim, italic, underline, strikeout, inverse, wide characters,
//! and the cursor.
//!
//! Each row is drawn as runs of cells that share a style, so a typical screen is a
//! few dozen text shapes rather than one per cell. Wide characters (CJK, emoji) are
//! drawn on their own so a fallback font with a different advance cannot shift the
//! rest of the row off the grid.

use crate::theme::Palette;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};
use mrt_core::{xterm_palette, AnsiColor, TermCell, TermColor, TermCursorShape, TermSnapshot};

/// Default foreground of the terminal grid.
#[allow(non_snake_case)]
pub fn TERM_FG() -> Color32 {
    Palette::TEXT_BODY()
}
/// Default background of the terminal grid.
#[allow(non_snake_case)]
pub fn TERM_BG() -> Color32 {
    Palette::BG_INSET()
}

/// Size of one cell for `font`: the advance of `M` as egui actually lays out text, and
/// the font's row height.
///
/// `glyph_width` is not enough: text layout rounds glyph positions, so a run of N
/// characters can be wider than N × `glyph_width`, which pushed the last character of
/// each row past the grid. Measuring a long run gives the real per-cell advance.
pub fn cell_size(ui: &Ui, font: &FontId) -> Vec2 {
    const SAMPLE: usize = 100;
    ui.fonts(|fonts| {
        let run = fonts.layout_no_wrap("M".repeat(SAMPLE), font.clone(), Color32::WHITE);
        let width = run.size().x / SAMPLE as f32;
        let height = fonts.row_height(font);
        Vec2::new(width.max(1.0), height.max(1.0))
    })
}

/// How many columns and rows of `cell` fit in `available` (at least 2x1, at most 1000x500).
pub fn grid_dims(available: Vec2, cell: Vec2) -> (u16, u16) {
    let cols = (available.x / cell.x.max(1.0)).floor().clamp(2.0, 1000.0) as u16;
    let rows = (available.y / cell.y.max(1.0)).floor().clamp(1.0, 500.0) as u16;
    (cols, rows)
}

/// Draws `snapshot` at the cursor of `ui`. `focused` draws a filled cursor; otherwise
/// the cursor is an outline, like other terminals show an unfocused window.
///
/// `id` must be the same every frame for this terminal: egui hit-tests clicks against
/// the widget rectangles of the previous frame by id, so an automatic id that shifts
/// with the surrounding layout would make clicks miss the terminal.
pub fn paint_terminal(ui: &mut Ui, id: egui::Id, snapshot: &TermSnapshot, font: &FontId, cell: Vec2, focused: bool) -> Response {
    let size = Vec2::new(cell.x * snapshot.cols as f32, cell.y * snapshot.rows as f32);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, TERM_BG());

    for (row_index, row) in snapshot.cells.iter().enumerate() {
        let top = rect.top() + row_index as f32 * cell.y;
        paint_row_backgrounds(&painter, row, rect.left(), top, cell);
        paint_row_text(ui, &painter, row, rect.left(), top, cell, font);
    }

    let cursor = snapshot.cursor;
    if cursor.visible && cursor.row < snapshot.rows && cursor.col < snapshot.cols {
        let cell_at = snapshot.cell(cursor.row, cursor.col);
        let width = if cell_at.is_some_and(|c| c.wide) { 2.0 } else { 1.0 };
        let min = Pos2::new(rect.left() + cursor.col as f32 * cell.x, rect.top() + cursor.row as f32 * cell.y);
        let cursor_rect = Rect::from_min_size(min, Vec2::new(cell.x * width, cell.y));
        let color = Palette::ACCENT_TEXT();
        if !focused {
            painter.rect_stroke(cursor_rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, color));
        } else {
            match cursor.shape {
                TermCursorShape::Block => {
                    painter.rect_filled(cursor_rect, 0.0, color);
                    if let Some(c) = cell_at.filter(|c| c.ch != ' ' && !c.wide_spacer) {
                        let text: String = std::iter::once(c.ch).chain(c.zero_width.iter().copied()).collect();
                        painter.text(min, egui::Align2::LEFT_TOP, text, font.clone(), TERM_BG());
                    }
                }
                TermCursorShape::Underline => {
                    let y = cursor_rect.bottom() - 1.0;
                    painter.line_segment([Pos2::new(cursor_rect.left(), y), Pos2::new(cursor_rect.right(), y)], Stroke::new(2.0_f32, color));
                }
                TermCursorShape::Beam => {
                    let x = cursor_rect.left() + 1.0;
                    painter.line_segment([Pos2::new(x, cursor_rect.top()), Pos2::new(x, cursor_rect.bottom())], Stroke::new(2.0_f32, color));
                }
            }
        }
    }
    response
}

/// A text selection on the visible grid, from where the drag started (`anchor`) to
/// where the pointer is (`head`), both `(col, row)`. Like other terminals it runs in
/// reading order: from the earlier cell to the end of its row, whole rows between,
/// and the start of the last row up to the later cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    /// Start and end cells in reading order (both inclusive).
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let key = |(c, r): (u16, u16)| (r, c);
        if key(self.anchor) <= key(self.head) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The column range selected on `row` (inclusive), if any, for a grid `cols` wide.
    pub fn columns_on(&self, row: u16, cols: u16) -> Option<(u16, u16)> {
        let ((sc, sr), (ec, er)) = self.ordered();
        if row < sr || row > er || cols == 0 {
            return None;
        }
        let from = if row == sr { sc } else { 0 };
        let to = if row == er { ec } else { cols - 1 };
        Some((from, to.min(cols - 1)))
    }
}

/// The selected text: each row's selected cells with trailing spaces removed,
/// rows joined with `\n`. Right halves of wide characters are skipped.
pub fn selection_text(snapshot: &TermSnapshot, selection: &Selection) -> String {
    let mut lines = Vec::new();
    for (row_index, row) in snapshot.cells.iter().enumerate() {
        let Some((from, to)) = selection.columns_on(row_index as u16, snapshot.cols) else {
            continue;
        };
        let mut line = String::new();
        for cell in row.iter().skip(from as usize).take((to - from) as usize + 1) {
            if cell.wide_spacer {
                continue;
            }
            line.push(cell.ch);
            line.extend(cell.zero_width.iter());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

/// Shades the selected cells of a grid drawn at `rect` with cells of `cell` size.
pub fn paint_selection(ui: &Ui, rect: Rect, cell: Vec2, cols: u16, rows: u16, selection: &Selection) {
    let painter = ui.painter_at(rect);
    let shade = Palette::ACCENT().gamma_multiply(0.35);
    for row in 0..rows {
        if let Some((from, to)) = selection.columns_on(row, cols) {
            let min = Pos2::new(rect.left() + from as f32 * cell.x, rect.top() + row as f32 * cell.y);
            let size = Vec2::new((to - from + 1) as f32 * cell.x, cell.y);
            painter.rect_filled(Rect::from_min_size(min, size), 0.0, shade);
        }
    }
}

/// Resolved colors and text attributes of one cell.
#[derive(Clone, Copy, PartialEq)]
struct CellStyle {
    fg: Color32,
    bg: Color32,
    italic: bool,
    underline: bool,
    strikeout: bool,
}

fn to_color32(c: AnsiColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

fn resolve(color: TermColor, default: Color32, bold: bool) -> Color32 {
    match color {
        TermColor::DefaultFg | TermColor::DefaultBg => default,
        // Bold text in one of the 8 basic colors uses its bright variant, as xterm does.
        TermColor::Indexed(i) if bold && i < 8 => to_color32(xterm_palette(i + 8)),
        TermColor::Indexed(i) => to_color32(xterm_palette(i)),
        TermColor::Rgb(rgb) => to_color32(rgb),
    }
}

fn cell_style(cell: &TermCell) -> CellStyle {
    let fg_default = if cell.bold { Palette::TEXT_PRIMARY() } else { TERM_FG() };
    let mut fg = resolve(cell.fg, fg_default, cell.bold);
    let mut bg = resolve(cell.bg, TERM_BG(), false);
    if cell.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.dim {
        fg = fg.gamma_multiply(0.6);
    }
    if cell.hidden {
        fg = bg;
    }
    CellStyle {
        fg,
        bg,
        italic: cell.italic,
        underline: cell.underline,
        strikeout: cell.strikeout,
    }
}

/// Fills non-default backgrounds, merging neighbouring cells of the same color.
fn paint_row_backgrounds(painter: &egui::Painter, row: &[TermCell], left: f32, top: f32, cell: Vec2) {
    let mut run: Option<(usize, usize, Color32)> = None; // (start col, end col exclusive, color)
    let flush = |run: Option<(usize, usize, Color32)>| {
        if let Some((start, end, color)) = run {
            let min = Pos2::new(left + start as f32 * cell.x, top);
            painter.rect_filled(Rect::from_min_size(min, Vec2::new((end - start) as f32 * cell.x, cell.y)), 0.0, color);
        }
    };
    for (col, c) in row.iter().enumerate() {
        let bg = cell_style(c).bg;
        if bg == TERM_BG() {
            flush(run.take());
            continue;
        }
        match run {
            Some((start, end, color)) if color == bg && end == col => run = Some((start, col + 1, color)),
            _ => {
                flush(run.take());
                run = Some((col, col + 1, bg));
            }
        }
    }
    flush(run);
}

/// Draws the text of one row as style runs; wide characters are drawn one by one.
fn paint_row_text(ui: &Ui, painter: &egui::Painter, row: &[TermCell], left: f32, top: f32, cell: Vec2, font: &FontId) {
    let mut run_text = String::new();
    let mut run_start = 0usize;
    let mut run_style: Option<CellStyle> = None;

    let flush = |text: &mut String, start: usize, style: Option<CellStyle>| {
        if let Some(style) = style {
            let visible = text.chars().any(|c| c != ' ');
            if visible || style.underline || style.strikeout {
                draw_text(ui, painter, text, Pos2::new(left + start as f32 * cell.x, top), style, font);
            }
        }
        text.clear();
    };

    for (col, c) in row.iter().enumerate() {
        if c.wide_spacer {
            continue;
        }
        let style = cell_style(c);
        if c.wide {
            flush(&mut run_text, run_start, run_style.take());
            let mut text = String::new();
            text.push(c.ch);
            text.extend(c.zero_width.iter());
            draw_text(ui, painter, &text, Pos2::new(left + col as f32 * cell.x, top), style, font);
            continue;
        }
        if run_style != Some(style) {
            flush(&mut run_text, run_start, run_style.take());
            run_start = col;
            run_style = Some(style);
        }
        run_text.push(c.ch);
        run_text.extend(c.zero_width.iter());
    }
    flush(&mut run_text, run_start, run_style);
}

fn draw_text(ui: &Ui, painter: &egui::Painter, text: &str, pos: Pos2, style: CellStyle, font: &FontId) {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: font.clone(),
            color: style.fg,
            italics: style.italic,
            underline: if style.underline { Stroke::new(1.0_f32, style.fg) } else { Stroke::NONE },
            strikethrough: if style.strikeout { Stroke::new(1.0_f32, style.fg) } else { Stroke::NONE },
            ..Default::default()
        },
    );
    job.wrap.max_width = f32::INFINITY;
    let galley = ui.fonts(|f| f.layout_job(job));
    painter.galley(pos, galley, style.fg);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_dims_fit_the_available_space() {
        assert_eq!(grid_dims(Vec2::new(800.0, 400.0), Vec2::new(8.0, 16.0)), (100, 25));
        // Partial cells do not count.
        assert_eq!(grid_dims(Vec2::new(805.0, 415.0), Vec2::new(8.0, 16.0)), (100, 25));
        // Tiny areas still give a usable grid.
        assert_eq!(grid_dims(Vec2::new(1.0, 1.0), Vec2::new(8.0, 16.0)), (2, 1));
    }

    #[test]
    fn inverse_swaps_resolved_colors_and_bold_brightens() {
        let cell = TermCell {
            fg: TermColor::Indexed(1),
            inverse: true,
            ..TermCell::default()
        };
        let s = cell_style(&cell);
        assert_eq!(s.bg, to_color32(xterm_palette(1)));
        assert_eq!(s.fg, TERM_BG());

        let bold = TermCell {
            fg: TermColor::Indexed(2),
            bold: true,
            ..TermCell::default()
        };
        assert_eq!(cell_style(&bold).fg, to_color32(xterm_palette(10)));
    }

    fn snapshot_of(lines: &[&str], cols: u16) -> TermSnapshot {
        let mut term = mrt_core::TerminalEmulator::new(cols, lines.len() as u16, 0);
        term.feed(lines.join("\r\n").as_bytes());
        term.snapshot()
    }

    #[test]
    fn selection_runs_in_reading_order_and_trims_rows() {
        let snap = snapshot_of(&["hello world", "second line", "third"], 12);
        // Dragging backwards selects the same text as dragging forwards.
        let forward = Selection { anchor: (6, 0), head: (5, 1) };
        let backward = Selection { anchor: (5, 1), head: (6, 0) };
        assert_eq!(selection_text(&snap, &forward), "world\nsecond");
        assert_eq!(selection_text(&snap, &backward), "world\nsecond");
        // Whole middle rows are included, trailing blanks dropped.
        let three = Selection { anchor: (0, 0), head: (2, 2) };
        assert_eq!(selection_text(&snap, &three), "hello world\nsecond line\nthi");
        // A selection inside one row.
        let word = Selection { anchor: (0, 1), head: (5, 1) };
        assert_eq!(selection_text(&snap, &word), "second");
    }

    #[test]
    fn selection_columns_per_row() {
        let sel = Selection { anchor: (4, 1), head: (2, 3) };
        assert_eq!(sel.columns_on(0, 10), None);
        assert_eq!(sel.columns_on(1, 10), Some((4, 9)));
        assert_eq!(sel.columns_on(2, 10), Some((0, 9)));
        assert_eq!(sel.columns_on(3, 10), Some((0, 2)));
        assert_eq!(sel.columns_on(4, 10), None);
    }

    #[test]
    fn hidden_text_uses_the_background_color() {
        let cell = TermCell {
            fg: TermColor::Indexed(3),
            hidden: true,
            ..TermCell::default()
        };
        let s = cell_style(&cell);
        assert_eq!(s.fg, s.bg);
    }
}
