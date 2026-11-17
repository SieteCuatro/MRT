//! Inline images in the terminal grid (full-screen programs and `:tui`).
//!
//! `alacritty_terminal` has no graphics support, so [`GraphicsSplitter`] takes image
//! sequences (iTerm2 `OSC 1337 ; File=`, kitty `ESC _ G`, Sixel `ESC P … q`) out of
//! the byte stream before the emulator sees it. The emulator decodes them with the
//! block parser's decoders, anchors each image at the cursor's line counted from
//! the top of the scrollback, and moves the cursor below it, as iTerm2 does, so the
//! image scrolls with the text around it.

/// A piece of PTY output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Bytes for the emulator.
    Text(Vec<u8>),
    /// One complete image sequence, introducer and terminator included.
    Graphic(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Osc,
    Dcs,
    Apc,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    /// Inside a string sequence; `esc` is set after an ESC that may start the ST.
    String { kind: Kind, esc: bool },
}

/// Largest string sequence kept whole; longer ones are passed on as text.
const MAX_SEQUENCE: usize = crate::images::MAX_IMAGE_PAYLOAD / 3 * 4 + 1024;

/// Splits a byte stream into text and image sequences, across chunk boundaries.
#[derive(Debug, Default)]
pub struct GraphicsSplitter {
    state: State,
    /// The sequence being collected (from its ESC).
    buf: Vec<u8>,
}

fn is_graphic(kind: Kind, seq: &[u8]) -> bool {
    // seq starts with ESC and the introducer byte.
    let body = &seq[2.min(seq.len())..];
    match kind {
        Kind::Osc => body.starts_with(b"1337;File="),
        Kind::Apc => body.first() == Some(&b'G'),
        // DCS: parameters (digits and ';') then the final byte `q`.
        Kind::Dcs => {
            let params = body.iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
            body.get(params) == Some(&b'q')
        }
    }
}

impl GraphicsSplitter {
    pub fn split(&mut self, bytes: &[u8]) -> Vec<Segment> {
        let mut out: Vec<Segment> = Vec::new();
        let mut text: Vec<u8> = Vec::new();
        for &byte in bytes {
            match std::mem::take(&mut self.state) {
                State::Ground => {
                    if byte == 0x1b {
                        self.buf.clear();
                        self.buf.push(byte);
                        self.state = State::Escape;
                    } else {
                        text.push(byte);
                    }
                }
                State::Escape => {
                    let kind = match byte {
                        b']' => Some(Kind::Osc),
                        b'P' => Some(Kind::Dcs),
                        b'_' => Some(Kind::Apc),
                        _ => None,
                    };
                    match kind {
                        Some(kind) => {
                            self.buf.push(byte);
                            self.state = State::String { kind, esc: false };
                        }
                        None => {
                            text.extend_from_slice(&self.buf);
                            self.buf.clear();
                            if byte == 0x1b {
                                self.buf.push(byte);
                                self.state = State::Escape;
                            } else {
                                text.push(byte);
                            }
                        }
                    }
                }
                State::String { kind, esc } => {
                    self.buf.push(byte);
                    let ended = (esc && byte == b'\\') || (kind == Kind::Osc && byte == 0x07);
                    if ended {
                        let seq = std::mem::take(&mut self.buf);
                        if is_graphic(kind, &seq) {
                            if !text.is_empty() {
                                out.push(Segment::Text(std::mem::take(&mut text)));
                            }
                            out.push(Segment::Graphic(seq));
                        } else {
                            text.extend_from_slice(&seq);
                        }
                    } else if self.buf.len() > MAX_SEQUENCE {
                        // Too long to be an image we accept: let the emulator have it.
                        text.extend_from_slice(&std::mem::take(&mut self.buf));
                    } else {
                        self.state = State::String { kind, esc: byte == 0x1b };
                    }
                }
            }
        }
        if !text.is_empty() {
            out.push(Segment::Text(text));
        }
        out
    }
}

/// An image placed in the grid.
#[derive(Debug, Clone)]
pub struct GridImage {
    /// Unique per emulator, for texture caching.
    pub id: u64,
    pub image: crate::images::InlineImage,
    /// Line of the image's top row, counted from the oldest scrollback line.
    pub line: usize,
    pub col: u16,
    /// Size in cells.
    pub cols: u16,
    pub rows: u16,
}

/// Where an image is on the visible screen (its top row may be above the screen).
#[derive(Debug, Clone, PartialEq)]
pub struct GridImagePlacement {
    pub id: u64,
    pub row: i32,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
}

/// Cells an image covers: the requested size, else its pixel size over the cell
/// size, limited to the room right of `col` (keeping the aspect ratio).
pub fn image_cells(image: &crate::images::InlineImage, col: u16, cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> (u16, u16) {
    use crate::images::ImageSize;
    let (cw, ch) = (cell_w.max(1) as f32, cell_h.max(1) as f32);
    let room = cols.saturating_sub(col).max(1) as f32;
    let natural_w = image.width as f32 / cw;
    let aspect = image.height as f32 / image.width.max(1) as f32;
    let mut width = match image.requested_width {
        Some(ImageSize::Cells(n)) => n as f32,
        Some(ImageSize::Pixels(px)) => px as f32 / cw,
        Some(ImageSize::Percent(p)) => room * p as f32 / 100.0,
        None => natural_w,
    };
    let mut height = match image.requested_height {
        Some(ImageSize::Cells(n)) => n as f32,
        Some(ImageSize::Pixels(px)) => px as f32 / ch,
        Some(ImageSize::Percent(p)) => rows as f32 * p as f32 / 100.0,
        None => width * cw * aspect / ch,
    };
    if image.requested_height.is_some() && image.requested_width.is_none() {
        width = height * ch / aspect.max(0.0001) / cw;
    }
    if width > room {
        height *= room / width;
        width = room;
    }
    (width.ceil().clamp(1.0, room) as u16, height.ceil().clamp(1.0, 1000.0) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_all(chunks: &[&[u8]]) -> Vec<Segment> {
        let mut splitter = GraphicsSplitter::default();
        let mut out: Vec<Segment> = Vec::new();
        for chunk in chunks {
            for seg in splitter.split(chunk) {
                match (out.last_mut(), seg) {
                    (Some(Segment::Text(prev)), Segment::Text(more)) => prev.extend(more),
                    (_, seg) => out.push(seg),
                }
            }
        }
        out
    }

    #[test]
    fn takes_image_sequences_out_and_keeps_everything_else() {
        let input: &[u8] = b"a\x1b[31mb\x1b]0;title\x07c\x1b]1337;File=inline=1:QQ==\x07d\x1b_Ga=T;AA\x1b\\e\x1bP0;1q#1~\x1b\\f\x1bP$q\x1b\\";
        let expected = vec![
            Segment::Text(b"a\x1b[31mb\x1b]0;title\x07c".to_vec()),
            Segment::Graphic(b"\x1b]1337;File=inline=1:QQ==\x07".to_vec()),
            Segment::Text(b"d".to_vec()),
            Segment::Graphic(b"\x1b_Ga=T;AA\x1b\\".to_vec()),
            Segment::Text(b"e".to_vec()),
            Segment::Graphic(b"\x1bP0;1q#1~\x1b\\".to_vec()),
            Segment::Text(b"f\x1bP$q\x1b\\".to_vec()),
        ];
        assert_eq!(split_all(&[input]), expected);
        // Byte by byte gives the same result.
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(split_all(&bytes), expected);
    }

    #[test]
    fn image_cells_fit_the_room() {
        let image = |w, h, rw| crate::images::InlineImage {
            width: w,
            height: h,
            rgba: std::sync::Arc::new(Vec::new()),
            anchor: 0,
            requested_width: rw,
            requested_height: None,
        };
        assert_eq!(image_cells(&image(80, 64, None), 0, 80, 24, 8, 16), (10, 4));
        assert_eq!(image_cells(&image(1600, 160, None), 40, 80, 24, 8, 16), (40, 2), "limited to the room, aspect kept");
        assert_eq!(image_cells(&image(80, 80, Some(crate::images::ImageSize::Cells(20))), 0, 80, 24, 8, 16), (20, 10));
    }
}
