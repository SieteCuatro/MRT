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

#[cfg(test)]
mod tests {
    use super::*;

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
