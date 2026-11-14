use serde::{Deserialize, Serialize};
use vte::{Params, Perform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnsiColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl AnsiColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnsiSpan {
    pub text: String,
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Default for AnsiSpan {
    fn default() -> Self {
        Self {
            text: String::new(),
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
        }
    }
}

pub struct VtParserState {
    pub spans: Vec<AnsiSpan>,
    current: AnsiSpan,
    pub plain_text: String,
}

impl Default for VtParserState {
    fn default() -> Self {
        Self {
            spans: Vec::new(),
            current: AnsiSpan::default(),
            plain_text: String::new(),
        }
    }
}

impl VtParserState {
    fn flush_current(&mut self) {
        if !self.current.text.is_empty() {
            self.spans.push(self.current.clone());
            self.current.text.clear();
        }
    }

    fn standard_color(idx: u16, bright: bool) -> AnsiColor {
        match (idx, bright) {
            (0, false) => AnsiColor::new(40, 44, 52),
            (1, false) => AnsiColor::new(224, 108, 117),
            (2, false) => AnsiColor::new(152, 195, 121),
            (3, false) => AnsiColor::new(229, 192, 123),
            (4, false) => AnsiColor::new(97, 175, 239),
            (5, false) => AnsiColor::new(198, 120, 221),
            (6, false) => AnsiColor::new(86, 182, 194),
            (7, false) => AnsiColor::new(171, 178, 191),
            (0, true) => AnsiColor::new(92, 99, 112),
            (1, true) => AnsiColor::new(244, 128, 137),
            (2, true) => AnsiColor::new(172, 215, 141),
            (3, true) => AnsiColor::new(249, 212, 143),
            (4, true) => AnsiColor::new(117, 195, 255),
            (5, true) => AnsiColor::new(218, 140, 241),
            (6, true) => AnsiColor::new(106, 202, 214),
            _ => AnsiColor::new(255, 255, 255),
        }
    }
}

impl Perform for VtParserState {
    fn print(&mut self, c: char) {
        self.current.text.push(c);
        self.plain_text.push(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                self.current.text.push('\n');
                self.plain_text.push('\n');
            }
            b'\t' => {
                self.current.text.push_str("    ");
                self.plain_text.push_str("    ");
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        if action == 'm' {
            self.flush_current();
            let mut iter = params.iter();
            let mut any = false;
            while let Some(sub) = iter.next() {
                any = true;
                let code = sub.first().copied().unwrap_or(0);
                match code {
                    0 => {
                        self.current.fg = None;
                        self.current.bg = None;
                        self.current.bold = false;
                        self.current.italic = false;
                        self.current.underline = false;
                    }
                    1 => self.current.bold = true,
                    3 => self.current.italic = true,
                    4 => self.current.underline = true,
                    22 => self.current.bold = false,
                    23 => self.current.italic = false,
                    24 => self.current.underline = false,
                    30..=37 => {
                        self.current.fg = Some(Self::standard_color(code - 30, false));
                    }
                    39 => self.current.fg = None,
                    40..=47 => {
                        self.current.bg = Some(Self::standard_color(code - 40, false));
                    }
                    49 => self.current.bg = None,
                    90..=97 => {
                        self.current.fg = Some(Self::standard_color(code - 90, true));
                    }
                    100..=107 => {
                        self.current.bg = Some(Self::standard_color(code - 100, true));
                    }
                    38 | 48 => {
                        let is_fg = code == 38;
                        if let Some(mode_slice) = iter.next() {
                            let mode = mode_slice.first().copied().unwrap_or(0);
                            if mode == 2 {
                                let r = iter.next().and_then(|s| s.first()).copied().unwrap_or(255) as u8;
                                let g = iter.next().and_then(|s| s.first()).copied().unwrap_or(255) as u8;
                                let b = iter.next().and_then(|s| s.first()).copied().unwrap_or(255) as u8;
                                if is_fg {
                                    self.current.fg = Some(AnsiColor::new(r, g, b));
                                } else {
                                    self.current.bg = Some(AnsiColor::new(r, g, b));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !any {
                self.current.fg = None;
                self.current.bg = None;
                self.current.bold = false;
                self.current.italic = false;
                self.current.underline = false;
            }
        }
    }
}

/// Parses an ANSI-escaped string into styled spans and stripped plain text.
pub fn parse_ansi_text(input: &str) -> (Vec<AnsiSpan>, String) {
    let mut parser = vte::Parser::new();
    let mut state = VtParserState::default();
    for byte in input.as_bytes() {
        parser.advance(&mut state, *byte);
    }
    state.flush_current();
    (state.spans, state.plain_text)
}
