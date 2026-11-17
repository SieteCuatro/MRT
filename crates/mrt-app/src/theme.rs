use egui::{Color32, Margin, Rounding, Stroke, Visuals};

pub struct Palette;

/// "Graphite Calm" design tokens: neutral graphite surfaces and one soft accent.
impl Palette {
    pub const BG_SIDEBAR: Color32 = Color32::from_rgb(17, 17, 19); // #111113
    pub const BG_BASE: Color32 = Color32::from_rgb(23, 23, 26); // #17171A
    pub const BG_SURFACE: Color32 = Color32::from_rgb(27, 27, 31); // #1B1B1F (blocks)
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(30, 30, 34); // #1E1E22 (composer, popups)
    pub const BG_PANEL: Color32 = Color32::from_rgb(26, 26, 30); // #1A1A1E (inspector panels)
    pub const BG_INSET: Color32 = Color32::from_rgb(21, 21, 24); // #151518 (code wells)
    pub const BG_OVERLAY: Color32 = Color32::from_rgb(38, 38, 43); // #26262B (hover)
    pub const BG_SELECTED: Color32 = Color32::from_rgb(34, 34, 39); // #222227 (active row)

    pub const BORDER_FAINT: Color32 = Color32::from_rgb(38, 38, 43); // #26262B
    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(46, 46, 52); // #2E2E34
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(52, 52, 59); // #34343B

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(236, 236, 238); // #ECECEE
    pub const TEXT_BODY: Color32 = Color32::from_rgb(196, 196, 204); // #C4C4CC
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(161, 161, 170); // #A1A1AA
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(138, 138, 147); // #8A8A93

    pub const ACCENT: Color32 = Color32::from_rgb(142, 162, 255); // #8EA2FF
    pub const ACCENT_TEXT: Color32 = Color32::from_rgb(184, 194, 255); // #B8C2FF
    pub const ON_ACCENT: Color32 = Color32::from_rgb(15, 18, 34); // #0F1222
    pub const ACCENT_BLUE: Color32 = Self::ACCENT;
    pub const ACCENT_GREEN: Color32 = Color32::from_rgb(74, 222, 128); // #4ADE80
    pub const ACCENT_RED: Color32 = Color32::from_rgb(248, 113, 113); // #F87171
    pub const ACCENT_AMBER: Color32 = Color32::from_rgb(251, 191, 36); // #FBBF24

    pub const AI_FILL: Color32 = Color32::from_rgb(31, 33, 48); // #1F2130
    pub const AI_BORDER: Color32 = Color32::from_rgb(44, 49, 80); // #2C3150
    pub const DANGER_FILL: Color32 = Color32::from_rgb(34, 29, 26); // #221D1A
    pub const DANGER_BORDER: Color32 = Color32::from_rgb(90, 58, 34); // #5A3A22
    pub const FAILED_BORDER: Color32 = Color32::from_rgb(58, 42, 44); // #3A2A2C
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
        .fill(Palette::BG_PANEL)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_FAINT))
        .inner_margin(Margin::symmetric(16.0, 14.0))
}

/// Quiet section label used in the sidebar, panels and palette.
pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(text).size(12.0).color(Palette::TEXT_MUTED));
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
        egui::Button::new(egui::RichText::new(icon).size(14.0).color(Palette::TEXT_SECONDARY))
            .frame(false)
            .min_size(egui::vec2(30.0, 30.0)),
    )
    .on_hover_text(label)
}

/// Quiet text button (no frame until hovered).
pub fn ghost_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).size(12.5).color(Palette::TEXT_SECONDARY))
        .frame(false)
        .min_size(egui::vec2(0.0, 28.0))
}

/// Filled accent call-to-action button.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).strong().color(Palette::ON_ACCENT))
        .fill(Palette::ACCENT)
        .stroke(Stroke::NONE)
        .rounding(Rounding::same(Radius::BUTTON))
        .min_size(egui::vec2(0.0, 32.0))
}

/// Outlined secondary button.
pub fn secondary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).color(Palette::TEXT_PRIMARY))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0_f32, Palette::BORDER_STRONG))
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
    visuals.panel_fill = Palette::BG_BASE;
    visuals.window_fill = Palette::BG_ELEVATED;
    visuals.extreme_bg_color = Palette::BG_INSET;
    visuals.faint_bg_color = Palette::BG_SURFACE;
    visuals.code_bg_color = Palette::BG_INSET;
    visuals.hyperlink_color = Palette::ACCENT_TEXT;

    visuals.window_rounding = Rounding::same(Radius::COMPOSER);
    visuals.menu_rounding = Rounding::same(12.0);
    visuals.window_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG);
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
    visuals.selection.stroke = Stroke::new(1.0_f32, Palette::ACCENT);
    visuals.text_cursor.stroke = Stroke::new(2.0_f32, Palette::ACCENT);

    let r = Rounding::same(Radius::BUTTON);
    visuals.widgets.noninteractive.bg_fill = Palette::BG_SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = Palette::BG_SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_FAINT);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_BODY);
    visuals.widgets.noninteractive.rounding = r;

    visuals.widgets.inactive.bg_fill = Palette::BG_INSET;
    visuals.widgets.inactive.weak_bg_fill = Palette::BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_SUBTLE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = r;

    visuals.widgets.hovered.bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.hovered.weak_bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.hovered.rounding = r;
    visuals.widgets.hovered.expansion = 0.0;

    visuals.widgets.active.bg_fill = Palette::BG_SELECTED;
    visuals.widgets.active.weak_bg_fill = Palette::BG_SELECTED;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Palette::ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.active.rounding = r;
    visuals.widgets.active.expansion = 0.0;

    visuals.widgets.open.bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.open.weak_bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_STRONG);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.open.rounding = r;

    style.visuals = visuals;
    ctx.set_style(style);
}
