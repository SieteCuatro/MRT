//! Inline images in command output: the iTerm2 `OSC 1337 ; File=` sequence, the
//! kitty graphics protocol (`ESC _ G … ESC \`) and Sixel (`ESC P … q … ESC \`).
//!
//! [`crate::vt::VtParserState`] calls into this module and reports each image as an
//! [`InlineImage`] anchored at a byte offset of the plain text, so the GUI can draw
//! it between the lines around it. Images are decoded to RGBA here, with size
//! limits, so a hostile program cannot make MRT allocate without bound.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

/// Largest width or height of an inline image, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 8192;
/// Largest encoded payload MRT accepts for one image.
pub const MAX_IMAGE_PAYLOAD: usize = 64 * 1024 * 1024;
/// Largest decoded image, in bytes of RGBA.
const MAX_DECODED_BYTES: u64 = 256 * 1024 * 1024;
/// Images a program may store with kitty `a=t` for a later `a=p`.
const MAX_STORED_KITTY_IMAGES: usize = 16;

/// A decoded image shown in a block.
#[derive(Clone, PartialEq, Eq)]
pub struct InlineImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` RGBA pixels, shared so blocks clone cheaply.
    pub rgba: Arc<Vec<u8>>,
    /// Byte offset in the plain text where the image appears.
    pub anchor: usize,
    /// Display width the program asked for, if any.
    pub requested_width: Option<ImageSize>,
    /// Display height the program asked for, if any.
    pub requested_height: Option<ImageSize>,
}

impl std::fmt::Debug for InlineImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("anchor", &self.anchor)
            .field("requested_width", &self.requested_width)
            .field("requested_height", &self.requested_height)
            .finish()
    }
}

/// A display size from an image sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSize {
    /// Character cells (iTerm2 `width=N`, kitty `c=` / `r=`).
    Cells(u32),
    /// Pixels (iTerm2 `width=Npx`).
    Pixels(u32),
    /// Percent of the available width or height (iTerm2 `width=N%`).
    Percent(u32),
}

/// Pixels decoded from a sequence, before it is anchored in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub requested_width: Option<ImageSize>,
    pub requested_height: Option<ImageSize>,
}

impl DecodedImage {
    pub fn anchored(self, anchor: usize) -> InlineImage {
        InlineImage {
            width: self.width,
            height: self.height,
            rgba: Arc::new(self.rgba),
            anchor,
            requested_width: self.requested_width,
            requested_height: self.requested_height,
        }
    }
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("image has no pixels".into());
    }
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err(format!("image {width}×{height} is larger than {MAX_IMAGE_SIDE} pixels"));
    }
    Ok(())
}

/// Decodes a PNG, JPEG or GIF file to RGBA, refusing images over the size limits.
pub fn decode_image_file(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_DECODED_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
    let (width, height) = decoded.dimensions();
    check_size(width, height)?;
    Ok((width, height, decoded.into_raw()))
}

fn parse_iterm_size(value: &str) -> Option<ImageSize> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    if let Some(px) = value.strip_suffix("px") {
        return px.parse().ok().map(ImageSize::Pixels);
    }
    if let Some(pct) = value.strip_suffix('%') {
        return pct.parse().ok().map(|p: u32| ImageSize::Percent(p.min(100)));
    }
    value.parse().ok().map(ImageSize::Cells)
}

/// Parses the payload of `OSC 1337 ; File=[args]:<base64>` (everything after
/// `1337;`). Returns `None` for other 1337 commands and for files sent without
/// `inline=1` (iTerm2 downloads those; MRT ignores them).
pub fn parse_iterm_file(payload: &str) -> Option<Result<DecodedImage, String>> {
    let rest = payload.strip_prefix("File=")?;
    let (args, data) = rest.split_once(':')?;
    let mut inline = false;
    let (mut requested_width, mut requested_height) = (None, None);
    for arg in args.split(';') {
        let Some((key, value)) = arg.split_once('=') else { continue };
        match key.trim() {
            "inline" => inline = value.trim() == "1",
            "width" => requested_width = parse_iterm_size(value),
            "height" => requested_height = parse_iterm_size(value),
            _ => {}
        }
    }
    if !inline {
        return None;
    }
    if data.len() > MAX_IMAGE_PAYLOAD / 3 * 4 + 4 {
        return Some(Err("image payload is too large".into()));
    }
    let Some(bytes) = crate::vt::decode_base64(data.trim().as_bytes()) else {
        return Some(Err("image payload is not valid base64".into()));
    };
    Some(decode_image_file(&bytes).map(|(width, height, rgba)| DecodedImage {
        width,
        height,
        rgba,
        requested_width,
        requested_height,
    }))
}

/// Reassembles kitty graphics commands sent in chunks (`m=1`) and keeps images
/// stored with `a=t` for a later `a=p`. Only direct transmission (`t=d`) is
/// accepted: MRT never reads files or shared memory a program names.
#[derive(Default)]
pub struct KittyGraphics {
    pending: Option<(HashMap<String, String>, Vec<u8>)>,
    stored: HashMap<u32, DecodedImage>,
    stored_order: Vec<u32>,
}

impl KittyGraphics {
    /// Handles one APC payload (without `ESC _` and `ESC \`). Returns an image to
    /// show now, if the command displays one.
    pub fn handle_apc(&mut self, apc: &[u8]) -> Option<Result<DecodedImage, String>> {
        let body = apc.strip_prefix(b"G")?;
        let (control, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &[][..]),
        };
        let keys: HashMap<String, String> = String::from_utf8_lossy(control)
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let more = keys.get("m").is_some_and(|m| m == "1");
        // Continuation chunks carry only `m` (and maybe `q`); the first chunk's keys apply.
        let (keys, mut data) = match self.pending.take() {
            Some((first, data)) => (first, data),
            None => (keys, Vec::new()),
        };
        if data.len() + payload.len() > MAX_IMAGE_PAYLOAD / 3 * 4 + 4 {
            return Some(Err("image payload is too large".into()));
        }
        data.extend_from_slice(payload);
        if more {
            self.pending = Some((keys, data));
            return None;
        }

        let get = |k: &str| keys.get(k).map(String::as_str);
        let action = get("a").unwrap_or("t");
        let id: Option<u32> = get("i").and_then(|v| v.parse().ok());
        let cells = |k: &str| get(k).and_then(|v| v.parse().ok()).filter(|&n: &u32| n > 0).map(ImageSize::Cells);
        match action {
            "p" => {
                let mut image = self.stored.get(&id?)?.clone();
                image.requested_width = cells("c").or(image.requested_width);
                image.requested_height = cells("r").or(image.requested_height);
                Some(Ok(image))
            }
            "t" | "T" => {
                if get("t").unwrap_or("d") != "d" {
                    return Some(Err("kitty images must be sent directly (t=d)".into()));
                }
                let image = Self::decode(&keys, &data).map(|(width, height, rgba)| DecodedImage {
                    width,
                    height,
                    rgba,
                    requested_width: cells("c"),
                    requested_height: cells("r"),
                });
                match (action, image) {
                    ("T", image) => Some(image),
                    (_, Ok(image)) => {
                        if let Some(id) = id {
                            self.store(id, image);
                        }
                        None
                    }
                    (_, Err(e)) => Some(Err(e)),
                }
            }
            // Queries, deletes and animation frames are not supported; no reply is sent.
            _ => None,
        }
    }

    fn store(&mut self, id: u32, image: DecodedImage) {
        if self.stored.insert(id, image).is_none() {
            self.stored_order.push(id);
        }
        while self.stored_order.len() > MAX_STORED_KITTY_IMAGES {
            let old = self.stored_order.remove(0);
            self.stored.remove(&old);
        }
    }

    fn decode(keys: &HashMap<String, String>, base64: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
        let get = |k: &str| keys.get(k).map(String::as_str);
        let mut bytes = crate::vt::decode_base64(base64).ok_or("image payload is not valid base64")?;
        if get("o") == Some("z") {
            let mut inflated = Vec::new();
            flate2::read::ZlibDecoder::new(&bytes[..])
                .take(MAX_DECODED_BYTES + 1)
                .read_to_end(&mut inflated)
                .map_err(|e| format!("image data is not valid zlib: {e}"))?;
            if inflated.len() as u64 > MAX_DECODED_BYTES {
                return Err("image is too large".into());
            }
            bytes = inflated;
        }
        let format = get("f").unwrap_or("32");
        if format == "100" {
            return decode_image_file(&bytes);
        }
        let channels = match format {
            "24" => 3,
            "32" => 4,
            other => return Err(format!("kitty image format f={other} is not supported")),
        };
        let width: u32 = get("s").and_then(|v| v.parse().ok()).unwrap_or(0);
        let height: u32 = get("v").and_then(|v| v.parse().ok()).unwrap_or(0);
        check_size(width, height)?;
        let expected = width as usize * height as usize * channels;
        if bytes.len() < expected {
            return Err(format!("image data is {} bytes, expected {expected}", bytes.len()));
        }
        let rgba = if channels == 4 {
            bytes.truncate(expected);
            bytes
        } else {
            bytes[..expected].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        };
        Ok((width, height, rgba))
    }
}

/// Finds APC strings (`ESC _ … ESC \`) in a byte stream. `vte` skips their
/// content, so MRT scans for them next to the parser.
#[derive(Default)]
pub struct ApcScanner {
    state: ApcState,
}

#[derive(Default)]
enum ApcState {
    #[default]
    Ground,
    Escape,
    Apc(Vec<u8>),
    ApcEscape(Vec<u8>),
    /// An APC that grew past the payload limit: skip to its end.
    Skip,
}

impl ApcScanner {
    /// Feeds one byte; returns the APC payload when a string ends.
    pub fn advance(&mut self, byte: u8) -> Option<Vec<u8>> {
        let state = std::mem::take(&mut self.state);
        let (next, done) = match (state, byte) {
            (ApcState::Ground | ApcState::Escape, 0x1b) => (ApcState::Escape, None),
            (ApcState::Escape, b'_') => (ApcState::Apc(Vec::new()), None),
            (ApcState::Ground | ApcState::Escape, _) => (ApcState::Ground, None),
            (ApcState::Apc(buf), 0x1b) => (ApcState::ApcEscape(buf), None),
            (ApcState::Apc(buf), 0x07) => (ApcState::Ground, Some(buf)),
            (ApcState::Apc(_), 0x18 | 0x1a) => (ApcState::Ground, None),
            (ApcState::Apc(mut buf), b) => {
                if buf.len() >= MAX_IMAGE_PAYLOAD / 3 * 4 + 64 {
                    (ApcState::Skip, None)
                } else {
                    buf.push(b);
                    (ApcState::Apc(buf), None)
                }
            }
            (ApcState::ApcEscape(buf), b'\\') => (ApcState::Ground, Some(buf)),
            (ApcState::ApcEscape(_), b'_') => (ApcState::Apc(Vec::new()), None),
            (ApcState::ApcEscape(_), _) => (ApcState::Ground, None),
            (ApcState::Skip, 0x1b | 0x07) => (ApcState::Ground, None),
            (ApcState::Skip, _) => (ApcState::Skip, None),
        };
        self.state = next;
        done
    }
}

/// The 16 VT340 colors Sixel images start with (percent RGB scaled to 0–255).
const SIXEL_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],
    [51, 51, 204],
    [204, 36, 36],
    [51, 204, 51],
    [204, 51, 204],
    [51, 204, 204],
    [204, 204, 51],
    [120, 120, 120],
    [69, 69, 69],
    [87, 87, 153],
    [153, 69, 69],
    [87, 153, 87],
    [153, 87, 153],
    [87, 153, 153],
    [153, 153, 87],
    [204, 204, 204],
];

/// Collects the data of a Sixel DCS (`ESC P <P1;P2;P3> q … ESC \`) and decodes it.
pub struct SixelDecoder {
    data: Vec<u8>,
    /// `P2 = 1`: pixels that are not drawn stay transparent.
    transparent: bool,
    overflow: bool,
}

impl SixelDecoder {
    pub fn new(p2: u16) -> Self {
        Self { data: Vec::new(), transparent: p2 == 1, overflow: false }
    }

    pub fn put(&mut self, byte: u8) {
        if self.data.len() < MAX_IMAGE_PAYLOAD {
            self.data.push(byte);
        } else {
            self.overflow = true;
        }
    }

    /// Decodes the collected data.
    pub fn finish(self) -> Result<DecodedImage, String> {
        if self.overflow {
            return Err("sixel image is too large".into());
        }
        decode_sixel(&self.data, self.transparent)
    }
}

fn hls_to_rgb(h: u32, l: u32, s: u32) -> [u8; 3] {
    // Sixel HLS: hue 0–360 with blue at 0, lightness and saturation 0–100.
    let (h, l, s) = (((h + 240) % 360) as f32, l.min(100) as f32 / 100.0, s.min(100) as f32 / 100.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0).round() as u8, ((g + m) * 255.0).round() as u8, ((b + m) * 255.0).round() as u8]
}

/// Decodes Sixel data (what follows the `q` of the DCS) into RGBA.
pub fn decode_sixel(data: &[u8], transparent: bool) -> Result<DecodedImage, String> {
    let mut palette: Vec<[u8; 3]> = SIXEL_PALETTE.to_vec();
    palette.resize(256, [0, 0, 0]);
    let mut color = 0usize;
    let (mut x, mut y) = (0usize, 0usize);
    let (mut width, mut height) = (0usize, 0usize);
    // Pixels as palette-resolved RGBA, grown as the image extends.
    let mut pixels: Vec<[u8; 4]> = Vec::new();
    let mut stride = 0usize;
    let background = if transparent { [0, 0, 0, 0] } else { [0, 0, 0, 255] };

    let mut i = 0;
    let number = |i: &mut usize| -> u32 {
        let mut n: u32 = 0;
        while *i < data.len() && data[*i].is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((data[*i] - b'0') as u32);
            *i += 1;
        }
        n
    };
    let numbers = |i: &mut usize| -> Vec<u32> {
        let mut out = vec![number(i)];
        while *i < data.len() && data[*i] == b';' {
            *i += 1;
            out.push(number(i));
        }
        out
    };

    let ensure = |pixels: &mut Vec<[u8; 4]>, stride: &mut usize, w: usize, h: usize| -> Result<(), String> {
        if w > MAX_IMAGE_SIDE as usize || h > MAX_IMAGE_SIDE as usize {
            return Err("sixel image is too large".into());
        }
        let rows = if *stride == 0 { 0 } else { pixels.len() / *stride };
        if w > *stride {
            let new_stride = w.max(*stride * 2).min(MAX_IMAGE_SIDE as usize);
            let mut grown = vec![background; new_stride * rows];
            for r in 0..rows {
                grown[r * new_stride..r * new_stride + *stride].copy_from_slice(&pixels[r * *stride..(r + 1) * *stride]);
            }
            *pixels = grown;
            *stride = new_stride;
        }
        if h > rows {
            let new_rows = h.max(rows * 2).min(MAX_IMAGE_SIDE as usize);
            pixels.resize(*stride * new_rows, background);
        }
        Ok(())
    };

    while i < data.len() {
        let b = data[i];
        match b {
            b'"' => {
                i += 1;
                let attrs = numbers(&mut i);
                if let (Some(&w), Some(&h)) = (attrs.get(2), attrs.get(3)) {
                    let (w, h) = (w as usize, h as usize);
                    ensure(&mut pixels, &mut stride, w.max(1), h.max(1))?;
                    width = width.max(w);
                    height = height.max(h);
                }
            }
            b'#' => {
                i += 1;
                let args = numbers(&mut i);
                color = (args[0] as usize).min(255);
                if args.len() >= 5 {
                    palette[color] = match args[1] {
                        1 => hls_to_rgb(args[2], args[3], args[4]),
                        _ => {
                            let pct = |v: u32| ((v.min(100) * 255 + 50) / 100) as u8;
                            [pct(args[2]), pct(args[3]), pct(args[4])]
                        }
                    };
                }
            }
            b'!' => {
                i += 1;
                let count = number(&mut i).max(1) as usize;
                if i < data.len() && (b'?'..=b'~').contains(&data[i]) {
                    let bits = data[i] - b'?';
                    i += 1;
                    let end = x.saturating_add(count);
                    if end > MAX_IMAGE_SIDE as usize {
                        return Err("sixel image is too large".into());
                    }
                    if bits != 0 {
                        ensure(&mut pixels, &mut stride, end, y + 6)?;
                        let [r, g, bl] = palette[color];
                        for bit in 0..6 {
                            if bits & (1 << bit) != 0 {
                                let row = (y + bit) * stride;
                                pixels[row + x..row + end].fill([r, g, bl, 255]);
                                height = height.max(y + bit + 1);
                            }
                        }
                    }
                    x = end;
                    width = width.max(x);
                }
            }
            b'$' => {
                i += 1;
                x = 0;
            }
            b'-' => {
                i += 1;
                x = 0;
                y += 6;
                if y > MAX_IMAGE_SIDE as usize {
                    return Err("sixel image is too large".into());
                }
            }
            b'?'..=b'~' => {
                i += 1;
                let bits = b - b'?';
                if bits != 0 {
                    ensure(&mut pixels, &mut stride, x + 1, y + 6)?;
                    let [r, g, bl] = palette[color];
                    for bit in 0..6 {
                        if bits & (1 << bit) != 0 {
                            pixels[(y + bit) * stride + x] = [r, g, bl, 255];
                            height = height.max(y + bit + 1);
                        }
                    }
                }
                x += 1;
                width = width.max(x);
            }
            _ => i += 1,
        }
    }

    check_size(width as u32, height as u32)?;
    ensure(&mut pixels, &mut stride, width, height)?;
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        for px in &pixels[row * stride..row * stride + width] {
            rgba.extend_from_slice(px);
        }
    }
    Ok(DecodedImage {
        width: width as u32,
        height: height as u32,
        rgba,
        requested_width: None,
        requested_height: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×1 PNG: one red and one blue pixel.
    fn tiny_png() -> Vec<u8> {
        let mut out = Vec::new();
        let img = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn iterm_inline_file_decodes_png() {
        let payload = format!("File=name=eA==;size=10;width=20;height=50%;inline=1:{}", crate::shell_integration::base64_encode(&tiny_png()));
        let image = parse_iterm_file(&payload).unwrap().unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(&image.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(image.requested_width, Some(ImageSize::Cells(20)));
        assert_eq!(image.requested_height, Some(ImageSize::Percent(50)));
        assert!(parse_iterm_file("File=inline=0:AAAA").is_none(), "downloads are ignored");
        assert!(parse_iterm_file("SetMark").is_none());
        assert!(parse_iterm_file("File=inline=1:!!!").unwrap().is_err());
    }

    #[test]
    fn kitty_chunks_store_and_place() {
        let mut kitty = KittyGraphics::default();
        let rgb = crate::shell_integration::base64_encode(&[1, 2, 3, 4, 5, 6]);
        let (a, b) = rgb.split_at(4);
        assert!(kitty.handle_apc(format!("Ga=T,f=24,s=2,v=1,m=1;{a}").as_bytes()).is_none());
        let image = kitty.handle_apc(format!("Gm=0;{b}").as_bytes()).unwrap().unwrap();
        assert_eq!(image.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);

        let png = crate::shell_integration::base64_encode(&tiny_png());
        assert!(kitty.handle_apc(format!("Ga=t,f=100,i=7;{png}").as_bytes()).is_none());
        let placed = kitty.handle_apc(b"Ga=p,i=7,c=10").unwrap().unwrap();
        assert_eq!((placed.width, placed.height), (2, 1));
        assert_eq!(placed.requested_width, Some(ImageSize::Cells(10)));
        assert!(kitty.handle_apc(b"Ga=p,i=8").is_none(), "unknown id");
        assert!(kitty.handle_apc(b"Ga=T,t=f;L3RtcC94").unwrap().is_err(), "files are never read");
        assert!(kitty.handle_apc(b"Ga=q,i=1;AAAA").is_none());
    }

    #[test]
    fn kitty_zlib_rgba() {
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&[9, 8, 7, 6]).unwrap();
        let data = crate::shell_integration::base64_encode(&enc.finish().unwrap());
        let image = KittyGraphics::default()
            .handle_apc(format!("Ga=T,f=32,s=1,v=1,o=z;{data}").as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(image.rgba, vec![9, 8, 7, 6]);
    }

    #[test]
    fn apc_scanner_finds_strings_split_anywhere() {
        let mut scanner = ApcScanner::default();
        let input = b"text\x1b_Gabc\x1b\\more\x1b[31m\x1b_Gx\x07";
        let found: Vec<Vec<u8>> = input.iter().filter_map(|&b| scanner.advance(b)).collect();
        assert_eq!(found, vec![b"Gabc".to_vec(), b"Gx".to_vec()]);
    }

    #[test]
    fn sixel_decodes_colors_repeats_and_bands() {
        // Color 1 = pure red (RGB percent); "~" sets all six pixels; "!3~" repeats.
        let image = decode_sixel(b"#1;2;100;0;0#1~!3~-#2;2;0;0;100@", false).unwrap();
        assert_eq!((image.width, image.height), (4, 7));
        assert_eq!(&image.rgba[..4], &[255, 0, 0, 255]);
        // Row 6, column 0: the blue pixel of the second band.
        let at = |x: usize, y: usize| &image.rgba[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(0, 6), &[0, 0, 255, 255]);
        assert_eq!(at(1, 6), &[0, 0, 0, 255], "background");
        assert!(decode_sixel(b"!99999~", false).is_err());
        assert!(decode_sixel(b"", false).is_err());
    }
}
