//! Font selection: the configured monospace font (`terminal.font_family`), a fallback
//! chain for glyphs it lacks (`terminal.font_fallback`, then platform symbol fonts),
//! and the lookup of family names in the system font folders.

use egui::{FontData, FontDefinitions, FontFamily};
use std::path::{Path, PathBuf};

/// Font preferences from the config file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FontPrefs {
    pub family: Option<String>,
    pub fallback: Vec<String>,
}

/// What [`build_font_definitions`] loaded, for the config notice.
#[derive(Debug, Default, PartialEq)]
pub struct FontReport {
    /// Configured fonts that were not found or could not be read.
    pub missing: Vec<String>,
}

const MONO_DEFAULTS: &[&str] = if cfg!(target_os = "windows") {
    &[r"C:\Windows\Fonts\CascadiaMono.ttf", r"C:\Windows\Fonts\CascadiaCode.ttf", r"C:\Windows\Fonts\consola.ttf"]
} else if cfg!(target_os = "macos") {
    &["/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Menlo.ttc"]
} else {
    &[
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ]
};

const UI_DEFAULTS: &[&str] = if cfg!(target_os = "windows") {
    &[r"C:\Windows\Fonts\segoeui.ttf", r"C:\Windows\Fonts\arial.ttf"]
} else if cfg!(target_os = "macos") {
    &["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"]
} else {
    &[
        "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
        "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]
};

/// Symbol fonts tried after the configured fallbacks, so box drawing, arrows and
/// common symbols render even when the main font lacks them.
const SYMBOL_DEFAULTS: &[&str] = if cfg!(target_os = "windows") {
    &[r"C:\Windows\Fonts\seguisym.ttf"]
} else if cfg!(target_os = "macos") {
    &["/System/Library/Fonts/Apple Symbols.ttf"]
} else {
    &[
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]
};

/// Folders searched for a font given by family name.
pub fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    if cfg!(target_os = "windows") {
        let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = &home {
            dirs.push(home.join("Library").join("Fonts"));
        }
    } else {
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(data).join("fonts"));
        }
        if let Some(home) = &home {
            dirs.push(home.join(".local").join("share").join("fonts"));
            dirs.push(home.join(".fonts"));
        }
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        dirs.push(PathBuf::from("/usr/share/fonts"));
    }
    dirs
}

/// Lowercase letters and digits only: `"JetBrains Mono"` and `JetBrainsMono-Regular`
/// compare as `jetbrainsmono` and `jetbrainsmonoregular`.
fn normalize(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// How well a font file name (without extension) matches a family name: `Some(0)`
/// for the regular face, higher for other faces, `None` for no match.
fn match_rank(stem: &str, family: &str) -> Option<u8> {
    let stem = normalize(stem);
    let family = normalize(family);
    if family.is_empty() {
        return None;
    }
    match stem.strip_prefix(&family)? {
        "" | "regular" | "book" | "r" => Some(0),
        "medium" | "mono" => Some(1),
        // Other faces of the same family (bold, italic, light...): usable but not preferred.
        rest if ["bold", "italic", "light", "thin", "black", "semi", "extra", "oblique"]
            .iter()
            .any(|face| rest.contains(face)) =>
        {
            Some(5)
        }
        _ => None,
    }
}

fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc"))
}

/// Finds a font: an existing file path is used as is; otherwise `name` is a family
/// name matched against the font files under `dirs` (searched two levels deep), and
/// the regular face wins over bold or italic ones.
pub fn resolve_font(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let as_path = Path::new(name);
    if is_font_file(as_path) && as_path.is_file() {
        return Some(as_path.to_path_buf());
    }
    let mut best: Option<(u8, PathBuf)> = None;
    let mut consider = |path: PathBuf| {
        if !is_font_file(&path) {
            return;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { return };
        if let Some(rank) = match_rank(stem, name) {
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, path));
            }
        }
    };
    let mut stack: Vec<(PathBuf, u8)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 3 {
                    stack.push((path, depth + 1));
                }
            } else {
                consider(path);
            }
        }
    }
    best.map(|(_, path)| path)
}

/// Builds egui's font list: the configured or default monospace font first, then the
/// configured fallbacks and platform symbol fonts, then egui's built-in fonts.
pub fn build_font_definitions(prefs: &FontPrefs) -> (FontDefinitions, FontReport) {
    let mut fonts = FontDefinitions::default();
    let mut report = FontReport::default();
    let dirs = system_font_dirs();
    let mut mono: Vec<String> = Vec::new();

    let load = |fonts: &mut FontDefinitions, key: String, path: &Path| -> bool {
        match std::fs::read(path) {
            Ok(bytes) => {
                fonts.font_data.insert(key, FontData::from_owned(bytes));
                true
            }
            Err(_) => false,
        }
    };

    let configured = prefs.family.as_deref().filter(|f| !f.trim().is_empty());
    let mut main_loaded = false;
    if let Some(family) = configured {
        match resolve_font(family, &dirs) {
            Some(path) if load(&mut fonts, "mrt_mono".into(), &path) => main_loaded = true,
            _ => report.missing.push(family.to_string()),
        }
    }
    if !main_loaded {
        main_loaded = MONO_DEFAULTS.iter().any(|p| load(&mut fonts, "mrt_mono".into(), Path::new(p)));
    }
    if main_loaded {
        mono.push("mrt_mono".into());
    }

    for (i, name) in prefs.fallback.iter().enumerate() {
        let key = format!("mrt_fallback_{i}");
        match resolve_font(name, &dirs) {
            Some(path) if load(&mut fonts, key.clone(), &path) => mono.push(key),
            _ => report.missing.push(name.clone()),
        }
    }
    for (i, path) in SYMBOL_DEFAULTS.iter().enumerate() {
        let key = format!("mrt_symbols_{i}");
        if load(&mut fonts, key.clone(), Path::new(path)) {
            mono.push(key);
        }
    }

    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        for (i, key) in mono.iter().enumerate() {
            list.insert(i, key.clone());
        }
    }

    if UI_DEFAULTS.iter().any(|p| load(&mut fonts, "mrt_ui".into(), Path::new(p))) {
        if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
            list.insert(0, "mrt_ui".into());
        }
    }
    // Symbols in UI text (sidebar, palette) fall back to the same fonts.
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        for key in mono.iter().filter(|k| k.starts_with("mrt_symbols") || k.starts_with("mrt_fallback")) {
            list.push(key.clone());
        }
    }
    (fonts, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names_match_file_names() {
        assert_eq!(match_rank("JetBrainsMono-Regular", "JetBrains Mono"), Some(0));
        assert_eq!(match_rank("CascadiaMono", "Cascadia Mono"), Some(0));
        assert_eq!(match_rank("JetBrainsMonoNerdFont-Regular", "JetBrainsMono Nerd Font"), Some(0));
        assert_eq!(match_rank("JetBrainsMono-BoldItalic", "JetBrains Mono"), Some(5));
        assert_eq!(match_rank("JetBrainsMonoNL-Regular", "JetBrains Mono"), None);
        assert_eq!(match_rank("Arial", "JetBrains Mono"), None);
        assert_eq!(match_rank("Arial", ""), None);
    }

    #[test]
    fn resolve_prefers_the_regular_face_and_accepts_paths() {
        let dir = std::env::temp_dir().join(format!("mrt-fonts-{}", uuid::Uuid::new_v4()));
        let nested = dir.join("truetype").join("fira");
        std::fs::create_dir_all(&nested).unwrap();
        for name in ["FiraCode-Bold.ttf", "FiraCode-Regular.ttf", "FiraCode-Regular.txt", "Other.otf"] {
            std::fs::write(nested.join(name), b"not really a font").unwrap();
        }
        let dirs = vec![dir.clone()];
        assert_eq!(resolve_font("Fira Code", &dirs), Some(nested.join("FiraCode-Regular.ttf")));
        assert_eq!(resolve_font("Missing Font", &dirs), None);
        let path = nested.join("Other.otf");
        assert_eq!(resolve_font(path.to_str().unwrap(), &[]), Some(path));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_configured_fonts_are_reported() {
        let prefs = FontPrefs {
            family: Some("No Such Font MRT".into()),
            fallback: vec!["Also Missing MRT".into()],
        };
        let (fonts, report) = build_font_definitions(&prefs);
        assert_eq!(report.missing, vec!["No Such Font MRT".to_string(), "Also Missing MRT".to_string()]);
        // egui's own fonts are still there.
        assert!(!fonts.families[&FontFamily::Monospace].is_empty());
    }
}
