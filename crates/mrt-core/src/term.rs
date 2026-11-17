//! Terminal grid emulator.
//!
//! [`TerminalEmulator`] turns the raw byte stream coming out of a PTY into a
//! screen of cells, the way a real terminal does: cursor movement, erase,
//! scroll regions, the alternate screen, wide characters, colors, and the
//! modes that full-screen programs (`vim`, `htop`, `less`, `lazygit`) switch
//! on and off.
//!
//! The emulation itself is done by [`alacritty_terminal`]. This module wraps
//! it in an MRT-owned API so that no `alacritty_terminal` type appears in a
//! public signature: callers only see [`TermCell`], [`TermColor`],
//! [`TermSnapshot`], [`TermCursor`] and [`TermModes`]. Swapping the backend
//! later only touches this file.
//!
//! The emulator is a pure state machine. It never reads from or writes to a
//! PTY by itself: feed it output with [`TerminalEmulator::feed`] and send the
//! bytes returned by [`TerminalEmulator::take_replies`] (device attribute and
//! cursor position reports, color queries) back to the PTY.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, CursorShape, NamedColor};
use serde::{Deserialize, Serialize};

use crate::vt::AnsiColor;

/// Smallest grid the emulator accepts; smaller sizes are clamped.
pub const MIN_COLS: u16 = 2;
/// Smallest number of rows the emulator accepts; smaller sizes are clamped.
pub const MIN_ROWS: u16 = 1;

/// Default foreground reported to programs that query it (OSC 10).
pub const DEFAULT_FG: AnsiColor = AnsiColor::new(0xd4, 0xd4, 0xd4);
/// Default background reported to programs that query it (OSC 11).
pub const DEFAULT_BG: AnsiColor = AnsiColor::new(0x1e, 0x1e, 0x1e);

/// A cell color, left unresolved so the renderer can apply its own theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TermColor {
    /// The theme's default foreground.
    DefaultFg,
    /// The theme's default background.
    DefaultBg,
    /// A palette color: 0-15 are the ANSI colors (8-15 bright), 16-231 the
    /// 6x6x6 color cube, 232-255 the grayscale ramp.
    Indexed(u8),
    /// A 24-bit truecolor value.
    Rgb(AnsiColor),
}

impl TermColor {
    /// Resolves the color to RGB using the xterm palette and the given
    /// default foreground/background.
    pub fn to_rgb(self, default_fg: AnsiColor, default_bg: AnsiColor) -> AnsiColor {
        match self {
            TermColor::DefaultFg => default_fg,
            TermColor::DefaultBg => default_bg,
            TermColor::Indexed(index) => xterm_palette(index),
            TermColor::Rgb(rgb) => rgb,
        }
    }
}

/// The SGR parameters that select `cell`'s style from the default style.
fn sgr_params(cell: &TermCell) -> Vec<String> {
    let mut params = Vec::new();
    let flags = [
        (cell.bold, "1"),
        (cell.dim, "2"),
        (cell.italic, "3"),
        (cell.underline, "4"),
        (cell.inverse, "7"),
        (cell.hidden, "8"),
        (cell.strikeout, "9"),
    ];
    params.extend(flags.iter().filter(|(on, _)| *on).map(|(_, p)| p.to_string()));
    let color = |c: TermColor, base: u8, bright: u8, ext: u8| -> Option<String> {
        match c {
            TermColor::DefaultFg | TermColor::DefaultBg => None,
            TermColor::Indexed(i) if i < 8 => Some((base + i).to_string()),
            TermColor::Indexed(i) if i < 16 => Some((bright + i - 8).to_string()),
            TermColor::Indexed(i) => Some(format!("{ext};5;{i}")),
            TermColor::Rgb(rgb) => Some(format!("{ext};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
        }
    };
    params.extend(color(cell.fg, 30, 90, 38));
    params.extend(color(cell.bg, 40, 100, 48));
    params
}

/// One row as text with SGR sequences, trailing default-styled blanks removed.
fn cells_to_sgr(cells: &[TermCell]) -> String {
    // A space shows nothing unless it has a background, is inverse, or draws a line.
    let is_blank = |c: &TermCell| {
        c.ch == ' '
            && c.zero_width.is_empty()
            && matches!(c.bg, TermColor::DefaultBg)
            && !c.inverse
            && !c.underline
            && !c.strikeout
    };
    let end = cells.iter().rposition(|c| !c.wide_spacer && !is_blank(c)).map_or(0, |i| i + 1);
    let mut out = String::new();
    let mut current: Vec<String> = Vec::new();
    for cell in cells[..end].iter().filter(|c| !c.wide_spacer) {
        let params = sgr_params(cell);
        if params != current {
            out.push_str("\x1b[0");
            for p in &params {
                out.push(';');
                out.push_str(p);
            }
            out.push('m');
            current = params;
        }
        out.push(cell.ch);
        out.extend(cell.zero_width.iter());
    }
    if !current.is_empty() {
        out.push_str("\x1b[0m");
    }
    out
}

/// The xterm 256-color palette entry for `index`.
pub fn xterm_palette(index: u8) -> AnsiColor {
    const ANSI16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match index {
        0..=15 => {
            if let Some(color) = crate::theme_file::themed_ansi(index as usize) {
                return color;
            }
            let (r, g, b) = ANSI16[index as usize];
            AnsiColor::new(r, g, b)
        }
        16..=231 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            AnsiColor::new(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            AnsiColor::new(v, v, v)
        }
    }
}

/// One cell of the terminal grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCell {
    /// The character in the cell (a space for empty cells).
    pub ch: char,
    /// Combining characters drawn on top of `ch` (accents, ZWJ sequences).
    pub zero_width: Vec<char>,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub inverse: bool,
    pub hidden: bool,
    /// The cell holds a double-width character (CJK, most emoji) that also
    /// covers the next cell.
    pub wide: bool,
    /// The cell is the right half of a wide character; renderers skip it.
    pub wide_spacer: bool,
}

impl Default for TermCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            zero_width: Vec::new(),
            fg: TermColor::DefaultFg,
            bg: TermColor::DefaultBg,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strikeout: false,
            inverse: false,
            hidden: false,
            wide: false,
            wide_spacer: false,
        }
    }
}

/// Cursor shape requested by the program (DECSCUSR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TermCursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

/// Cursor position (0-based, within the visible screen) and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: TermCursorShape,
}

/// Which mouse events the program asked to receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MouseMode {
    /// No mouse reporting; the terminal may use the mouse for selection.
    #[default]
    None,
    /// Button press/release only (mode 1000).
    Click,
    /// Press/release plus motion while a button is held (mode 1002).
    Drag,
    /// Press/release plus all motion (mode 1003).
    Motion,
}

/// Terminal modes the input side needs to encode keys, paste and mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TermModes {
    /// DECCKM: arrow keys send `ESC O A` instead of `ESC [ A`.
    pub app_cursor: bool,
    /// DECKPAM: the keypad sends application sequences.
    pub app_keypad: bool,
    /// Mode 2004: pasted text must be wrapped in `ESC [200~` / `ESC [201~`.
    pub bracketed_paste: bool,
    /// Mouse reporting mode (1000/1002/1003).
    pub mouse: MouseMode,
    /// Mode 1006: mouse reports use the SGR encoding.
    pub sgr_mouse: bool,
    /// Mode 1005: mouse reports use the UTF-8 encoding.
    pub utf8_mouse: bool,
    /// Mode 1004: report focus in/out.
    pub focus_reporting: bool,
    /// Mode 1007: on the alternate screen, the scroll wheel sends arrow keys.
    pub alternate_scroll: bool,
    /// The alternate screen (mode 1049/47/1047) is active.
    pub alt_screen: bool,
    /// Kitty keyboard protocol flags the program enabled (`CSI > flags u`): 1
    /// disambiguate, 2 report event types, 4 alternate keys, 8 all keys as escape
    /// codes, 16 associated text. `0` means legacy xterm keys.
    #[serde(default)]
    pub kitty_keyboard: u8,
}

/// A copy of the visible screen at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// `rows` rows of `cols` cells each, top to bottom.
    pub cells: Vec<Vec<TermCell>>,
    pub cursor: TermCursor,
    pub modes: TermModes,
    pub title: Option<String>,
}

impl TermSnapshot {
    /// The cell at `row`/`col`, if inside the grid.
    pub fn cell(&self, row: u16, col: u16) -> Option<&TermCell> {
        self.cells.get(row as usize)?.get(col as usize)
    }
}

/// State the backend reports through events, shared with the listener.
#[derive(Debug, Default)]
struct SharedState {
    replies: Vec<u8>,
    title: Option<String>,
    bell: bool,
    cell_width_px: u16,
    cell_height_px: u16,
    cols: u16,
    rows: u16,
}

#[derive(Clone)]
struct Listener(Arc<Mutex<SharedState>>);

impl Listener {
    fn state(&self) -> MutexGuard<'_, SharedState> {
        // A poisoned lock only means a panic elsewhere; the data is still usable.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mut state = self.state();
        match event {
            Event::PtyWrite(text) => state.replies.extend_from_slice(text.as_bytes()),
            Event::Title(title) => state.title = Some(title),
            Event::ResetTitle => state.title = None,
            Event::Bell => state.bell = true,
            Event::ColorRequest(index, format) => {
                let rgb = match index {
                    0..=255 => xterm_palette(index as u8),
                    i if i == NamedColor::Background as usize => DEFAULT_BG,
                    _ => DEFAULT_FG,
                };
                let reply = format(ansi::Rgb {
                    r: rgb.r,
                    g: rgb.g,
                    b: rgb.b,
                });
                state.replies.extend_from_slice(reply.as_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let reply = format(WindowSize {
                    num_lines: state.rows,
                    num_cols: state.cols,
                    cell_width: state.cell_width_px,
                    cell_height: state.cell_height_px,
                });
                state.replies.extend_from_slice(reply.as_bytes());
            }
            // OSC 52 clipboard access is disabled in the config, and the rest
            // are UI notifications MRT does not need from the emulator.
            Event::ClipboardStore(..)
            | Event::ClipboardLoad(..)
            | Event::MouseCursorDirty
            | Event::CursorBlinkingChange
            | Event::Wakeup
            | Event::Exit
            | Event::ChildExit(_) => {}
        }
    }
}

#[derive(Clone, Copy)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// A terminal screen emulator: feed it PTY output, read back the grid.
pub struct TerminalEmulator {
    term: Term<Listener>,
    parser: ansi::Processor,
    listener: Listener,
    /// Takes image sequences out of the stream (alacritty has no graphics).
    graphics: crate::grid_images::GraphicsSplitter,
    /// Decodes them (keeps kitty's chunk and `a=t` state between sequences).
    graphics_decoder: crate::vt::VtStream,
    images: Vec<crate::grid_images::GridImage>,
    next_image_id: u64,
    images_on_alt_screen: bool,
    /// Lines the view is scrolled back into the history (0 = the live screen).
    view_offset: usize,
}

/// Most images kept per emulator; older ones are dropped.
const MAX_GRID_IMAGES: usize = 64;

impl std::fmt::Debug for TerminalEmulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalEmulator")
            .field("cols", &self.cols())
            .field("rows", &self.rows())
            .field("scrollback_len", &self.scrollback_len())
            .finish()
    }
}

impl TerminalEmulator {
    /// Creates an emulator with a `cols` x `rows` screen that keeps up to
    /// `scrollback` lines of history.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let (cols, rows) = clamp_size(cols, rows);
        let listener = Listener(Arc::new(Mutex::new(SharedState {
            cell_width_px: 8,
            cell_height_px: 16,
            cols,
            rows,
            ..SharedState::default()
        })));
        let config = Config {
            scrolling_history: scrollback,
            // Never let a program read or overwrite the clipboard through the
            // terminal; MRT can opt in later with an explicit setting.
            osc52: Osc52::Disabled,
            // Programs may turn on the kitty keyboard protocol (CSI > flags u); the
            // window encodes keys accordingly (`TermModes::kitty_keyboard`).
            kitty_keyboard: true,
            ..Config::default()
        };
        let size = GridSize {
            cols: cols as usize,
            rows: rows as usize,
        };
        let term = Term::new(config, &size, listener.clone());
        Self {
            term,
            parser: ansi::Processor::new(),
            listener,
            graphics: crate::grid_images::GraphicsSplitter::default(),
            graphics_decoder: crate::vt::VtStream::new(),
            images: Vec::new(),
            next_image_id: 0,
            images_on_alt_screen: false,
            view_offset: 0,
        }
    }

    /// Feeds raw PTY output into the emulator. Inline images (iTerm2, kitty, Sixel)
    /// are placed at the cursor ([`images_on_screen`](Self::images_on_screen)).
    pub fn feed(&mut self, bytes: &[u8]) {
        let history_before = self.scrollback_len();
        self.feed_inner(bytes);
        // A view scrolled into the history stays on the same lines while output arrives.
        if self.view_offset > 0 {
            let grown = self.scrollback_len().saturating_sub(history_before);
            self.view_offset = (self.view_offset + grown).min(self.scrollback_len());
        }
    }

    fn feed_inner(&mut self, bytes: &[u8]) {
        self.flush_expired_sync();
        for segment in self.graphics.split(bytes) {
            match segment {
                crate::grid_images::Segment::Text(text) => self.parser.advance(&mut self.term, &text),
                crate::grid_images::Segment::Graphic(seq) => self.place_graphic(&seq),
            }
        }
        // Switching between the primary and alternate screen leaves the images behind.
        let alt = self.is_alt_screen();
        if alt != self.images_on_alt_screen {
            self.images.clear();
            self.images_on_alt_screen = alt;
        }
    }

    fn place_graphic(&mut self, seq: &[u8]) {
        let decoded = self.graphics_decoder.feed(&String::from_utf8_lossy(seq));
        // Decoding problems are shown as text, like in blocks.
        if !decoded.plain_text.is_empty() {
            let text = decoded.plain_text.replace('\n', "\r\n");
            self.parser.advance(&mut self.term, text.as_bytes());
        }
        for image in decoded.images {
            let cursor = self.cursor();
            let (cell_w, cell_h) = {
                let state = self.listener.state();
                (state.cell_width_px, state.cell_height_px)
            };
            let (cols, rows) = crate::grid_images::image_cells(&image, cursor.col, self.cols(), self.rows(), cell_w, cell_h);
            let line = self.scrollback_len() + cursor.row as usize;
            self.next_image_id += 1;
            self.images.push(crate::grid_images::GridImage { id: self.next_image_id, image, line, col: cursor.col, cols, rows });
            if self.images.len() > MAX_GRID_IMAGES {
                self.images.remove(0);
            }
            // Move below the image, as iTerm2 does, so following text does not cover it.
            let mut advance = Vec::with_capacity(rows as usize * 2);
            for _ in 0..rows {
                advance.extend_from_slice(b"\r\n");
            }
            self.parser.advance(&mut self.term, &advance);
        }
    }

    /// Images that are at least partly on the visible screen. `row` is relative to the
    /// top of the screen and may be negative for an image scrolled partly off it.
    pub fn images_on_screen(&self) -> Vec<crate::grid_images::GridImagePlacement> {
        let top = self.scrollback_len() as i64 - self.view_offset as i64;
        let rows = self.rows() as i64;
        self.images
            .iter()
            .filter_map(|img| {
                let row = img.line as i64 - top;
                (row + img.rows as i64 > 0 && row < rows).then(|| crate::grid_images::GridImagePlacement {
                    id: img.id,
                    row: row as i32,
                    col: img.col,
                    cols: img.cols,
                    rows: img.rows,
                })
            })
            .collect()
    }

    /// The image with `id`, for drawing.
    pub fn image(&self, id: u64) -> Option<&crate::images::InlineImage> {
        self.images.iter().find(|img| img.id == id).map(|img| &img.image)
    }

    /// When a program started a synchronized update (mode 2026) and has not
    /// ended it, output is held back until this deadline. Call
    /// [`flush_sync`](Self::flush_sync) once it passes.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Applies output held back by a pending synchronized update.
    pub fn flush_sync(&mut self) {
        if self.sync_deadline().is_some() {
            self.parser.stop_sync(&mut self.term);
        }
    }

    fn flush_expired_sync(&mut self) {
        if self
            .sync_deadline()
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.parser.stop_sync(&mut self.term);
        }
    }

    /// Resizes the screen. Sizes below [`MIN_COLS`] x [`MIN_ROWS`] are clamped.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = clamp_size(cols, rows);
        {
            let mut state = self.listener.state();
            state.cols = cols;
            state.rows = rows;
        }
        self.term.resize(GridSize {
            cols: cols as usize,
            rows: rows as usize,
        });
    }

    /// Sets the cell size in pixels reported to programs that ask for the
    /// text area size in pixels (`CSI 14 t`). Defaults to 8x16.
    pub fn set_cell_size_px(&mut self, width: u16, height: u16) {
        let mut state = self.listener.state();
        state.cell_width_px = width;
        state.cell_height_px = height;
    }

    pub fn cols(&self) -> u16 {
        self.term.grid().columns() as u16
    }

    pub fn rows(&self) -> u16 {
        self.term.grid().screen_lines() as u16
    }

    /// Bytes the terminal must write back to the PTY (device attributes,
    /// cursor position and status reports, color and size queries). Drains
    /// the buffer: each reply is returned once.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.listener.state().replies)
    }

    /// Whether a bell (BEL) rang since the last call; clears the flag.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.listener.state().bell)
    }

    /// The window title set by the program (OSC 0/2), if any.
    pub fn title(&self) -> Option<String> {
        self.listener.state().title.clone()
    }

    pub fn cursor(&self) -> TermCursor {
        let grid = self.term.grid();
        let mut point = grid.cursor.point;
        if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
            point.column -= 1;
        }
        let style = self.term.cursor_style();
        let shape = match style.shape {
            CursorShape::Underline => TermCursorShape::Underline,
            CursorShape::Beam => TermCursorShape::Beam,
            CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => {
                TermCursorShape::Block
            }
        };
        TermCursor {
            row: point.line.0.max(0) as u16,
            col: point.column.0 as u16,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR)
                && style.shape != CursorShape::Hidden,
            shape,
        }
    }

    pub fn modes(&self) -> TermModes {
        let mode = *self.term.mode();
        let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseMode::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseMode::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseMode::Click
        } else {
            MouseMode::None
        };
        TermModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse,
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            utf8_mouse: mode.contains(TermMode::UTF8_MOUSE),
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            kitty_keyboard: [
                (TermMode::DISAMBIGUATE_ESC_CODES, 1u8),
                (TermMode::REPORT_EVENT_TYPES, 2),
                (TermMode::REPORT_ALTERNATE_KEYS, 4),
                (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
                (TermMode::REPORT_ASSOCIATED_TEXT, 16),
            ]
            .iter()
            .filter(|(flag, _)| mode.contains(*flag))
            .map(|(_, bit)| bit)
            .sum(),
        }
    }

    /// Whether the alternate screen is active (full-screen programs).
    pub fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The visible row `row` (0 = top) as cells.
    pub fn row_cells(&self, row: u16) -> Option<Vec<TermCell>> {
        (row < self.rows()).then(|| self.line_cells(Line(row as i32)))
    }

    /// Lines the view is scrolled back into the history (0 = the live screen).
    pub fn view_offset(&self) -> usize {
        self.view_offset
    }

    /// Scrolls the view `lines` into the history (clamped; 0 shows the live screen).
    /// [`snapshot`](Self::snapshot) and the image placements follow it.
    pub fn set_view_offset(&mut self, lines: usize) {
        self.view_offset = lines.min(self.scrollback_len());
    }

    /// Every case-insensitive match of `query` in the history and the screen, as
    /// `(line, column)` where line 0 is the oldest history line, oldest first.
    pub fn search(&self, query: &str) -> Vec<(usize, u16)> {
        let needle: Vec<char> = query.to_lowercase().chars().collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let history = self.scrollback_len();
        let mut found = Vec::new();
        for index in 0..history + self.rows() as usize {
            let text: Vec<char> = self.line_text(Line(index as i32 - history as i32)).to_lowercase().chars().collect();
            if text.len() < needle.len() {
                continue;
            }
            for col in 0..=text.len() - needle.len() {
                if text[col..col + needle.len()] == needle[..] {
                    found.push((index, col as u16));
                }
            }
        }
        found
    }

    /// Scrolls the view so history line `index` (0 = oldest) is visible, near the
    /// middle; returns the row it is shown on.
    pub fn reveal_line(&mut self, index: usize) -> u16 {
        let history = self.scrollback_len() as i64;
        let rows = self.rows() as i64;
        let offset = (history - index as i64 + rows / 2).clamp(0, history);
        self.view_offset = offset as usize;
        (index as i64 - history + offset).clamp(0, rows - 1) as u16
    }

    /// A copy of the visible screen (scrolled back by the view offset), cursor, modes
    /// and title. The cursor is hidden while the view is in the history.
    pub fn snapshot(&self) -> TermSnapshot {
        let rows = self.rows();
        let offset = self.view_offset as i32;
        let mut cursor = self.cursor();
        if offset > 0 {
            cursor.visible = false;
        }
        TermSnapshot {
            cols: self.cols(),
            rows,
            cells: (0..rows as i32)
                .map(|row| self.line_cells(Line(row - offset)))
                .collect(),
            cursor,
            modes: self.modes(),
            title: self.title(),
        }
    }

    /// The visible screen as plain text: one line per row, trailing spaces
    /// trimmed, wide-character spacers skipped.
    pub fn screen_text(&self) -> String {
        (0..self.rows() as i32)
            .map(|row| self.line_text(Line(row)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Number of lines that have scrolled off the top of the primary screen.
    pub fn scrollback_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Scrollback history as plain text, oldest line first.
    pub fn scrollback_lines(&self) -> Vec<String> {
        let history = self.scrollback_len() as i32;
        (1..=history)
            .rev()
            .map(|n| self.line_text(Line(-n)))
            .collect()
    }

    /// History and screen as text with SGR color sequences, oldest line first:
    /// what the terminal shows, without the cursor movement that produced it.
    /// Trailing blank cells and leading/trailing empty lines are dropped. Used to keep
    /// a finished session as a colored command block.
    pub fn text_with_colors(&self) -> String {
        let mut lines = self.scrollback_with_colors();
        lines.extend((0..self.rows()).filter_map(|r| self.row_cells(r)).map(|cells| cells_to_sgr(&cells)));
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        // Clearing the screen pushes blank lines into the history; drop the leading ones.
        let first = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
        lines[first..].join("\n")
    }

    /// Scrollback history with SGR color sequences, oldest line first.
    pub fn scrollback_with_colors(&self) -> Vec<String> {
        (0..self.scrollback_len())
            .filter_map(|i| self.scrollback_row_cells(i))
            .map(|cells| cells_to_sgr(&cells))
            .collect()
    }

    /// Drops the scrollback history (the visible screen is kept).
    pub fn clear_scrollback(&mut self) {
        let dropped = self.scrollback_len();
        self.feed(b"\x1b[3J");
        // Images in the dropped history go with it; the rest move up.
        self.images.retain_mut(|img| {
            let kept = img.line >= dropped;
            img.line = img.line.saturating_sub(dropped);
            kept
        });
    }

    /// One scrollback line as cells; `index` 0 is the oldest line.
    pub fn scrollback_row_cells(&self, index: usize) -> Option<Vec<TermCell>> {
        let history = self.scrollback_len();
        (index < history).then(|| self.line_cells(Line(index as i32 - history as i32)))
    }

    fn line_cells(&self, line: Line) -> Vec<TermCell> {
        let row = &self.term.grid()[line];
        (0..self.term.grid().columns())
            .map(|col| convert_cell(&row[Column(col)]))
            .collect()
    }

    fn line_text(&self, line: Line) -> String {
        let row = &self.term.grid()[line];
        let mut text = String::new();
        for col in 0..self.term.grid().columns() {
            let cell = &row[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
            if let Some(zero_width) = cell.zerowidth() {
                text.extend(zero_width);
            }
        }
        text.truncate(text.trim_end_matches(' ').len());
        text
    }
}

fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(MIN_COLS), rows.max(MIN_ROWS))
}

fn convert_color(color: Color) -> TermColor {
    match color {
        Color::Spec(rgb) => TermColor::Rgb(AnsiColor::new(rgb.r, rgb.g, rgb.b)),
        Color::Indexed(index) => TermColor::Indexed(index),
        Color::Named(named) => match named {
            NamedColor::Background => TermColor::DefaultBg,
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => TermColor::DefaultFg,
            NamedColor::DimBlack => TermColor::Indexed(0),
            NamedColor::DimRed => TermColor::Indexed(1),
            NamedColor::DimGreen => TermColor::Indexed(2),
            NamedColor::DimYellow => TermColor::Indexed(3),
            NamedColor::DimBlue => TermColor::Indexed(4),
            NamedColor::DimMagenta => TermColor::Indexed(5),
            NamedColor::DimCyan => TermColor::Indexed(6),
            NamedColor::DimWhite => TermColor::Indexed(7),
            // Black..=BrightWhite are numbered 0..=15.
            other => TermColor::Indexed(other as usize as u8),
        },
    }
}

fn convert_cell(cell: &Cell) -> TermCell {
    let flags = cell.flags;
    TermCell {
        ch: cell.c,
        zero_width: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        bold: flags.contains(Flags::BOLD),
        dim: flags.contains(Flags::DIM),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::ALL_UNDERLINES),
        strikeout: flags.contains(Flags::STRIKEOUT),
        inverse: flags.contains(Flags::INVERSE),
        hidden: flags.contains(Flags::HIDDEN),
        wide: flags.contains(Flags::WIDE_CHAR),
        wide_spacer: flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_and_scroll_back_through_the_history() {
        let mut term = TerminalEmulator::new(40, 5, 1000);
        for i in 0..50 {
            term.feed(format!("row {i:02}{}\r\n", if i == 7 { " NEEDLE" } else { "" }).as_bytes());
        }
        let found = term.search("needle");
        assert_eq!(found.len(), 1);
        let (line, col) = found[0];
        assert_eq!(col, 7);
        let row = term.reveal_line(line);
        assert!(term.view_offset() > 0);
        let snap = term.snapshot();
        let text: String = snap.cells[row as usize].iter().map(|c| c.ch).collect();
        assert!(text.contains("row 07 NEEDLE"), "{text}");
        assert!(!snap.cursor.visible, "no cursor while looking at the history");
        // Output keeps the view on the same lines.
        let offset = term.view_offset();
        term.feed(b"more\r\n");
        assert_eq!(term.view_offset(), offset + 1);
        term.set_view_offset(0);
        assert!(term.snapshot().cursor.visible);
        assert!(term.search("").is_empty());
    }

    #[test]
    fn programs_can_turn_on_the_kitty_keyboard_protocol() {
        let mut term = TerminalEmulator::new(80, 24, 100);
        assert_eq!(term.modes().kitty_keyboard, 0);
        term.feed(b"\x1b[>1u");
        assert_eq!(term.modes().kitty_keyboard, 1);
        term.feed(b"\x1b[?u");
        let reply = String::from_utf8(term.take_replies()).unwrap();
        assert_eq!(reply, "\x1b[?1u", "the program can ask which flags are on");
        term.feed(b"\x1b[>11u");
        assert_eq!(term.modes().kitty_keyboard, 11);
        term.feed(b"\x1b[<u");
        assert_eq!(term.modes().kitty_keyboard, 1, "popping restores the previous flags");
        term.feed(b"\x1b[<u");
        assert_eq!(term.modes().kitty_keyboard, 0);
    }

    fn png(w: u32, h: u32) -> String {
        let mut out = Vec::new();
        image::RgbaImage::from_pixel(w, h, image::Rgba([9, 9, 9, 255]))
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        crate::shell_integration::base64_encode(&out)
    }

    #[test]
    fn grid_images_are_placed_at_the_cursor_and_scroll_with_the_text() {
        let mut term = TerminalEmulator::new(40, 10, 100);
        term.set_cell_size_px(8, 16);
        // A 16x32 px image is 2x2 cells.
        let seq = format!("line one\r\n\x1b]1337;File=inline=1:{}\x07after", png(16, 32));
        // Split the sequence across feeds.
        let (a, b) = seq.as_bytes().split_at(30);
        term.feed(a);
        term.feed(b);
        let placed = term.images_on_screen();
        assert_eq!(placed.len(), 1);
        assert_eq!((placed[0].row, placed[0].col, placed[0].cols, placed[0].rows), (1, 0, 2, 2));
        assert_eq!(term.cursor().row, 3, "the cursor moved below the image");
        assert!(term.screen_text().contains("after"));
        assert!(!term.screen_text().contains("1337"), "the sequence never reached the grid");
        assert_eq!(term.image(placed[0].id).unwrap().width, 16);
        for i in 0..8 {
            term.feed(format!("\r\nmore {i}").as_bytes());
        }
        let placed = term.images_on_screen();
        assert_eq!(placed[0].row, -1, "scrolled up with the text, partly off screen");
        term.feed(b"\x1b[?1049h");
        assert!(term.images_on_screen().is_empty(), "the alternate screen starts without images");
    }

    fn emu(cols: u16, rows: u16) -> TerminalEmulator {
        TerminalEmulator::new(cols, rows, 100)
    }

    fn lines(e: &TerminalEmulator) -> Vec<String> {
        e.screen_text().split('\n').map(str::to_owned).collect()
    }

    #[test]
    fn text_with_colors_keeps_styles_and_drops_cursor_movement() {
        let mut e = emu(20, 4);
        // Cursor positioning (as ConPTY emits) must not turn into blank lines.
        e.feed(b"\x1b[2J\x1b[H\x1b[31mred\x1b[0m plain\r\n\x1b[1;42mok\x1b[0m");
        assert_eq!(e.text_with_colors(), "\x1b[0;31mred\x1b[0m plain\n\x1b[0;1;42mok\x1b[0m");
        // Stripping the SGR sequences gives the screen text.
        assert_eq!(crate::vt::parse_ansi_text(&e.text_with_colors()).1, "red plain\nok");
    }

    #[test]
    fn clear_scrollback_keeps_the_screen_and_stops_history_pulling_on_grow() {
        let mut e = emu(10, 2);
        e.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(e.scrollback_with_colors(), ["one"]);
        e.clear_scrollback();
        assert_eq!(e.scrollback_len(), 0);
        assert_eq!(lines(&e), ["two", "three"]);
        // Growing now adds blank rows at the bottom instead of pulling history back in,
        // which is how conhost resizes behind ConPTY.
        e.resize(10, 4);
        assert_eq!(lines(&e), ["two", "three", "", ""]);
    }

    #[test]
    fn text_with_colors_includes_scrollback_and_extended_colors() {
        let mut e = emu(10, 2);
        e.feed(b"one\r\ntwo\r\n\x1b[38;5;208mthree\x1b[0m");
        assert_eq!(e.text_with_colors(), "one\ntwo\n\x1b[0;38;5;208mthree\x1b[0m");
        let mut empty = emu(10, 3);
        empty.feed(b"\x1b[2J");
        assert_eq!(empty.text_with_colors(), "");
    }

    #[test]
    fn plain_text_and_newlines() {
        let mut e = emu(10, 3);
        e.feed(b"hello\r\nworld");
        assert_eq!(e.screen_text(), "hello\nworld\n");
        let c = e.cursor();
        assert_eq!((c.row, c.col, c.visible), (1, 5, true));
    }

    #[test]
    fn long_line_wraps() {
        let mut e = emu(5, 3);
        e.feed(b"abcdefgh");
        assert_eq!(lines(&e), ["abcde", "fgh", ""]);
    }

    #[test]
    fn cursor_movement() {
        let mut e = emu(10, 5);
        // CUP to row 3 col 4, write, then relative moves: up, forward, back, down.
        e.feed(b"\x1b[3;4HX\x1b[AY\x1b[2CZ\x1b[5DW\x1b[2BV");
        assert_eq!(lines(&e), ["", "   WY  Z", "   X", "    V", ""]);
        // CHA (column absolute) and VPA (row absolute).
        e.feed(b"\x1b[1G\x1b[1dQ");
        assert_eq!(lines(&e)[0], "Q");
        assert_eq!((e.cursor().row, e.cursor().col), (0, 1));
    }

    #[test]
    fn save_and_restore_cursor() {
        let mut e = emu(10, 3);
        e.feed(b"ab\x1b7\x1b[3;1Hzz\x1b8c");
        assert_eq!(lines(&e), ["abc", "", "zz"]);
    }

    #[test]
    fn clear_screen_and_erase() {
        let mut e = emu(10, 3);
        e.feed(b"one\r\ntwo\r\nthree");
        e.feed(b"\x1b[2J\x1b[H");
        assert_eq!(e.screen_text(), "\n\n");
        assert_eq!((e.cursor().row, e.cursor().col), (0, 0));

        e.feed(b"abcdef\x1b[1;3H\x1b[K");
        assert_eq!(lines(&e)[0], "ab");
        e.feed(b"\x1b[1;6Hxyz\x1b[1;7H\x1b[1K");
        assert_eq!(lines(&e)[0], "       z");
        // ED 0: erase from cursor to end of screen.
        e.feed(b"\x1b[2;1Hrow2\x1b[3;1Hrow3\x1b[2;3H\x1b[J");
        assert_eq!(lines(&e), ["       z", "ro", ""]);
    }

    #[test]
    fn alternate_screen_enter_and_exit() {
        let mut e = emu(20, 4);
        e.feed(b"$ ls\r\nfile.txt\r\n$ ");
        let before = e.screen_text();
        assert!(!e.is_alt_screen());

        e.feed(b"\x1b[?1049h\x1b[H\x1b[2Jfull screen app");
        assert!(e.is_alt_screen());
        assert!(e.modes().alt_screen);
        assert_eq!(lines(&e)[0], "full screen app");

        e.feed(b"\x1b[?1049l");
        assert!(!e.is_alt_screen());
        assert_eq!(e.screen_text(), before);
        // 1049 saves and restores the cursor too.
        assert_eq!((e.cursor().row, e.cursor().col), (2, 2));
    }

    #[test]
    fn scroll_region() {
        let mut e = emu(10, 5);
        e.feed(b"top\r\n1\r\n2\r\n3\r\nbottom");
        // Region rows 2-4; linefeed at its bottom scrolls only inside it.
        e.feed(b"\x1b[2;4r\x1b[4;1H\nnew");
        assert_eq!(lines(&e), ["top", "2", "3", "new", "bottom"]);
        // Reverse index at the region top scrolls the region down.
        e.feed(b"\x1b[2;1H\x1bM");
        assert_eq!(lines(&e), ["top", "", "2", "3", "bottom"]);
        // SU / SD inside the region.
        e.feed(b"\x1b[S");
        assert_eq!(lines(&e), ["top", "2", "3", "", "bottom"]);
        e.feed(b"\x1b[r\x1b[T");
        assert_eq!(lines(&e), ["", "top", "2", "3", ""]);
    }

    #[test]
    fn scrollback_collects_lines_that_scroll_off() {
        let mut e = TerminalEmulator::new(10, 3, 2);
        e.feed(b"l1\r\nl2\r\nl3\r\nl4\r\nl5\r\nl6");
        assert_eq!(lines(&e), ["l4", "l5", "l6"]);
        // Limit is 2 lines of history.
        assert_eq!(e.scrollback_len(), 2);
        assert_eq!(e.scrollback_lines(), ["l2", "l3"]);
        assert_eq!(e.scrollback_row_cells(0).unwrap()[1].ch, '2');
        assert!(e.scrollback_row_cells(2).is_none());
    }

    #[test]
    fn colors_16_256_and_truecolor() {
        let mut e = emu(10, 2);
        e.feed(b"\x1b[31;42ma\x1b[0m");
        e.feed(b"\x1b[91;104mb\x1b[0m");
        e.feed(b"\x1b[38;5;208;48;5;236mc\x1b[0m");
        e.feed(b"\x1b[38;2;10;20;30;48;2;200;100;50md\x1b[0m");
        e.feed(b"e");
        let s = e.snapshot();
        let row = &s.cells[0];
        assert_eq!(
            (row[0].fg, row[0].bg),
            (TermColor::Indexed(1), TermColor::Indexed(2))
        );
        assert_eq!(
            (row[1].fg, row[1].bg),
            (TermColor::Indexed(9), TermColor::Indexed(12))
        );
        assert_eq!(
            (row[2].fg, row[2].bg),
            (TermColor::Indexed(208), TermColor::Indexed(236))
        );
        assert_eq!(row[3].fg, TermColor::Rgb(AnsiColor::new(10, 20, 30)));
        assert_eq!(row[3].bg, TermColor::Rgb(AnsiColor::new(200, 100, 50)));
        assert_eq!(
            (row[4].fg, row[4].bg),
            (TermColor::DefaultFg, TermColor::DefaultBg)
        );
    }

    #[test]
    fn text_attributes() {
        let mut e = emu(10, 1);
        e.feed(b"\x1b[1mB\x1b[22;3mI\x1b[23;4mU\x1b[24;7mR\x1b[27;2;9mD\x1b[0mN");
        let row = e.row_cells(0).unwrap();
        assert!(row[0].bold && !row[0].italic);
        assert!(row[1].italic && !row[1].bold);
        assert!(row[2].underline && !row[2].italic);
        assert!(row[3].inverse && !row[3].underline);
        assert!(row[4].dim && row[4].strikeout && !row[4].inverse);
        let n = &row[5];
        assert!(!(n.bold || n.italic || n.underline || n.inverse || n.dim || n.strikeout));
    }

    #[test]
    fn palette_resolution() {
        assert_eq!(xterm_palette(1), AnsiColor::new(0xcd, 0, 0));
        assert_eq!(xterm_palette(16), AnsiColor::new(0, 0, 0));
        assert_eq!(xterm_palette(196), AnsiColor::new(255, 0, 0));
        assert_eq!(xterm_palette(231), AnsiColor::new(255, 255, 255));
        assert_eq!(xterm_palette(232), AnsiColor::new(8, 8, 8));
        assert_eq!(xterm_palette(255), AnsiColor::new(238, 238, 238));
        let fg = AnsiColor::new(1, 2, 3);
        assert_eq!(TermColor::DefaultFg.to_rgb(fg, DEFAULT_BG), fg);
        assert_eq!(TermColor::DefaultBg.to_rgb(fg, DEFAULT_BG), DEFAULT_BG);
    }

    #[test]
    fn wide_cjk_characters() {
        let mut e = emu(10, 2);
        e.feed("日本語ab".as_bytes());
        assert_eq!(lines(&e)[0], "日本語ab");
        let row = e.row_cells(0).unwrap();
        assert!(row[0].wide && row[0].ch == '日');
        assert!(row[1].wide_spacer);
        assert!(row[4].wide && row[4].ch == '語');
        assert_eq!(row[6].ch, 'a');
        assert_eq!(e.cursor().col, 8);
    }

    #[test]
    fn wide_character_wraps_instead_of_splitting() {
        let mut e = emu(5, 2);
        e.feed("abcd中".as_bytes());
        assert_eq!(lines(&e), ["abcd", "中"]);
        assert!(e.row_cells(1).unwrap()[0].wide);
    }

    #[test]
    fn combining_characters_stay_in_one_cell() {
        let mut e = emu(5, 1);
        e.feed("e\u{301}x".as_bytes());
        let row = e.row_cells(0).unwrap();
        assert_eq!(row[0].zero_width, vec!['\u{301}']);
        assert_eq!(row[1].ch, 'x');
        assert_eq!(e.screen_text(), "e\u{301}x");
    }

    #[test]
    fn resize_changes_grid_and_keeps_content() {
        let mut e = emu(10, 3);
        e.feed(b"abc\r\ndef");
        e.resize(20, 6);
        assert_eq!((e.cols(), e.rows()), (20, 6));
        let s = e.snapshot();
        assert_eq!((s.cols, s.rows), (20, 6));
        assert_eq!(s.cells.len(), 6);
        assert!(s.cells.iter().all(|r| r.len() == 20));
        assert!(e.screen_text().starts_with("abc\ndef"));

        // Shrinking rows pushes the top into scrollback so the cursor line stays.
        e.feed(b"\r\nghi\r\njkl");
        e.resize(20, 2);
        assert_eq!(lines(&e), ["ghi", "jkl"]);
        assert_eq!(e.scrollback_lines(), ["abc", "def"]);

        // Sizes are clamped to the minimum.
        e.resize(0, 0);
        assert_eq!((e.cols(), e.rows()), (MIN_COLS, MIN_ROWS));
    }

    #[test]
    fn resize_is_reported_to_size_queries() {
        let mut e = emu(80, 24);
        e.resize(100, 30);
        e.feed(b"\x1b[18t");
        assert_eq!(e.take_replies(), b"\x1b[8;30;100t");
        e.set_cell_size_px(10, 20);
        e.feed(b"\x1b[14t");
        assert_eq!(e.take_replies(), b"\x1b[4;600;1000t");
    }

    #[test]
    fn replies_for_device_attributes_and_cursor_reports() {
        let mut e = emu(20, 5);
        e.feed(b"\x1b[c");
        assert_eq!(e.take_replies(), b"\x1b[?6c");
        // Drained: nothing left the second time.
        assert!(e.take_replies().is_empty());

        e.feed(b"\x1b[3;7H\x1b[6n");
        assert_eq!(e.take_replies(), b"\x1b[3;7R");
        e.feed(b"\x1b[5n");
        assert_eq!(e.take_replies(), b"\x1b[0n");

        // Secondary DA starts with ESC [ > and ends in c.
        e.feed(b"\x1b[>c");
        let reply = String::from_utf8(e.take_replies()).unwrap();
        assert!(
            reply.starts_with("\x1b[>0;") && reply.ends_with('c'),
            "{reply:?}"
        );
    }

    #[test]
    fn color_queries_get_answered() {
        let mut e = emu(10, 2);
        e.feed(b"\x1b]11;?\x07");
        let reply = String::from_utf8(e.take_replies()).unwrap();
        assert!(reply.starts_with("\x1b]11;rgb:1e1e/1e1e/1e1e"), "{reply:?}");
        // Palette query: OSC 4 ; index ; ?
        e.feed(b"\x1b]4;1;?\x07");
        let reply = String::from_utf8(e.take_replies()).unwrap();
        assert!(reply.starts_with("\x1b]4;1;rgb:cdcd/0000/0000"), "{reply:?}");
    }

    #[test]
    fn clipboard_escape_is_ignored() {
        let mut e = emu(10, 2);
        // OSC 52 read request must not produce a reply.
        e.feed(b"\x1b]52;c;?\x07");
        assert!(e.take_replies().is_empty());
    }

    #[test]
    fn title_and_bell() {
        let mut e = emu(10, 2);
        assert_eq!(e.title(), None);
        e.feed(b"\x1b]0;my title\x07");
        assert_eq!(e.title().as_deref(), Some("my title"));
        e.feed(b"\x1b]2;other\x1b\\");
        assert_eq!(e.snapshot().title.as_deref(), Some("other"));
        assert!(!e.take_bell());
        e.feed(b"\x07");
        assert!(e.take_bell());
        assert!(!e.take_bell());
    }

    #[test]
    fn input_modes() {
        let mut e = emu(10, 2);
        assert_eq!(e.modes().mouse, MouseMode::None);
        assert!(!e.modes().app_cursor);
        e.feed(b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1004h");
        let m = e.modes();
        assert!(m.app_cursor && m.app_keypad && m.bracketed_paste);
        assert!(m.sgr_mouse && m.focus_reporting);
        assert_eq!(m.mouse, MouseMode::Click);
        e.feed(b"\x1b[?1002h");
        assert_eq!(e.modes().mouse, MouseMode::Drag);
        e.feed(b"\x1b[?1003h");
        assert_eq!(e.modes().mouse, MouseMode::Motion);
        e.feed(b"\x1b[?1l\x1b>\x1b[?2004l\x1b[?1003l\x1b[?1002l\x1b[?1000l");
        let m = e.modes();
        assert!(!m.app_cursor && !m.app_keypad && !m.bracketed_paste);
        assert_eq!(m.mouse, MouseMode::None);
    }

    #[test]
    fn cursor_visibility_and_shape() {
        let mut e = emu(10, 2);
        e.feed(b"\x1b[?25l");
        assert!(!e.cursor().visible);
        e.feed(b"\x1b[?25h\x1b[6 q");
        assert!(e.cursor().visible);
        assert_eq!(e.cursor().shape, TermCursorShape::Beam);
        e.feed(b"\x1b[4 q");
        assert_eq!(e.cursor().shape, TermCursorShape::Underline);
    }

    #[test]
    fn split_escape_sequences_across_feeds() {
        let mut e = emu(10, 2);
        for byte in b"\x1b[31mred\x1b[0m" {
            e.feed(std::slice::from_ref(byte));
        }
        assert_eq!(e.screen_text(), "red\n");
        assert_eq!(e.row_cells(0).unwrap()[0].fg, TermColor::Indexed(1));
        // A UTF-8 character split in the middle.
        let bytes = "é".as_bytes();
        e.feed(&bytes[..1]);
        e.feed(&bytes[1..]);
        assert_eq!(lines(&e)[0], "redé");
    }

    #[test]
    fn synchronized_update_is_applied_on_end_or_flush() {
        let mut e = emu(20, 2);
        e.feed(b"\x1b[?2026hhidden");
        assert!(e.sync_deadline().is_some());
        assert_eq!(e.screen_text(), "\n");
        e.feed(b"\x1b[?2026l");
        assert_eq!(e.screen_text(), "hidden\n");
        assert!(e.sync_deadline().is_none());

        e.feed(b"\x1b[?2026h more");
        e.flush_sync();
        assert_eq!(e.screen_text(), "hidden more\n");
    }

    /// A vim-like session on a 40x6 terminal: enter the alternate screen,
    /// set modes, draw buffer lines, tildes and a reverse-video status line,
    /// move the cursor, then quit and restore the shell screen.
    const VIM_LIKE_SESSION: &[u8] = b"\
\x1b[?1049h\x1b[22;0;0t\x1b[>4;2m\x1b[?1h\x1b=\x1b[H\x1b[2J\x1b[?2004h\x1b[?1004h\
\x1b]2;notes.txt - VIM\x07\x1b[?25l\
\x1b[1;1Hfn main() {\x1b[K\
\x1b[2;1H    println!(\x1b[31m\"hi\"\x1b[m);\x1b[K\
\x1b[3;1H}\x1b[K\
\x1b[4;1H\x1b[94m~\x1b[m\x1b[K\
\x1b[5;1H\x1b[94m~\x1b[m\x1b[K\
\x1b[6;1H\x1b[7m\"notes.txt\" 3L, 38B\x1b[27m\x1b[6;30H1,1\x1b[6;38HAll\
\x1b[2;5H\x1b[?25h\x1b[c";

    const VIM_LIKE_EXIT: &[u8] =
        b"\x1b[?25l\x1b[?1004l\x1b[?2004l\x1b[6;1H\x1b[K\x1b[?1l\x1b>\x1b[?25h\x1b[?1049l\x1b[23;0;0t";

    #[test]
    fn vim_like_byte_stream() {
        let mut e = emu(40, 6);
        e.feed(b"$ vim notes.txt\r\n");
        e.feed(VIM_LIKE_SESSION);

        assert_eq!(
            lines(&e),
            [
                "fn main() {",
                "    println!(\"hi\");",
                "}",
                "~",
                "~",
                "\"notes.txt\" 3L, 38B          1,1     All",
            ]
        );
        let s = e.snapshot();
        assert_eq!(s.title.as_deref(), Some("notes.txt - VIM"));
        assert!(s.modes.alt_screen && s.modes.app_cursor && s.modes.app_keypad);
        assert!(s.modes.bracketed_paste && s.modes.focus_reporting);
        assert_eq!((s.cursor.row, s.cursor.col, s.cursor.visible), (1, 4, true));
        // "hi" literal is red, tildes bright blue, status line inverse.
        assert_eq!(s.cell(1, 13).unwrap().fg, TermColor::Indexed(1));
        assert_eq!(s.cell(1, 16).unwrap().fg, TermColor::Indexed(1));
        assert_eq!(s.cell(1, 17).unwrap().fg, TermColor::DefaultFg);
        assert_eq!(s.cell(3, 0).unwrap().fg, TermColor::Indexed(12));
        assert!(s.cell(5, 0).unwrap().inverse);
        assert!(!s.cell(5, 29).unwrap().inverse);
        // vim's DA probe gets answered.
        assert_eq!(e.take_replies(), b"\x1b[?6c");

        e.feed(VIM_LIKE_EXIT);
        assert!(!e.is_alt_screen());
        let m = e.modes();
        assert!(!m.app_cursor && !m.bracketed_paste && !m.focus_reporting);
        assert_eq!(lines(&e)[0], "$ vim notes.txt");
        assert_eq!((e.cursor().row, e.cursor().col), (1, 0));
    }

    /// An htop-like frame: alternate screen, mouse reporting, colored meter
    /// bars drawn with absolute positioning, a header row on a colored
    /// background and a scroll region for the process list.
    const HTOP_LIKE_FRAME: &[u8] = b"\
\x1b[?1049h\x1b[1;10r\x1b(B\x1b[m\x1b[4l\x1b[?7h\x1b[?1h\x1b=\x1b[?25l\x1b[39;49m\
\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[H\x1b[2J\
\x1b[1;3H\x1b[36m1\x1b[39m\x1b[1m[\x1b[32m||||\x1b[31m||\x1b[90m      \x1b[39m 25.0%]\x1b[m\
\x1b[2;3H\x1b[36mMem\x1b[1;39m[\x1b[32m|||\x1b[34m|\x1b[33m|\x1b[90m      \x1b[39m1.2G/8G]\x1b[m\
\x1b[4;1H\x1b[30;42m  PID USER      CPU% Command          \x1b[m\
\x1b[5;9r\
\x1b[5;1H\x1b[30;46m    1 root       0.0 init             \x1b[m\
\x1b[6;1H  412 user      12.5 \x1b[1mfirefox\x1b[m\
\x1b[7;1H  977 user       3.1 htop\
\x1b[1;10r\
\x1b[10;1H\x1b[30;46mF1\x1b[39;49mHelp  \x1b[30;46mF10\x1b[39;49mQuit\x1b[K";

    #[test]
    fn htop_like_byte_stream() {
        let mut e = emu(38, 10);
        e.feed(HTOP_LIKE_FRAME);
        assert_eq!(
            lines(&e),
            [
                "  1[||||||       25.0%]",
                "  Mem[|||||      1.2G/8G]",
                "",
                "  PID USER      CPU% Command",
                "    1 root       0.0 init",
                "  412 user      12.5 firefox",
                "  977 user       3.1 htop",
                "",
                "",
                "F1Help  F10Quit",
            ]
        );
        let s = e.snapshot();
        assert!(s.modes.alt_screen);
        assert_eq!(s.modes.mouse, MouseMode::Drag);
        assert!(s.modes.sgr_mouse);
        assert!(!s.cursor.visible);
        // Meter bar colors and the bold bracket.
        assert_eq!(s.cell(0, 2).unwrap().fg, TermColor::Indexed(6));
        assert!(s.cell(0, 3).unwrap().bold);
        assert_eq!(s.cell(0, 4).unwrap().fg, TermColor::Indexed(2));
        assert_eq!(s.cell(0, 8).unwrap().fg, TermColor::Indexed(1));
        // The header keeps its background across the full padded width.
        assert_eq!(s.cell(3, 0).unwrap().bg, TermColor::Indexed(2));
        assert_eq!(s.cell(3, 36).unwrap().bg, TermColor::Indexed(2));
        assert_eq!(s.cell(4, 0).unwrap().bg, TermColor::Indexed(6));
        assert!(s.cell(5, 21).unwrap().bold);
        assert!(!s.cell(5, 20).unwrap().bold);

        // A new process scrolls inside the list region only (rows 5-9).
        e.feed(b"\x1b[5;9r\x1b[9;1H\n 1200 user       0.5 bash\x1b[1;10r");
        let l = lines(&e);
        assert_eq!(l[3], "  PID USER      CPU% Command");
        assert_eq!(l[4], "  412 user      12.5 firefox");
        assert_eq!(l[8], " 1200 user       0.5 bash");
        assert_eq!(l[9], "F1Help  F10Quit");
        // Alternate screen output never lands in scrollback.
        assert_eq!(e.scrollback_len(), 0);
    }
}
