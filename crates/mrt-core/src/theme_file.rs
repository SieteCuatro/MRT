//! Theme files: MRT's own TOML format and imports of common terminal theme formats
//! (Windows Terminal JSON schemes, Alacritty TOML, Ghostty and iTerm2
//! `.itermcolors`). The result is a [`ThemeSpec`]: optional overrides for MRT's UI
//! colors, plus terminal background, foreground and the 16 ANSI colors.
//!
//! The 16 ANSI colors are applied process-wide with [`set_ansi_palette`]; the
//! block parser (`vt.rs`) and the grid emulator (`term.rs`) read them.

use crate::vt::AnsiColor;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;

/// A parsed theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeSpec {
    pub name: Option<String>,
    /// UI color overrides by MRT palette name in lowercase (`bg_base`, `accent`...).
    pub ui: BTreeMap<String, AnsiColor>,
    pub background: Option<AnsiColor>,
    pub foreground: Option<AnsiColor>,
    /// The 16 ANSI colors (0–7 normal, 8–15 bright), when the theme sets all of them.
    pub ansi: Option<[AnsiColor; 16]>,
}

/// Parses `#rrggbb`, `rrggbb`, `#rgb` or `0xrrggbb`.
pub fn parse_hex_color(text: &str) -> Option<AnsiColor> {
    let t = text.trim().trim_matches('"').trim_matches('\'');
    let hex = t.strip_prefix('#').or_else(|| t.strip_prefix("0x")).unwrap_or(t);
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        6 => Some(AnsiColor::new(byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?)),
        3 => {
            let d = |i: usize| byte(&hex[i..i + 1]).map(|v| v * 17);
            Some(AnsiColor::new(d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

const ANSI_NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

fn collect_ansi(get: impl Fn(usize) -> Option<AnsiColor>) -> Option<[AnsiColor; 16]> {
    let mut out = [AnsiColor::new(0, 0, 0); 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = get(i)?;
    }
    Some(out)
}

fn toml_color(table: &toml::Table, key: &str) -> Option<AnsiColor> {
    table.get(key).and_then(|v| v.as_str()).and_then(parse_hex_color)
}

/// MRT's own format:
///
/// ```toml
/// name = "Night"
/// [ui]
/// accent = "#8EA2FF"
/// bg_base = "#101014"
/// [terminal]
/// background = "#0B0B0E"
/// foreground = "#D0D0D8"
/// ansi = ["#000000", "#cd0000", ...]   # 16 colors
/// ```
fn parse_mrt_toml(table: &toml::Table) -> Option<ThemeSpec> {
    if !table.contains_key("ui") && !table.contains_key("terminal") {
        return None;
    }
    let mut spec = ThemeSpec { name: table.get("name").and_then(|v| v.as_str()).map(str::to_string), ..Default::default() };
    if let Some(ui) = table.get("ui").and_then(|v| v.as_table()) {
        for (key, value) in ui {
            if let Some(color) = value.as_str().and_then(parse_hex_color) {
                spec.ui.insert(key.to_ascii_lowercase(), color);
            }
        }
    }
    if let Some(term) = table.get("terminal").and_then(|v| v.as_table()) {
        spec.background = toml_color(term, "background");
        spec.foreground = toml_color(term, "foreground");
        if let Some(list) = term.get("ansi").and_then(|v| v.as_array()) {
            spec.ansi = collect_ansi(|i| list.get(i).and_then(|v| v.as_str()).and_then(parse_hex_color));
        }
    }
    Some(spec)
}

/// Alacritty: `[colors.primary]` background/foreground, `[colors.normal]` and `[colors.bright]`.
fn parse_alacritty(table: &toml::Table) -> Option<ThemeSpec> {
    let colors = table.get("colors")?.as_table()?;
    let primary = colors.get("primary").and_then(|v| v.as_table());
    let normal = colors.get("normal").and_then(|v| v.as_table());
    let bright = colors.get("bright").and_then(|v| v.as_table());
    let ansi = match (normal, bright) {
        (Some(n), Some(b)) => collect_ansi(|i| toml_color(if i < 8 { n } else { b }, ANSI_NAMES[i % 8])),
        _ => None,
    };
    Some(ThemeSpec {
        name: None,
        ui: BTreeMap::new(),
        background: primary.and_then(|p| toml_color(p, "background")),
        foreground: primary.and_then(|p| toml_color(p, "foreground")),
        ansi,
    })
}

/// Windows Terminal scheme: `background`, `foreground`, `black`..`white` with `purple`
/// for magenta, and `brightBlack`..`brightWhite`. Accepts one scheme or `{"schemes": [..]}`.
fn parse_windows_terminal(value: &serde_json::Value) -> Option<ThemeSpec> {
    let scheme = value.get("schemes").and_then(|s| s.get(0)).unwrap_or(value);
    let get = |key: &str| scheme.get(key).and_then(|v| v.as_str()).and_then(parse_hex_color);
    let names = ["black", "red", "green", "yellow", "blue", "purple", "cyan", "white"];
    let ansi = collect_ansi(|i| {
        let base = names[i % 8];
        if i < 8 {
            get(base)
        } else {
            let mut key = String::from("bright");
            key.push_str(&base[..1].to_ascii_uppercase());
            key.push_str(&base[1..]);
            get(&key)
        }
    });
    if ansi.is_none() && get("background").is_none() {
        return None;
    }
    Some(ThemeSpec {
        name: scheme.get("name").and_then(|v| v.as_str()).map(str::to_string),
        ui: BTreeMap::new(),
        background: get("background"),
        foreground: get("foreground"),
        ansi,
    })
}

/// Ghostty: `palette = N=#rrggbb` lines, `background = ..`, `foreground = ..`.
fn parse_ghostty(text: &str) -> Option<ThemeSpec> {
    let mut palette: BTreeMap<usize, AnsiColor> = BTreeMap::new();
    let mut spec = ThemeSpec::default();
    let mut any = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "palette" => {
                if let Some((n, color)) = value.split_once('=') {
                    if let (Ok(n), Some(color)) = (n.trim().parse::<usize>(), parse_hex_color(color)) {
                        palette.insert(n, color);
                        any = true;
                    }
                }
            }
            "background" => {
                spec.background = parse_hex_color(value);
                any |= spec.background.is_some();
            }
            "foreground" => {
                spec.foreground = parse_hex_color(value);
                any |= spec.foreground.is_some();
            }
            _ => {}
        }
    }
    spec.ansi = collect_ansi(|i| palette.get(&i).copied());
    any.then_some(spec)
}

/// iTerm2 `.itermcolors`: a plist of `Ansi N Color`, `Background Color` and
/// `Foreground Color` dictionaries with `Red/Green/Blue Component` reals (0–1).
fn parse_itermcolors(text: &str) -> Option<ThemeSpec> {
    if !text.contains("<plist") {
        return None;
    }
    let mut colors: BTreeMap<String, AnsiColor> = BTreeMap::new();
    let mut rest = text;
    while let Some(start) = rest.find("<key>") {
        let after = &rest[start + 5..];
        let Some(end) = after.find("</key>") else { break };
        let key = after[..end].trim().to_string();
        let body = &after[end..];
        if key.ends_with("Color") {
            if let (Some(open), Some(close)) = (body.find("<dict>"), body.find("</dict>")) {
                let dict = &body[open..close];
                let component = |name: &str| -> Option<u8> {
                    let at = dict.find(&format!("<key>{name} Component</key>"))?;
                    let real = &dict[at..];
                    let v0 = real.find("<real>")? + 6;
                    let v1 = real.find("</real>")?;
                    let value: f32 = real[v0..v1].trim().parse().ok()?;
                    Some((value.clamp(0.0, 1.0) * 255.0).round() as u8)
                };
                if let (Some(r), Some(g), Some(b)) = (component("Red"), component("Green"), component("Blue")) {
                    colors.insert(key.clone(), AnsiColor::new(r, g, b));
                }
                rest = &body[close..];
                continue;
            }
        }
        rest = body;
    }
    if colors.is_empty() {
        return None;
    }
    Some(ThemeSpec {
        name: None,
        ui: BTreeMap::new(),
        background: colors.get("Background Color").copied(),
        foreground: colors.get("Foreground Color").copied(),
        ansi: collect_ansi(|i| colors.get(&format!("Ansi {i} Color")).copied()),
    })
}

/// Parses theme text in any supported format.
pub fn parse_theme(text: &str) -> Result<ThemeSpec, String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        return parse_windows_terminal(&value).ok_or_else(|| "JSON theme has no Windows Terminal scheme colors".into());
    }
    if let Some(spec) = parse_itermcolors(text) {
        return Ok(spec);
    }
    if let Ok(table) = text.parse::<toml::Table>() {
        if let Some(spec) = parse_mrt_toml(&table).or_else(|| parse_alacritty(&table)) {
            return Ok(spec);
        }
    }
    parse_ghostty(text).ok_or_else(|| {
        "not a recognised theme (MRT TOML, Windows Terminal JSON, Alacritty TOML, Ghostty or .itermcolors)".into()
    })
}

pub fn load_theme_file(path: &Path) -> Result<ThemeSpec, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut spec = parse_theme(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if spec.name.is_none() {
        spec.name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    }
    Ok(spec)
}

static ANSI_PALETTE: RwLock<Option<[AnsiColor; 16]>> = RwLock::new(None);

/// Replaces the 16 ANSI colors used for new output (`None` restores the defaults).
pub fn set_ansi_palette(colors: Option<[AnsiColor; 16]>) {
    if let Ok(mut guard) = ANSI_PALETTE.write() {
        *guard = colors;
    }
}

/// The theme's color for ANSI index 0–15, if a theme set them.
pub fn themed_ansi(index: usize) -> Option<AnsiColor> {
    ANSI_PALETTE.read().ok().and_then(|p| p.map(|colors| colors[index.min(15)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#FF8000"), Some(AnsiColor::new(255, 128, 0)));
        assert_eq!(parse_hex_color("0x0a0b0c"), Some(AnsiColor::new(10, 11, 12)));
        assert_eq!(parse_hex_color("\"#fff\""), Some(AnsiColor::new(255, 255, 255)));
        assert_eq!(parse_hex_color("blue"), None);
    }

    fn sixteen(prefix: &str) -> Vec<String> {
        (0..16).map(|i| format!("{prefix}{:02x}{:02x}{:02x}", i, i * 2, i * 3)).collect()
    }

    #[test]
    fn mrt_toml() {
        let ansi: Vec<String> = sixteen("#").iter().map(|c| format!("\"{c}\"")).collect();
        let text = format!(
            "name = \"Night\"\n[ui]\naccent = \"#112233\"\nBG_BASE = \"#000000\"\n[terminal]\nbackground = \"#010203\"\nansi = [{}]\n",
            ansi.join(", ")
        );
        let spec = parse_theme(&text).unwrap();
        assert_eq!(spec.name.as_deref(), Some("Night"));
        assert_eq!(spec.ui["accent"], AnsiColor::new(0x11, 0x22, 0x33));
        assert!(spec.ui.contains_key("bg_base"), "keys are lowercased");
        assert_eq!(spec.background, Some(AnsiColor::new(1, 2, 3)));
        assert_eq!(spec.ansi.unwrap()[15], AnsiColor::new(15, 30, 45));
    }

    #[test]
    fn windows_terminal_scheme() {
        let names = ["black", "red", "green", "yellow", "blue", "purple", "cyan", "white"];
        let mut obj = serde_json::Map::new();
        obj.insert("name".into(), "Campbell".into());
        obj.insert("background".into(), "#0C0C0C".into());
        obj.insert("foreground".into(), "#CCCCCC".into());
        for (i, n) in names.iter().enumerate() {
            obj.insert((*n).into(), format!("#0000{:02X}", i).into());
            let bright = format!("bright{}{}", n[..1].to_uppercase(), &n[1..]);
            obj.insert(bright, format!("#00FF{:02X}", i).into());
        }
        let spec = parse_theme(&serde_json::Value::Object(obj).to_string()).unwrap();
        let ansi = spec.ansi.unwrap();
        assert_eq!(ansi[5], AnsiColor::new(0, 0, 5), "purple is magenta");
        assert_eq!(ansi[13], AnsiColor::new(0, 255, 5));
        assert_eq!(spec.name.as_deref(), Some("Campbell"));
    }

    #[test]
    fn alacritty_toml() {
        let mut text = String::from("[colors.primary]\nbackground = \"#1d1f21\"\nforeground = \"#c5c8c6\"\n");
        for section in ["normal", "bright"] {
            text.push_str(&format!("[colors.{section}]\n"));
            for (i, n) in ANSI_NAMES.iter().enumerate() {
                text.push_str(&format!("{n} = \"#{:02x}{:02x}{:02x}\"\n", i, if section == "bright" { 255 } else { 0 }, 0));
            }
        }
        let spec = parse_theme(&text).unwrap();
        assert_eq!(spec.background, Some(AnsiColor::new(0x1d, 0x1f, 0x21)));
        assert_eq!(spec.ansi.unwrap()[9], AnsiColor::new(1, 255, 0));
    }

    #[test]
    fn ghostty_theme() {
        let mut text = String::from("# comment\nbackground = 282a36\nforeground = #f8f8f2\n");
        for (i, c) in sixteen("#").iter().enumerate() {
            text.push_str(&format!("palette = {i}={c}\n"));
        }
        let spec = parse_theme(&text).unwrap();
        assert_eq!(spec.background, Some(AnsiColor::new(0x28, 0x2a, 0x36)));
        assert_eq!(spec.ansi.unwrap()[3], AnsiColor::new(3, 6, 9));
    }

    #[test]
    fn itermcolors_plist() {
        let mut text = String::from("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>");
        for i in 0..16 {
            text.push_str(&format!(
                "<key>Ansi {i} Color</key><dict><key>Blue Component</key><real>0</real><key>Green Component</key><real>0.5</real><key>Red Component</key><real>1</real></dict>"
            ));
        }
        text.push_str("<key>Background Color</key><dict><key>Blue Component</key><real>0.2</real><key>Green Component</key><real>0.1</real><key>Red Component</key><real>0</real></dict></dict></plist>");
        let spec = parse_theme(&text).unwrap();
        assert_eq!(spec.ansi.unwrap()[0], AnsiColor::new(255, 128, 0));
        assert_eq!(spec.background, Some(AnsiColor::new(0, 26, 51)));
    }

    #[test]
    fn rejects_unknown_text() {
        assert!(parse_theme("hello world").is_err());
        assert!(parse_theme("{\"a\": 1}").is_err());
    }
}
