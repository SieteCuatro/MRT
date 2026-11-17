//! Copy mode (`copy_mode`, Ctrl+Shift+Space): move through the active tab's
//! output with the keyboard, select lines and copy them, without the mouse.
//!
//! The cursor is a line of one block's plain output. Keys while it is active:
//! `↑`/`↓` (or `k`/`j`) move a line, `PgUp`/`PgDn` a page, `Home`/`End` (or
//! `g`/`G`) to the first or last line, `[`/`]` (or `Shift+↑`/`Shift+↓`) to the
//! previous or next block, `v` starts or ends a selection, `y`/`Enter` copies the
//! selection (or the cursor line), `Y` the whole block output, `c` the command,
//! and `Esc`/`q` leaves.

use mrt_core::CommandBlock;

const PAGE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    PreviousBlock,
    NextBlock,
    ToggleSelection,
    CopySelection,
    CopyBlock,
    CopyCommand,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    Stay,
    /// Copy this text and leave copy mode.
    Copy(String),
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyMode {
    /// Index into the tab's blocks.
    pub block: usize,
    /// Line of that block's output.
    pub line: usize,
    /// First line of the selection, when one is being made (same block).
    pub anchor: Option<usize>,
}

fn line_count(block: &CommandBlock) -> usize {
    block.plain_output.trim_end_matches('\n').split('\n').count()
}

/// The output lines of a block (a trailing newline does not add an empty line).
pub fn block_lines(block: &CommandBlock) -> Vec<&str> {
    block.plain_output.trim_end_matches('\n').split('\n').collect()
}

impl CopyMode {
    /// Starts on the last line of the last block, or `None` without blocks.
    pub fn new(blocks: &[CommandBlock]) -> Option<Self> {
        let block = blocks.len().checked_sub(1)?;
        Some(Self { block, line: line_count(&blocks[block]) - 1, anchor: None })
    }

    /// The selected line range (inclusive), or just the cursor line.
    pub fn range(&self) -> (usize, usize) {
        match self.anchor {
            Some(a) => (a.min(self.line), a.max(self.line)),
            None => (self.line, self.line),
        }
    }

    pub fn selection_text(&self, blocks: &[CommandBlock]) -> String {
        let Some(block) = blocks.get(self.block) else { return String::new() };
        let lines = block_lines(block);
        let (a, b) = self.range();
        lines[a.min(lines.len() - 1)..=b.min(lines.len() - 1)].join("\n")
    }

    pub fn apply(&mut self, key: CopyKey, blocks: &[CommandBlock]) -> CopyOutcome {
        if blocks.is_empty() {
            return CopyOutcome::Exit;
        }
        self.block = self.block.min(blocks.len() - 1);
        let last = line_count(&blocks[self.block]) - 1;
        let move_block = |to: usize, at_end: bool, this: &mut Self| {
            this.block = to;
            this.anchor = None;
            this.line = if at_end { line_count(&blocks[to]) - 1 } else { 0 };
        };
        match key {
            CopyKey::Up if self.line == 0 && self.anchor.is_none() && self.block > 0 => move_block(self.block - 1, true, self),
            CopyKey::Down if self.line == last && self.anchor.is_none() && self.block + 1 < blocks.len() => {
                move_block(self.block + 1, false, self)
            }
            CopyKey::Up => self.line = self.line.saturating_sub(1),
            CopyKey::Down => self.line = (self.line + 1).min(last),
            CopyKey::PageUp => self.line = self.line.saturating_sub(PAGE),
            CopyKey::PageDown => self.line = (self.line + PAGE).min(last),
            CopyKey::Top => self.line = 0,
            CopyKey::Bottom => self.line = last,
            CopyKey::PreviousBlock if self.block > 0 => move_block(self.block - 1, false, self),
            CopyKey::NextBlock if self.block + 1 < blocks.len() => move_block(self.block + 1, false, self),
            CopyKey::PreviousBlock | CopyKey::NextBlock => {}
            CopyKey::ToggleSelection => {
                self.anchor = match self.anchor {
                    Some(_) => None,
                    None => Some(self.line),
                }
            }
            CopyKey::CopySelection => return CopyOutcome::Copy(self.selection_text(blocks)),
            CopyKey::CopyBlock => {
                return CopyOutcome::Copy(blocks[self.block].plain_output.trim_end_matches('\n').to_string())
            }
            CopyKey::CopyCommand => return CopyOutcome::Copy(blocks[self.block].command.clone()),
            CopyKey::Exit => return CopyOutcome::Exit,
        }
        CopyOutcome::Stay
    }
}

/// The copy-mode key for a key press, if it is one.
pub fn key_for(key: egui::Key, modifiers: egui::Modifiers) -> Option<CopyKey> {
    use egui::Key as K;
    Some(match key {
        K::ArrowUp if modifiers.shift => CopyKey::PreviousBlock,
        K::ArrowDown if modifiers.shift => CopyKey::NextBlock,
        K::ArrowUp | K::K => CopyKey::Up,
        K::ArrowDown | K::J => CopyKey::Down,
        K::PageUp => CopyKey::PageUp,
        K::PageDown => CopyKey::PageDown,
        K::Home => CopyKey::Top,
        K::G if modifiers.shift => CopyKey::Bottom,
        K::G => CopyKey::Top,
        K::End => CopyKey::Bottom,
        K::OpenBracket => CopyKey::PreviousBlock,
        K::CloseBracket => CopyKey::NextBlock,
        K::V | K::Space => CopyKey::ToggleSelection,
        K::Y if modifiers.shift => CopyKey::CopyBlock,
        K::Y | K::Enter => CopyKey::CopySelection,
        K::C if !modifiers.command => CopyKey::CopyCommand,
        K::Escape | K::Q => CopyKey::Exit,
        _ => return None,
    })
}

/// Draws the copy-mode panel: the block's command and the lines around the cursor,
/// with the selection highlighted.
pub fn render(ctx: &egui::Context, mode: &CopyMode, blocks: &[CommandBlock]) {
    use crate::theme::Palette;
    let Some(block) = blocks.get(mode.block) else { return };
    let lines = block_lines(block);
    let (a, b) = mode.range();
    let first = mode.line.saturating_sub(8);
    let end = (first + 17).min(lines.len());
    egui::Window::new("Copy mode")
        .id(egui::Id::new("mrt_copy_mode"))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -24.0])
        .collapsible(false)
        .resizable(false)
        .default_width(720.0)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Block {}/{} · $ {} · line {}/{}{}",
                    mode.block + 1,
                    blocks.len(),
                    block.command,
                    mode.line + 1,
                    lines.len(),
                    if mode.anchor.is_some() { format!(" · {} lines selected", b - a + 1) } else { String::new() }
                ))
                .color(Palette::TEXT_SECONDARY()),
            );
            ui.separator();
            for (n, line) in lines.iter().enumerate().take(end).skip(first) {
                let selected = n >= a && n <= b && (mode.anchor.is_some() || n == mode.line);
                let marker = if n == mode.line { "▶" } else { " " };
                let mut text = egui::RichText::new(format!("{marker}{:>5} {line}", n + 1)).monospace();
                if selected {
                    text = text.background_color(Palette::ACCENT().gamma_multiply(0.35)).color(Palette::TEXT_PRIMARY());
                } else {
                    text = text.color(Palette::TEXT_MUTED());
                }
                ui.add(egui::Label::new(text).truncate());
            }
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "↑↓ j k line · PgUp PgDn · g G · [ ] or Shift+↑↓ block · v select · y/Enter copy · Y block · c command · Esc",
                )
                .size(11.0)
                .color(Palette::TEXT_MUTED()),
            );
        });
}

/// Copy mode inside a full-screen program's grid: a keyboard cursor over the visible
/// screen, with the same keys as block copy mode (`h` `j` `k` `l` or arrows, `0` / `$`
/// for the line's start / end, `g` / `G`, `v` to select, `y` / Enter to copy, Esc).
/// Keys go to MRT, not the program, until it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCopy {
    /// (column, row) of the cursor.
    pub cursor: (u16, u16),
    pub anchor: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridOutcome {
    Stay,
    /// Copy the selection (or the cursor line when nothing is selected) and leave.
    Copy { whole_line: bool },
    Exit,
    /// Scroll the view this many lines into the history (negative: towards the screen).
    Scroll(i32),
    /// `/`: start typing a search.
    StartSearch,
    /// `n` / `N`: the next older / newer match.
    NextMatch,
    PreviousMatch,
}

/// A search in the grid's history and screen (`/` in grid copy mode).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridSearch {
    /// `Some` while the query is being typed.
    pub typing: Option<String>,
    pub query: String,
    /// Matches as (history line, column), oldest first.
    pub matches: Vec<(usize, u16)>,
    /// Index of the current match.
    pub current: Option<usize>,
}

impl GridSearch {
    /// The match to go to after `n` (older) or `N` (newer), wrapping around.
    pub fn step(&mut self, older: bool) -> Option<(usize, u16)> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        let next = match self.current {
            None => n - 1,
            Some(i) if older => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
        };
        self.current = Some(next);
        Some(self.matches[next])
    }

    /// "/query (3 of 10)" for the status line.
    pub fn status(&self) -> String {
        match (&self.typing, self.current) {
            (Some(typing), _) => format!("/{typing}"),
            (None, Some(i)) => format!("/{} ({} of {})", self.query, i + 1, self.matches.len()),
            (None, None) if !self.query.is_empty() => format!("/{} (no matches)", self.query),
            _ => String::new(),
        }
    }
}

impl GridCopy {
    pub fn new(col: u16, row: u16) -> Self {
        Self { cursor: (col, row), anchor: None }
    }

    /// The selection to shade: from the anchor to the cursor, or the cursor cell.
    pub fn selection(&self) -> crate::widgets::term_view::Selection {
        crate::widgets::term_view::Selection { anchor: self.anchor.unwrap_or(self.cursor), head: self.cursor }
    }

    pub fn apply(&mut self, key: egui::Key, modifiers: egui::Modifiers, cols: u16, rows: u16) -> GridOutcome {
        use egui::Key as K;
        let (max_c, max_r) = (cols.saturating_sub(1), rows.saturating_sub(1));
        let (c, r) = &mut self.cursor;
        match key {
            K::ArrowLeft | K::H => *c = c.saturating_sub(1),
            K::ArrowRight | K::L => *c = (*c + 1).min(max_c),
            // Moving past the top or bottom row scrolls through the history.
            K::ArrowUp | K::K if *r == 0 => return GridOutcome::Scroll(1),
            K::ArrowDown | K::J if *r == max_r => return GridOutcome::Scroll(-1),
            K::ArrowUp | K::K => *r = r.saturating_sub(1),
            K::ArrowDown | K::J => *r = (*r + 1).min(max_r),
            K::Slash => return GridOutcome::StartSearch,
            K::N if modifiers.shift => return GridOutcome::PreviousMatch,
            K::N => return GridOutcome::NextMatch,
            K::Num0 | K::Home => *c = 0,
            K::Num4 if modifiers.shift => *c = max_c,
            K::End => *c = max_c,
            K::G if modifiers.shift => *r = max_r,
            K::G => *r = 0,
            K::PageUp => return GridOutcome::Scroll(i32::from(rows / 2).max(1)),
            K::PageDown => return GridOutcome::Scroll(-i32::from(rows / 2).max(1)),
            K::V | K::Space => {
                self.anchor = match self.anchor {
                    Some(_) => None,
                    None => Some(self.cursor),
                }
            }
            K::Y | K::Enter => return GridOutcome::Copy { whole_line: self.anchor.is_none() },
            K::Escape | K::Q => return GridOutcome::Exit,
            _ => {}
        }
        GridOutcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_search_steps_through_matches_and_edges_scroll() {
        let mut search = GridSearch { query: "x".into(), matches: vec![(1, 0), (5, 2), (9, 4)], ..Default::default() };
        assert_eq!(search.step(true), Some((9, 4)), "the first n goes to the newest match");
        assert_eq!(search.step(true), Some((5, 2)));
        assert_eq!(search.status(), "/x (2 of 3)");
        assert_eq!(search.step(false), Some((9, 4)));
        assert_eq!(search.step(false), Some((1, 0)), "wraps around");
        assert_eq!(GridSearch::default().step(true), None);
        let none = egui::Modifiers::NONE;
        let mut grid = GridCopy::new(0, 0);
        assert_eq!(grid.apply(egui::Key::K, none, 80, 24), GridOutcome::Scroll(1));
        assert_eq!(grid.apply(egui::Key::PageDown, none, 80, 24), GridOutcome::Scroll(-12));
        assert_eq!(grid.apply(egui::Key::Slash, none, 80, 24), GridOutcome::StartSearch);
        assert_eq!(grid.apply(egui::Key::N, egui::Modifiers::SHIFT, 80, 24), GridOutcome::PreviousMatch);
    }

    #[test]
    fn grid_copy_moves_selects_and_copies() {
        let none = egui::Modifiers::NONE;
        let mut grid = GridCopy::new(5, 2);
        grid.apply(egui::Key::H, none, 80, 24);
        grid.apply(egui::Key::K, none, 80, 24);
        assert_eq!(grid.cursor, (4, 1));
        grid.apply(egui::Key::Num4, egui::Modifiers::SHIFT, 80, 24);
        assert_eq!(grid.cursor, (79, 1), "$ goes to the end of the line");
        grid.apply(egui::Key::G, egui::Modifiers::SHIFT, 80, 24);
        assert_eq!(grid.cursor.1, 23);
        assert_eq!(grid.apply(egui::Key::Y, none, 80, 24), GridOutcome::Copy { whole_line: true });
        grid.apply(egui::Key::V, none, 80, 24);
        grid.apply(egui::Key::K, none, 80, 24);
        let sel = grid.selection();
        assert_eq!((sel.anchor, sel.head), ((79, 23), (79, 22)));
        assert_eq!(grid.apply(egui::Key::Enter, none, 80, 24), GridOutcome::Copy { whole_line: false });
        assert_eq!(grid.apply(egui::Key::Escape, none, 80, 24), GridOutcome::Exit);
    }

    fn blocks() -> Vec<CommandBlock> {
        vec![
            CommandBlock::new_completed("first", "sh", "/", "a\nb\nc\n", 0, 0),
            CommandBlock::new_completed("second", "sh", "/", "one\ntwo", 0, 0),
        ]
    }

    #[test]
    fn starts_at_the_end_and_moves_across_blocks() {
        let blocks = blocks();
        let mut mode = CopyMode::new(&blocks).unwrap();
        assert_eq!((mode.block, mode.line), (1, 1));
        mode.apply(CopyKey::Up, &blocks);
        mode.apply(CopyKey::Up, &blocks);
        assert_eq!((mode.block, mode.line), (0, 2), "up from the first line goes to the previous block's end");
        mode.apply(CopyKey::Down, &blocks);
        assert_eq!((mode.block, mode.line), (1, 0));
        mode.apply(CopyKey::PreviousBlock, &blocks);
        assert_eq!((mode.block, mode.line), (0, 0));
        mode.apply(CopyKey::PageDown, &blocks);
        assert_eq!(mode.line, 2);
        assert!(CopyMode::new(&[]).is_none());
    }

    #[test]
    fn selects_and_copies() {
        let blocks = blocks();
        let mut mode = CopyMode { block: 0, line: 0, anchor: None };
        assert_eq!(mode.apply(CopyKey::CopySelection, &blocks), CopyOutcome::Copy("a".into()));
        mode.apply(CopyKey::ToggleSelection, &blocks);
        mode.apply(CopyKey::Down, &blocks);
        mode.apply(CopyKey::Down, &blocks);
        mode.apply(CopyKey::Down, &blocks);
        assert_eq!(mode.block, 0, "a selection stays in its block");
        assert_eq!(mode.apply(CopyKey::CopySelection, &blocks), CopyOutcome::Copy("a\nb\nc".into()));
        assert_eq!(mode.apply(CopyKey::CopyBlock, &blocks), CopyOutcome::Copy("a\nb\nc".into()));
        assert_eq!(mode.apply(CopyKey::CopyCommand, &blocks), CopyOutcome::Copy("first".into()));
        assert_eq!(mode.apply(CopyKey::Exit, &blocks), CopyOutcome::Exit);
    }

    #[test]
    fn keys_map_like_vi_and_arrows() {
        let none = egui::Modifiers::NONE;
        assert_eq!(key_for(egui::Key::J, none), Some(CopyKey::Down));
        assert_eq!(key_for(egui::Key::G, egui::Modifiers::SHIFT), Some(CopyKey::Bottom));
        assert_eq!(key_for(egui::Key::Y, egui::Modifiers::SHIFT), Some(CopyKey::CopyBlock));
        assert_eq!(key_for(egui::Key::ArrowUp, egui::Modifiers::SHIFT), Some(CopyKey::PreviousBlock));
        assert_eq!(key_for(egui::Key::A, none), None);
    }
}
