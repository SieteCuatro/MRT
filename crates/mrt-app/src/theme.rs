use egui::{Color32, Margin, Rounding, Stroke, Visuals};

/// MRT's UI colors. Each is read from the active theme ([`set_theme_colors`]), so a
/// theme file or `terminal.theme = "high-contrast"` restyles the whole window.
pub struct Palette;

/// One theme's UI colors (see [`Palette`] for what each is used for).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeColors {
    pub bg_sidebar: Color32,
    pub bg_base: Color32,
    pub bg_surface: Color32,
    pub bg_elevated: Color32,
    pub bg_panel: Color32,
    pub bg_inset: Color32,
    pub bg_overlay: Color32,
    pub bg_selected: Color32,
    pub border_faint: Color32,
    pub border_subtle: Color32,
    pub border_strong: Color32,
    pub text_primary: Color32,
    pub text_body: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub on_accent: Color32,
    pub accent_green: Color32,
    pub accent_red: Color32,
    pub accent_amber: Color32,
    pub ai_fill: Color32,
    pub ai_border: Color32,
    pub danger_fill: Color32,
    pub danger_border: Color32,
    pub failed_border: Color32,
}

/// "Graphite Calm": neutral graphite surfaces and one soft accent (the default).
pub const GRAPHITE_CALM: ThemeColors = ThemeColors {
    bg_sidebar: Color32::from_rgb(17, 17, 19),
    bg_base: Color32::from_rgb(23, 23, 26),
    bg_surface: Color32::from_rgb(27, 27, 31),
    bg_elevated: Color32::from_rgb(30, 30, 34),
    bg_panel: Color32::from_rgb(26, 26, 30),
    bg_inset: Color32::from_rgb(21, 21, 24),
    bg_overlay: Color32::from_rgb(38, 38, 43),
    bg_selected: Color32::from_rgb(34, 34, 39),
    border_faint: Color32::from_rgb(38, 38, 43),
    border_subtle: Color32::from_rgb(46, 46, 52),
    border_strong: Color32::from_rgb(52, 52, 59),
    text_primary: Color32::from_rgb(236, 236, 238),
    text_body: Color32::from_rgb(196, 196, 204),
    text_secondary: Color32::from_rgb(161, 161, 170),
    text_muted: Color32::from_rgb(138, 138, 147),
    accent: Color32::from_rgb(142, 162, 255),
    accent_text: Color32::from_rgb(184, 194, 255),
    on_accent: Color32::from_rgb(15, 18, 34),
    accent_green: Color32::from_rgb(74, 222, 128),
    accent_red: Color32::from_rgb(248, 113, 113),
    accent_amber: Color32::from_rgb(251, 191, 36),
    ai_fill: Color32::from_rgb(31, 33, 48),
    ai_border: Color32::from_rgb(44, 49, 80),
    danger_fill: Color32::from_rgb(34, 29, 26),
    danger_border: Color32::from_rgb(90, 58, 34),
    failed_border: Color32::from_rgb(58, 42, 44),
};

/// High contrast for low vision: black surfaces, white text, strong borders and
/// saturated accents.
pub const HIGH_CONTRAST: ThemeColors = ThemeColors {
    bg_sidebar: Color32::from_rgb(0, 0, 0),
    bg_base: Color32::from_rgb(0, 0, 0),
    bg_surface: Color32::from_rgb(10, 10, 10),
    bg_elevated: Color32::from_rgb(18, 18, 18),
    bg_panel: Color32::from_rgb(8, 8, 8),
    bg_inset: Color32::from_rgb(0, 0, 0),
    bg_overlay: Color32::from_rgb(45, 45, 45),
    bg_selected: Color32::from_rgb(30, 30, 70),
    border_faint: Color32::from_rgb(130, 130, 130),
    border_subtle: Color32::from_rgb(170, 170, 170),
    border_strong: Color32::from_rgb(230, 230, 230),
    text_primary: Color32::from_rgb(255, 255, 255),
    text_body: Color32::from_rgb(255, 255, 255),
    text_secondary: Color32::from_rgb(235, 235, 235),
    text_muted: Color32::from_rgb(205, 205, 205),
    accent: Color32::from_rgb(0, 200, 255),
    accent_text: Color32::from_rgb(130, 230, 255),
    on_accent: Color32::from_rgb(0, 0, 0),
    accent_green: Color32::from_rgb(0, 255, 110),
    accent_red: Color32::from_rgb(255, 90, 90),
    accent_amber: Color32::from_rgb(255, 225, 0),
    ai_fill: Color32::from_rgb(0, 20, 40),
    ai_border: Color32::from_rgb(0, 200, 255),
    danger_fill: Color32::from_rgb(40, 12, 0),
    danger_border: Color32::from_rgb(255, 150, 0),
    failed_border: Color32::from_rgb(255, 90, 90),
};

static THEME: std::sync::RwLock<ThemeColors> = std::sync::RwLock::new(GRAPHITE_CALM);

/// Makes `colors` the active UI theme.
pub fn set_theme_colors(colors: ThemeColors) {
    if let Ok(mut theme) = THEME.write() {
        *theme = colors;
    }
}

/// The active UI theme.
pub fn theme_colors() -> ThemeColors {
    THEME.read().map(|t| *t).unwrap_or(GRAPHITE_CALM)
}

impl ThemeColors {
    /// Sets one color by its palette name in lowercase (`bg_base`, `accent`...).
    /// Returns `false` for an unknown name.
    pub fn set(&mut self, name: &str, color: Color32) -> bool {
        match name {
            "bg_sidebar" => self.bg_sidebar = color,
            "bg_base" => self.bg_base = color,
            "bg_surface" => self.bg_surface = color,
            "bg_elevated" => self.bg_elevated = color,
            "bg_panel" => self.bg_panel = color,
            "bg_inset" => self.bg_inset = color,
            "bg_overlay" => self.bg_overlay = color,
            "bg_selected" => self.bg_selected = color,
            "border_faint" => self.border_faint = color,
            "border_subtle" => self.border_subtle = color,
            "border_strong" => self.border_strong = color,
            "text_primary" => self.text_primary = color,
            "text_body" => self.text_body = color,
            "text_secondary" => self.text_secondary = color,
            "text_muted" => self.text_muted = color,
            "accent" => self.accent = color,
            "accent_text" => self.accent_text = color,
            "on_accent" => self.on_accent = color,
            "accent_green" => self.accent_green = color,
            "accent_red" => self.accent_red = color,
            "accent_amber" => self.accent_amber = color,
            "ai_fill" => self.ai_fill = color,
            "ai_border" => self.ai_border = color,
            "danger_fill" => self.danger_fill = color,
            "danger_border" => self.danger_border = color,
            "failed_border" => self.failed_border = color,
            "accent_blue" => self.accent = color,
            _ => return false,
        }
        true
    }
}

#[allow(non_snake_case)]
impl Palette {
    pub fn BG_SIDEBAR() -> Color32 {
        theme_colors().bg_sidebar
    }
    pub fn BG_BASE() -> Color32 {
        theme_colors().bg_base
    }
    pub fn BG_SURFACE() -> Color32 {
        theme_colors().bg_surface
    }
    pub fn BG_ELEVATED() -> Color32 {
        theme_colors().bg_elevated
    }
    pub fn BG_PANEL() -> Color32 {
        theme_colors().bg_panel
    }
    pub fn BG_INSET() -> Color32 {
        theme_colors().bg_inset
    }
    pub fn BG_OVERLAY() -> Color32 {
        theme_colors().bg_overlay
    }
    pub fn BG_SELECTED() -> Color32 {
        theme_colors().bg_selected
    }
    pub fn BORDER_FAINT() -> Color32 {
        theme_colors().border_faint
    }
    pub fn BORDER_SUBTLE() -> Color32 {
        theme_colors().border_subtle
    }
    pub fn BORDER_STRONG() -> Color32 {
        theme_colors().border_strong
    }
    pub fn TEXT_PRIMARY() -> Color32 {
        theme_colors().text_primary
    }
    pub fn TEXT_BODY() -> Color32 {
        theme_colors().text_body
    }
    pub fn TEXT_SECONDARY() -> Color32 {
        theme_colors().text_secondary
    }
    pub fn TEXT_MUTED() -> Color32 {
        theme_colors().text_muted
    }
    pub fn ACCENT() -> Color32 {
        theme_colors().accent
    }
    pub fn ACCENT_TEXT() -> Color32 {
        theme_colors().accent_text
    }
    pub fn ON_ACCENT() -> Color32 {
        theme_colors().on_accent
    }
    pub fn ACCENT_GREEN() -> Color32 {
        theme_colors().accent_green
    }
    pub fn ACCENT_RED() -> Color32 {
        theme_colors().accent_red
    }
    pub fn ACCENT_AMBER() -> Color32 {
        theme_colors().accent_amber
    }
    pub fn AI_FILL() -> Color32 {
        theme_colors().ai_fill
    }
    pub fn AI_BORDER() -> Color32 {
        theme_colors().ai_border
    }
    pub fn DANGER_FILL() -> Color32 {
        theme_colors().danger_fill
    }
    pub fn DANGER_BORDER() -> Color32 {
        theme_colors().danger_border
    }
    pub fn FAILED_BORDER() -> Color32 {
        theme_colors().failed_border
    }
    pub fn ACCENT_BLUE() -> Color32 {
        theme_colors().accent
    }
}

/// Corner radii shared by every surface.
pub struct Radius;

impl Radius {
    pub const ICON: f32 = 8.0;
    pub const BUTTON: f32 = 10.0;
    pub const BLOCK: f32 = 14.0;
    pub const COMPOSER: f32 = 18.0;
}

/// Maximum width of the centered block stream and composer.
pub const STREAM_MAX_WIDTH: f32 = 880.0;

/// Frame used by every right-hand inspector panel.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(Palette::BG_PANEL())
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_FAINT()))
        .inner_margin(Margin::symmetric(16.0, 14.0))
}

/// Quiet section label used in the sidebar, panels and palette.
pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(text).size(12.0).color(Palette::TEXT_MUTED()));
    ui.add_space(2.0);
}

/// Small tinted chip (badges such as `JSON` or `exit 101`).
pub fn chip(ui: &mut egui::Ui, text: &str, fill: Color32, fg: Color32) {
    egui::Frame::none()
        .fill(fill)
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.0).strong().color(fg));
        });
}

/// Frameless icon button; `label` doubles as hover text.
pub fn icon_button(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(icon).size(14.0).color(Palette::TEXT_SECONDARY()))
            .frame(false)
            .min_size(egui::vec2(30.0, 30.0)),
    )
    .on_hover_text(label)
}

/// Quiet text button (no frame until hovered).
pub fn ghost_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).size(12.5).color(Palette::TEXT_SECONDARY()))
        .frame(false)
        .min_size(egui::vec2(0.0, 28.0))
}

/// Filled accent call-to-action button.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).strong().color(Palette::ON_ACCENT()))
        .fill(Palette::ACCENT())
        .stroke(Stroke::NONE)
        .rounding(Rounding::same(Radius::BUTTON))
        .min_size(egui::vec2(0.0, 32.0))
}

/// Outlined secondary button.
pub fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).color(Palette::TEXT_PRIMARY()))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG()))
        .rounding(Rounding::same(Radius::BUTTON))
        .min_size(egui::vec2(0.0, 32.0))
}

/// Applies MRT's "Graphite Calm" theme, spacing and typography to `egui::Context`.
/// Fonts are installed from the config by `config_runtime` (`fonts.rs`).
pub fn apply_mrt_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 28.0;
    style.spacing.window_margin = Margin::same(16.0);
    style.spacing.menu_margin = Margin::same(8.0);
    style.spacing.indent = 16.0;
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating = true;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.5));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(16.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));

    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;
    visuals.panel_fill = Palette::BG_BASE();
    visuals.window_fill = Palette::BG_ELEVATED();
    visuals.extreme_bg_color = Palette::BG_INSET();
    visuals.faint_bg_color = Palette::BG_SURFACE();
    visuals.code_bg_color = Palette::BG_INSET();
    visuals.hyperlink_color = Palette::ACCENT_TEXT();

    visuals.window_rounding = Rounding::same(Radius::COMPOSER);
    visuals.menu_rounding = Rounding::same(12.0);
    visuals.window_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG());
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 16.0),
        blur: 48.0,
        spread: 0.0,
        color: Color32::from_black_alpha(140),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(110),
    };

    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(142, 162, 255, 60);
    visuals.selection.stroke = Stroke::new(1.0_f32, Palette::ACCENT());
    visuals.text_cursor.stroke = Stroke::new(2.0_f32, Palette::ACCENT());

    let r = Rounding::same(Radius::BUTTON);
    visuals.widgets.noninteractive.bg_fill = Palette::BG_SURFACE();
    visuals.widgets.noninteractive.weak_bg_fill = Palette::BG_SURFACE();
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_FAINT());
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_BODY());
    visuals.widgets.noninteractive.rounding = r;

    visuals.widgets.inactive.bg_fill = Palette::BG_INSET();
    visuals.widgets.inactive.weak_bg_fill = Palette::BG_ELEVATED();
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_SUBTLE());
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY());
    visuals.widgets.inactive.rounding = r;

    visuals.widgets.hovered.bg_fill = Palette::BG_OVERLAY();
    visuals.widgets.hovered.weak_bg_fill = Palette::BG_OVERLAY();
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG());
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.hovered.rounding = r;
    visuals.widgets.hovered.expansion = 0.0;

    visuals.widgets.active.bg_fill = Palette::BG_SELECTED();
    visuals.widgets.active.weak_bg_fill = Palette::BG_SELECTED();
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Palette::ACCENT());
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.active.rounding = r;
    visuals.widgets.active.expansion = 0.0;

    visuals.widgets.open.bg_fill = Palette::BG_OVERLAY();
    visuals.widgets.open.weak_bg_fill = Palette::BG_OVERLAY();
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG());
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.open.rounding = r;

    style.visuals = visuals;
    ctx.set_style(style);
}

fn rgb(c: mrt_core::AnsiColor) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// `a` moved towards `b` by `t` (0–1).
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn luminance(c: Color32) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

/// UI colors for a theme that only sets terminal colors (an imported terminal
/// scheme): surfaces are steps from the background towards the foreground,
/// text steps from the foreground back, and accents come from the ANSI colors.
pub fn derive_from_terminal(
    background: Color32,
    foreground: Color32,
    ansi: Option<&[mrt_core::AnsiColor; 16]>,
) -> ThemeColors {
    let to_fg = |t: f32| mix(background, foreground, t);
    let to_bg = |t: f32| mix(foreground, background, t);
    let dark = luminance(background) < 0.5;
    let shade = if dark { Color32::BLACK } else { Color32::WHITE };
    let pick = |i: usize, fallback: Color32| ansi.map(|a| rgb(a[i])).unwrap_or(fallback);
    let accent = pick(12, pick(4, GRAPHITE_CALM.accent));
    ThemeColors {
        bg_sidebar: mix(background, shade, 0.25),
        bg_base: background,
        bg_surface: to_fg(0.04),
        bg_elevated: to_fg(0.06),
        bg_panel: to_fg(0.03),
        bg_inset: mix(background, shade, 0.12),
        bg_overlay: to_fg(0.12),
        bg_selected: to_fg(0.09),
        border_faint: to_fg(0.12),
        border_subtle: to_fg(0.18),
        border_strong: to_fg(0.24),
        text_primary: foreground,
        text_body: to_bg(0.12),
        text_secondary: to_bg(0.28),
        text_muted: to_bg(0.38),
        accent,
        accent_text: mix(accent, foreground, 0.35),
        on_accent: background,
        accent_green: pick(2, GRAPHITE_CALM.accent_green),
        accent_red: pick(1, GRAPHITE_CALM.accent_red),
        accent_amber: pick(3, GRAPHITE_CALM.accent_amber),
        ai_fill: mix(background, accent, 0.08),
        ai_border: mix(background, accent, 0.25),
        danger_fill: mix(background, pick(3, GRAPHITE_CALM.accent_amber), 0.06),
        danger_border: mix(background, pick(3, GRAPHITE_CALM.accent_amber), 0.3),
        failed_border: mix(background, pick(1, GRAPHITE_CALM.accent_red), 0.25),
    }
}

/// A resolved theme: UI colors plus the 16 ANSI colors, if the theme sets them.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    pub name: String,
    pub colors: ThemeColors,
    pub ansi: Option<[mrt_core::AnsiColor; 16]>,
    /// `[ui]` keys the theme set that MRT does not know.
    pub unknown_keys: Vec<String>,
}

/// Applies a parsed theme: terminal colors first (deriving the UI from them when
/// the theme has a background), then explicit `[ui]` overrides.
pub fn theme_from_spec(spec: &mrt_core::ThemeSpec, base: ThemeColors) -> ResolvedTheme {
    let mut colors = match spec.background {
        Some(bg) => derive_from_terminal(rgb(bg), spec.foreground.map(rgb).unwrap_or(base.text_primary), spec.ansi.as_ref()),
        None => base,
    };
    if let Some(fg) = spec.foreground {
        colors.text_body = rgb(fg);
    }
    if let Some(bg) = spec.background {
        // The terminal grid and command output wells use the scheme's own background.
        colors.bg_inset = rgb(bg);
    }
    let mut unknown_keys = Vec::new();
    for (key, color) in &spec.ui {
        if !colors.set(key, rgb(*color)) {
            unknown_keys.push(key.clone());
        }
    }
    ResolvedTheme { name: spec.name.clone().unwrap_or_else(|| "custom".into()), colors, ansi: spec.ansi, unknown_keys }
}

/// Built-in theme names.
pub const BUILT_IN_THEMES: &[&str] = &["graphite-calm", "dark", "default", "high-contrast"];

/// Resolves `terminal.theme`: a built-in name, or a theme file (a path, or a name
/// looked up next to the config file and in its `themes/` folder, with or without
/// `.toml`).
pub fn resolve_theme(setting: Option<&str>, config_dir: Option<&std::path::Path>) -> Result<ResolvedTheme, String> {
    let value = setting.map(str::trim).filter(|v| !v.is_empty()).unwrap_or("graphite-calm");
    let built_in = |name: &str, colors| ResolvedTheme { name: name.into(), colors, ansi: None, unknown_keys: Vec::new() };
    match value.to_ascii_lowercase().as_str() {
        "graphite-calm" | "dark" | "default" => return Ok(built_in("graphite-calm", GRAPHITE_CALM)),
        "high-contrast" | "high_contrast" => return Ok(built_in("high-contrast", HIGH_CONTRAST)),
        _ => {}
    }
    let mut candidates = vec![std::path::PathBuf::from(value)];
    if let Some(dir) = config_dir {
        candidates.push(dir.join(value));
        candidates.push(dir.join("themes").join(value));
        candidates.push(dir.join("themes").join(format!("{value}.toml")));
    }
    let path = candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("theme \"{value}\" is neither a built-in theme ({}) nor a file", BUILT_IN_THEMES.join(", ")))?;
    let spec = mrt_core::load_theme_file(&path)?;
    Ok(theme_from_spec(&spec, GRAPHITE_CALM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_themes_and_missing_files() {
        assert_eq!(resolve_theme(None, None).unwrap().colors, GRAPHITE_CALM);
        assert_eq!(resolve_theme(Some("High-Contrast"), None).unwrap().colors, HIGH_CONTRAST);
        assert!(resolve_theme(Some("no-such-theme-mrt"), None).is_err());
    }

    #[test]
    fn theme_files_derive_the_ui_and_apply_overrides() {
        let dir = std::env::temp_dir().join(format!("mrt-theme-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(
            dir.join("themes").join("night.toml"),
            "[ui]\naccent = \"#ff0000\"\nsparkle = \"#00ff00\"\n[terminal]\nbackground = \"#101010\"\nforeground = \"#e0e0e0\"\n",
        )
        .unwrap();
        let theme = resolve_theme(Some("night"), Some(&dir)).unwrap();
        assert_eq!(theme.name, "night");
        assert_eq!(theme.colors.bg_base, Color32::from_rgb(16, 16, 16));
        assert_eq!(theme.colors.bg_inset, Color32::from_rgb(16, 16, 16), "the terminal uses the scheme background");
        assert_eq!(theme.colors.text_primary, Color32::from_rgb(224, 224, 224));
        assert_eq!(theme.colors.accent, Color32::from_rgb(255, 0, 0), "[ui] wins over derived colors");
        assert_eq!(theme.unknown_keys, vec!["sparkle".to_string()]);
        assert!(luminance(theme.colors.bg_surface) > luminance(theme.colors.bg_base), "surfaces step towards the foreground");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn high_contrast_text_is_far_from_its_background() {
        let c = HIGH_CONTRAST;
        assert!(luminance(c.text_primary) - luminance(c.bg_base) > 0.9);
        assert!(luminance(c.text_muted) - luminance(c.bg_surface) > 0.7);
    }
}
