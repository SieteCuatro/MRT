//! Mosaic layout engine: arranges every terminal tab on screen at once.
//!
//! Layout math is a pure function ([`mosaic_rects`]) so it can be unit tested
//! independently of egui rendering.

use egui::{Pos2, Rect, Vec2};

/// Smallest comfortable tile size; layouts fall back to fewer columns (and
/// scrolling) rather than squeezing tiles below this.
pub const MIN_TILE_WIDTH: f32 = 240.0;
pub const MIN_TILE_HEIGHT: f32 = 150.0;
/// Side-by-side columns may get narrower than grid tiles before wrapping.
pub const MIN_COLUMN_WIDTH: f32 = 180.0;

/// How the Mosaic view arranges terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MosaicLayout {
    /// Even grid with a chosen number of columns.
    Grid,
    /// Every terminal side by side.
    Columns,
    /// Every terminal stacked top to bottom.
    Rows,
    /// The active terminal large on the left, the rest stacked on the right.
    Focus,
}

impl MosaicLayout {
    pub const ALL: [MosaicLayout; 4] = [
        MosaicLayout::Grid,
        MosaicLayout::Columns,
        MosaicLayout::Rows,
        MosaicLayout::Focus,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MosaicLayout::Grid => "Grid",
            MosaicLayout::Columns => "Columns",
            MosaicLayout::Rows => "Rows",
            MosaicLayout::Focus => "Focus",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            MosaicLayout::Grid => "Even grid; choose how many columns",
            MosaicLayout::Columns => "All terminals side by side",
            MosaicLayout::Rows => "All terminals stacked",
            MosaicLayout::Focus => "Active terminal large, the rest beside it",
        }
    }

    /// Parses a layout name (used by the MCP `mrt_control_gui` tool).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "grid" => Some(MosaicLayout::Grid),
            "columns" | "cols" | "side-by-side" => Some(MosaicLayout::Columns),
            "rows" | "stack" | "stacked" => Some(MosaicLayout::Rows),
            "focus" | "main" | "main-stack" => Some(MosaicLayout::Focus),
            _ => None,
        }
    }
}

/// Computes one rectangle per terminal, in the order the terminals should be
/// drawn (`order[i]` is the tab index for `rects[i]`).
///
/// `area` is the visible region; the returned content may be taller than it
/// when tiles would otherwise be smaller than [`MIN_TILE_HEIGHT`], in which
/// case the caller scrolls.
pub fn mosaic_rects(
    layout: MosaicLayout,
    count: usize,
    columns: usize,
    focus_index: usize,
    area: Rect,
    gap: f32,
) -> (Vec<usize>, Vec<Rect>) {
    if count == 0 {
        return (Vec::new(), Vec::new());
    }
    let focus_index = focus_index.min(count - 1);
    match layout {
        MosaicLayout::Focus if count > 1 && area.width() >= MIN_TILE_WIDTH * 2.0 + gap => {
            focus_rects(count, focus_index, area, gap)
        }
        MosaicLayout::Focus | MosaicLayout::Rows => grid_rects(count, 1, MIN_TILE_WIDTH, area, gap),
        MosaicLayout::Columns => grid_rects(count, count, MIN_COLUMN_WIDTH, area, gap),
        MosaicLayout::Grid => grid_rects(count, columns, MIN_TILE_WIDTH, area, gap),
    }
}

fn grid_rects(
    count: usize,
    columns: usize,
    min_width: f32,
    area: Rect,
    gap: f32,
) -> (Vec<usize>, Vec<Rect>) {
    let max_fit = (((area.width() + gap) / (min_width + gap)).floor() as usize).max(1);
    let cols = columns.clamp(1, count).min(max_fit);
    let rows = count.div_ceil(cols);
    let tile_h = ((area.height() - gap * (rows - 1) as f32) / rows as f32).max(MIN_TILE_HEIGHT);
    let mut rects = Vec::with_capacity(count);
    for row in 0..rows {
        let first = row * cols;
        let in_row = (count - first).min(cols);
        // The last row stretches its tiles so the grid never leaves a hole.
        let tile_w = (area.width() - gap * (in_row - 1) as f32) / in_row as f32;
        for col in 0..in_row {
            let min = Pos2::new(
                area.left() + col as f32 * (tile_w + gap),
                area.top() + row as f32 * (tile_h + gap),
            );
            rects.push(Rect::from_min_size(min, Vec2::new(tile_w, tile_h)));
        }
    }
    ((0..count).collect(), rects)
}

fn focus_rects(count: usize, focus: usize, area: Rect, gap: f32) -> (Vec<usize>, Vec<Rect>) {
    let main_w = ((area.width() - gap) * 0.62).max(MIN_TILE_WIDTH);
    let side_w = area.width() - gap - main_w;
    let others = count - 1;
    let side_h = ((area.height() - gap * (others - 1) as f32) / others as f32)
        .max(MIN_TILE_HEIGHT * 0.8);
    let mut order = vec![focus];
    let mut rects = vec![Rect::from_min_size(
        area.min,
        Vec2::new(main_w, area.height()),
    )];
    for (slot, index) in (0..count).filter(|i| *i != focus).enumerate() {
        order.push(index);
        rects.push(Rect::from_min_size(
            Pos2::new(
                area.left() + main_w + gap,
                area.top() + slot as f32 * (side_h + gap),
            ),
            Vec2::new(side_w, side_h),
        ));
    }
    (order, rects)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h))
    }

    fn no_overlap(rects: &[Rect]) -> bool {
        rects.iter().enumerate().all(|(i, a)| {
            rects
                .iter()
                .skip(i + 1)
                .all(|b| !a.shrink(0.5).intersects(b.shrink(0.5)))
        })
    }

    #[test]
    fn six_terminals_fill_a_three_column_grid_without_overflow() {
        let a = area(1200.0, 800.0);
        let (order, rects) = mosaic_rects(MosaicLayout::Grid, 6, 3, 0, a, 10.0);
        assert_eq!(order, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(rects.len(), 6);
        assert!(no_overlap(&rects));
        assert!(rects.iter().all(|r| r.right() <= a.right() + 0.5 && r.bottom() <= a.bottom() + 0.5));
        assert!((rects[0].width() - 393.33).abs() < 0.5);
        assert!((rects[0].height() - 395.0).abs() < 0.5);
    }

    #[test]
    fn last_row_stretches_to_fill_the_width() {
        let (_, rects) = mosaic_rects(MosaicLayout::Grid, 5, 3, 0, area(1200.0, 800.0), 10.0);
        assert!((rects[3].width() - 595.0).abs() < 0.5);
        assert!((rects[4].right() - 1200.0).abs() < 0.5);
    }

    #[test]
    fn narrow_areas_reduce_columns_instead_of_overflowing() {
        let a = area(320.0, 700.0);
        for layout in MosaicLayout::ALL {
            let (_, rects) = mosaic_rects(layout, 6, 6, 2, a, 10.0);
            assert!(rects.iter().all(|r| r.right() <= a.right() + 0.5), "{layout:?}");
            assert!(rects.iter().all(|r| r.height() >= MIN_TILE_HEIGHT * 0.8 - 0.5));
            assert!(no_overlap(&rects));
        }
    }

    #[test]
    fn focus_puts_the_active_terminal_first_and_largest() {
        let (order, rects) = mosaic_rects(MosaicLayout::Focus, 6, 3, 4, area(1400.0, 900.0), 10.0);
        assert_eq!(order[0], 4);
        assert_eq!(order.len(), 6);
        assert!(order[1..].iter().all(|i| *i != 4));
        assert!(rects[0].area() > rects[1].area() * 3.0);
        assert!(no_overlap(&rects));
    }

    #[test]
    fn rows_and_columns_use_one_axis() {
        let (_, rows) = mosaic_rects(MosaicLayout::Rows, 3, 3, 0, area(900.0, 600.0), 10.0);
        assert!(rows.iter().all(|r| (r.width() - 900.0).abs() < 0.5));
        let (_, cols) = mosaic_rects(MosaicLayout::Columns, 3, 1, 0, area(900.0, 600.0), 10.0);
        assert!(cols.iter().all(|r| (r.height() - 600.0).abs() < 0.5));
    }

    #[test]
    fn six_columns_stay_on_one_row_when_they_fit() {
        let (_, rects) = mosaic_rects(MosaicLayout::Columns, 6, 3, 0, area(1220.0, 800.0), 10.0);
        assert!(rects.iter().all(|r| r.top() == 0.0));
        assert!((rects[5].right() - 1220.0).abs() < 0.5);
    }

    #[test]
    fn layout_names_parse() {
        assert_eq!(MosaicLayout::parse("Focus"), Some(MosaicLayout::Focus));
        assert_eq!(MosaicLayout::parse("stack"), Some(MosaicLayout::Rows));
        assert_eq!(MosaicLayout::parse("nope"), None);
    }
}
