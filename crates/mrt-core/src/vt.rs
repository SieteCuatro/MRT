use serde::{Deserialize, Serialize};
use crate::images::{ApcScanner, DecodedImage, InlineImage, KittyGraphics, SixelDecoder};
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
    /// Target URI of an OSC 8 hyperlink covering this span, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
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
            link: None,
        }
    }
}

/// Side-channel events carried by OSC sequences in command output.
///
/// They are reported to the caller and never applied automatically: the GUI
/// decides what to do with them (OSC 52 always needs the user's approval).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VtEvent {
    /// OSC 7: the shell reported its working directory (`file://host/path`).
    WorkingDirectory {
        /// Host part of the URI (empty for `file:///path`).
        host: String,
        /// Percent-decoded local path (`/C:/x` becomes `C:/x`).
        path: String,
    },
    /// OSC 52: a program asked to write `text` to the clipboard.
    ClipboardWrite {
        /// Selection targets as sent (`c`, `p`, `s`, `0`..`7`; empty means `s0`).
        selection: String,
        /// Decoded clipboard payload.
        text: String,
    },
}

/// Result of [`parse_ansi_rich`]: styled spans, stripped text and OSC events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedAnsi {
    pub spans: Vec<AnsiSpan>,
    pub plain_text: String,
    pub events: Vec<VtEvent>,
    /// Inline images (iTerm2 OSC 1337, kitty graphics, Sixel), anchored in `plain_text`.
    pub images: Vec<InlineImage>,
}

/// Largest OSC 52 payload (decoded bytes) MRT will offer to put on the clipboard.
pub const MAX_CLIPBOARD_WRITE_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct VtParserState {
    pub spans: Vec<AnsiSpan>,
    current: AnsiSpan,
    pub plain_text: String,
    pub events: Vec<VtEvent>,
    pub images: Vec<InlineImage>,
    apc: ApcScanner,
    kitty: KittyGraphics,
    sixel: Option<SixelDecoder>,
}

impl VtParserState {
    /// Feeds bytes to `parser` and to the APC scanner (kitty graphics), which
    /// `vte` does not report.
    fn advance(&mut self, parser: &mut vte::Parser, bytes: &[u8]) {
        for &byte in bytes {
            if let Some(apc) = self.apc.advance(byte) {
                if let Some(result) = self.kitty.handle_apc(&apc) {
                    self.add_image(result);
                }
            }
            parser.advance(self, byte);
        }
    }

    /// Anchors a decoded image at the current end of the text, or writes why it
    /// could not be shown.
    fn add_image(&mut self, result: Result<DecodedImage, String>) {
        match result {
            Ok(image) => {
                let anchor = self.plain_text.len();
                self.images.push(image.anchored(anchor));
            }
            Err(reason) => {
                let note = format!("[image not shown: {reason}]\n");
                self.current.text.push_str(&note);
                self.plain_text.push_str(&note);
            }
        }
    }

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

impl VtParserState {
    fn handle_osc(&mut self, params: &[&[u8]]) {
        let Some(code) = params.first() else { return };
        // vte splits OSC data on ';'. Payloads such as URLs may contain ';',
        // so rejoin everything after the fixed parameters.
        let rest = |from: usize| -> String {
            let joined: Vec<u8> = params.get(from..).unwrap_or(&[]).join(&b';');
            String::from_utf8_lossy(&joined).into_owned()
        };
        match *code {
            b"7" => {
                if let Some((host, path)) = parse_osc7_parts(&rest(1)) {
                    self.events.push(VtEvent::WorkingDirectory { host, path });
                }
            }
            b"8" => {
                // OSC 8 ; params ; URI. An empty URI closes the hyperlink.
                if params.len() < 3 {
                    return;
                }
                let uri = rest(2);
                let uri = uri.trim();
                self.flush_current();
                self.current.link = if uri.is_empty() || uri.chars().any(char::is_control) {
                    None
                } else {
                    Some(uri.to_string())
                };
            }
            b"1337" => {
                if let Some(result) = crate::images::parse_iterm_file(&rest(1)) {
                    self.add_image(result);
                }
            }
            b"52" => {
                if params.len() < 3 {
                    return;
                }
                let selection = String::from_utf8_lossy(params[1]).into_owned();
                let data = rest(2);
                // `?` is a clipboard read request; MRT never answers those.
                if data == "?" {
                    return;
                }
                if let Some(bytes) = decode_base64(data.as_bytes()) {
                    if bytes.len() <= MAX_CLIPBOARD_WRITE_BYTES {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        self.events.push(VtEvent::ClipboardWrite { selection, text });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Parses an OSC 7 `file://host/path` URI into the local path it names.
/// Percent-escapes are decoded and `/C:/x` becomes `C:/x` on any OS.
pub fn parse_osc7_uri(uri: &str) -> Option<std::path::PathBuf> {
    parse_osc7_parts(uri).map(|(_, path)| std::path::PathBuf::from(path))
}

/// Parses an OSC 7 `file://host/path` URI into `(host, decoded_path)`.
fn parse_osc7_parts(uri: &str) -> Option<(String, String)> {
    let rest = uri.trim().strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = rest[..slash].to_string();
    let mut path = percent_decode(&rest[slash..])?;
    // Windows shells report `file://host/C:/Users/...`.
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        path.remove(0);
    }
    if path.chars().any(char::is_control) {
        return None;
    }
    Some((host, path))
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Decodes standard (RFC 4648) base64, ignoring ASCII whitespace.
/// Returns `None` for any other invalid character or a malformed length.
pub(crate) fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
    let mut end = clean.len();
    while end > 0 && clean[end - 1] == b'=' && clean.len() - end < 2 {
        end -= 1;
    }
    if end != clean.len() && clean.len() % 4 != 0 {
        return None;
    }
    let data = &clean[..end];
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= value(c)? << (18 - 6 * i as u32);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

impl Perform for VtParserState {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.handle_osc(params);
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // DCS P1 ; P2 ; P3 q starts a Sixel image.
        if action == 'q' && intermediates.is_empty() {
            let p2 = params.iter().nth(1).and_then(|p| p.first().copied()).unwrap_or(0);
            self.sixel = Some(SixelDecoder::new(p2));
        }
    }

    fn put(&mut self, byte: u8) {
        if let Some(sixel) = self.sixel.as_mut() {
            sixel.put(byte);
        }
    }

    fn unhook(&mut self) {
        if let Some(sixel) = self.sixel.take() {
            let result = sixel.finish();
            self.add_image(result);
        }
    }

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
    let parsed = parse_ansi_rich(input);
    (parsed.spans, parsed.plain_text)
}

/// Like [`parse_ansi_text`], but also returns OSC 7 / OSC 52 events.
/// OSC 8 hyperlinks are attached to the spans they cover ([`AnsiSpan::link`]).
pub fn parse_ansi_rich(input: &str) -> ParsedAnsi {
    let mut parser = vte::Parser::new();
    let mut state = VtParserState::default();
    state.advance(&mut parser, input.as_bytes());
    state.flush_current();
    ParsedAnsi {
        spans: state.spans,
        plain_text: state.plain_text,
        events: state.events,
        images: state.images,
    }
}

/// Incremental parser for output that arrives in chunks.
///
/// [`parse_ansi_rich`] parses a whole string, so re-parsing all output after
/// every chunk costs O(n²) while a command streams. A `VtStream` keeps the
/// parser state between chunks (an escape sequence split across two chunks is
/// still recognised) and returns only what the new chunk added.
pub struct VtStream {
    parser: vte::Parser,
    state: VtParserState,
}

impl Default for VtStream {
    fn default() -> Self {
        Self::new()
    }
}

impl VtStream {
    pub fn new() -> Self {
        Self {
            parser: vte::Parser::new(),
            state: VtParserState::default(),
        }
    }

    /// Parses `chunk` and returns the spans, plain text and events it produced.
    ///
    /// Text after the last style change is returned as a finished span too, so a
    /// long run in one style is not copied again on every chunk; the next chunk
    /// starts a new span with the same style. Concatenating the returned spans of
    /// every call gives the same text as [`parse_ansi_rich`] on the whole input.
    pub fn feed(&mut self, chunk: &str) -> ParsedAnsi {
        self.state.advance(&mut self.parser, chunk.as_bytes());
        self.state.flush_current();
        // Anchors are offsets in the plain text of this chunk.
        ParsedAnsi {
            spans: std::mem::take(&mut self.state.spans),
            plain_text: std::mem::take(&mut self.state.plain_text),
            events: std::mem::take(&mut self.state.events),
            images: std::mem::take(&mut self.state.images),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ST: &str = "\x1b\\";

    #[test]
    fn stream_matches_whole_parse_across_split_escapes() {
        let input = "a\x1b[31mred\x1b[0m plain\n\x1b]8;;https://x.io\x1b\\link\x1b]8;;\x1b\\ \x1b]7;file:///tmp\x07end";
        let whole = parse_ansi_rich(input);
        // Feed one byte-ish piece at a time (on char boundaries) so every escape is split.
        let mut stream = VtStream::new();
        let mut spans = Vec::new();
        let mut plain = String::new();
        let mut events = Vec::new();
        for (i, ch) in input.char_indices() {
            let piece = &input[i..i + ch.len_utf8()];
            let delta = stream.feed(piece);
            spans.extend(delta.spans);
            plain.push_str(&delta.plain_text);
            events.extend(delta.events);
        }
        assert_eq!(plain, whole.plain_text);
        assert_eq!(events, whole.events);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, whole.plain_text);
        let red: String = spans.iter().filter(|s| s.fg.is_some()).map(|s| s.text.as_str()).collect();
        assert_eq!(red, "red");
        let linked: String = spans.iter().filter(|s| s.link.is_some()).map(|s| s.text.as_str()).collect();
        assert_eq!(linked, "link");
    }

    #[test]
    fn plain_parse_is_unchanged() {
        let (spans, plain) = parse_ansi_text("\x1b[31mred\x1b[0m ok\n");
        assert_eq!(plain, "red ok\n");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "red");
        assert!(spans[0].fg.is_some());
        assert!(spans.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn osc7_bel_and_st() {
        let bel = parse_ansi_rich("\x1b]7;file://box/home/me/my%20dir\x07$ ");
        assert_eq!(
            bel.events,
            vec![VtEvent::WorkingDirectory { host: "box".into(), path: "/home/me/my dir".into() }]
        );
        assert_eq!(bel.plain_text, "$ ");

        let st = parse_ansi_rich(&format!("\x1b]7;file:///C:/Users/me{ST}"));
        assert_eq!(
            st.events,
            vec![VtEvent::WorkingDirectory { host: String::new(), path: "C:/Users/me".into() }]
        );
        assert_eq!(st.plain_text, "");
    }

    #[test]
    fn public_osc7_helper() {
        assert_eq!(
            parse_osc7_uri("file://h/home/a%20b"),
            Some(std::path::PathBuf::from("/home/a b"))
        );
        assert_eq!(parse_osc7_uri("file:///D:/x"), Some(std::path::PathBuf::from("D:/x")));
        assert_eq!(parse_osc7_uri("https://h/x"), None);
    }

    #[test]
    fn other_osc_sequences_are_stripped() {
        let parsed = parse_ansi_rich(
            "\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]9;4;1;50\x07\x1b]0;title\x1b\\\x1b]133;D;0\x07done",
        );
        assert_eq!(parsed.plain_text, "$ lsdone");
        assert!(parsed.events.is_empty());
        assert!(parsed.spans.iter().all(|s| s.link.is_none()));
    }

    #[test]
    fn osc7_malformed_is_ignored() {
        for bad in [
            "\x1b]7;http://host/x\x07",
            "\x1b]7;file://hostonly\x07",
            "\x1b]7;file:///bad%zz\x07",
            "\x1b]7\x07",
        ] {
            let parsed = parse_ansi_rich(bad);
            assert!(parsed.events.is_empty(), "{bad:?}");
            assert_eq!(parsed.plain_text, "");
        }
    }

    #[test]
    fn osc8_links_spans_bel_and_st() {
        let input = format!(
            "see \x1b]8;;https://example.com/a;b\x07here\x1b]8;;\x07 and \x1b]8;id=1;file:///tmp/x{ST}\x1b[1mthere\x1b]8;;{ST} done"
        );
        let parsed = parse_ansi_rich(&input);
        assert_eq!(parsed.plain_text, "see here and there done");
        let linked: Vec<_> = parsed
            .spans
            .iter()
            .filter_map(|s| s.link.as_deref().map(|l| (s.text.as_str(), l)))
            .collect();
        assert_eq!(linked, vec![("here", "https://example.com/a;b"), ("there", "file:///tmp/x")]);
        assert!(parsed.spans.last().unwrap().link.is_none());
        assert!(parsed.events.is_empty());
    }

    #[test]
    fn osc8_malformed_does_not_break_text() {
        let parsed = parse_ansi_rich("a\x1b]8\x07b\x1b]8;;\x07c");
        assert_eq!(parsed.plain_text, "abc");
        assert!(parsed.spans.iter().all(|s| s.link.is_none()));
        // Unterminated link: the text still shows and keeps the link.
        let parsed = parse_ansi_rich("\x1b]8;;https://x.dev\x07tail");
        assert_eq!(parsed.plain_text, "tail");
        assert_eq!(parsed.spans[0].link.as_deref(), Some("https://x.dev"));
    }

    #[test]
    fn osc52_bel_and_st() {
        let bel = parse_ansi_rich("\x1b]52;c;aGVsbG8gd29ybGQ=\x07");
        assert_eq!(
            bel.events,
            vec![VtEvent::ClipboardWrite { selection: "c".into(), text: "hello world".into() }]
        );
        assert_eq!(bel.plain_text, "");
        let st = parse_ansi_rich(&format!("\x1b]52;;w7xiZXI={ST}"));
        assert_eq!(
            st.events,
            vec![VtEvent::ClipboardWrite { selection: String::new(), text: "über".into() }]
        );
    }

    #[test]
    fn osc52_malformed_and_queries_are_ignored() {
        for bad in [
            "\x1b]52;c;?\x07",
            "\x1b]52;c;not*base64\x07",
            "\x1b]52;c;QQ=Q\x07",
            "\x1b]52;c\x07",
            "\x1b]52\x07",
        ] {
            assert!(parse_ansi_rich(bad).events.is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn base64_decoder() {
        assert_eq!(decode_base64(b"").unwrap(), b"");
        assert_eq!(decode_base64(b"Zg==").unwrap(), b"f");
        assert_eq!(decode_base64(b"Zm8=").unwrap(), b"fo");
        assert_eq!(decode_base64(b"Zm9v").unwrap(), b"foo");
        assert_eq!(decode_base64(b"Zm9vYg").unwrap(), b"foob");
        assert_eq!(decode_base64(b"Zm9v\nYmFy").unwrap(), b"foobar");
        assert!(decode_base64(b"Z").is_none());
        assert!(decode_base64(b"Zg=").is_none());
    }
}
