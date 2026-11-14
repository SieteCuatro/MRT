use egui::{Color32, FontData, FontDefinitions, FontFamily, Margin, Rounding, Stroke, Visuals};

pub struct Palette;

impl Palette {
    pub const BG_BASE: Color32 = Color32::from_rgb(13, 17, 23);       // #0D1117
    pub const BG_SURFACE: Color32 = Color32::from_rgb(22, 27, 34);    // #161B22
    pub const BG_ELEVATED: Color32 = Color32::from_rgb(28, 33, 40);   // #1C2128
    pub const BG_OVERLAY: Color32 = Color32::from_rgb(33, 38, 45);    // #21262D

    pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(48, 54, 61); // #30363D

    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(230, 237, 243);   // #E6EDF3
    pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(139, 148, 158); // #8B949E
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(110, 118, 129);     // #6E7681

    pub const ACCENT_BLUE: Color32 = Color32::from_rgb(88, 166, 255);     // #58A6FF
    pub const ACCENT_GREEN: Color32 = Color32::from_rgb(63, 185, 80);     // #3FB950
    pub const ACCENT_RED: Color32 = Color32::from_rgb(248, 81, 73);       // #F85149
    pub const ACCENT_AMBER: Color32 = Color32::from_rgb(210, 153, 34);    // #D29922
}

/// Applies MRT's cohesive dark theme, spacing, typography, and crisp system fonts to `egui::Context`.
pub fn apply_mrt_theme(ctx: &egui::Context) {
    install_system_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = Margin::same(14.0);
    style.spacing.menu_margin = Margin::same(8.0);
    style.spacing.indent = 16.0;
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating = true;

    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;
    visuals.panel_fill = Palette::BG_BASE;
    visuals.window_fill = Palette::BG_SURFACE;
    visuals.extreme_bg_color = Palette::BG_BASE;
    visuals.faint_bg_color = Palette::BG_ELEVATED;
    visuals.code_bg_color = Palette::BG_ELEVATED;

    visuals.window_rounding = Rounding::same(10.0);
    visuals.menu_rounding = Rounding::same(8.0);
    visuals.window_stroke = Stroke::new(1.0_f32, Palette::BORDER_SUBTLE);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 14.0,
        spread: 0.0,
        color: Color32::from_black_alpha(90),
    };

    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(31, 111, 235, 75);
    visuals.selection.stroke = Stroke::new(1.0_f32, Palette::ACCENT_BLUE);

    visuals.widgets.noninteractive.bg_fill = Palette::BG_SURFACE;
    visuals.widgets.noninteractive.weak_bg_fill = Palette::BG_SURFACE;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_SECONDARY);
    visuals.widgets.noninteractive.rounding = Rounding::same(6.0);

    visuals.widgets.inactive.bg_fill = Palette::BG_ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = Palette::BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Palette::BORDER_SUBTLE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Palette::TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = Rounding::same(6.0);

    visuals.widgets.hovered.bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.hovered.weak_bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(72, 79, 88));
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.hovered.rounding = Rounding::same(6.0);

    visuals.widgets.active.bg_fill = Color32::from_rgb(38, 48, 61);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(38, 48, 61);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, Palette::ACCENT_BLUE);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.active.rounding = Rounding::same(6.0);

    visuals.widgets.open.bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.open.weak_bg_fill = Palette::BG_OVERLAY;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, Palette::ACCENT_BLUE);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.widgets.open.rounding = Rounding::same(6.0);

    style.visuals = visuals;
    ctx.set_style(style);
}

fn install_system_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let mono_candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\CascadiaMono.ttf",
            r"C:\Windows\Fonts\CascadiaCode.ttf",
            r"C:\Windows\Fonts\consola.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Menlo.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        ]
    };

    for path in mono_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("mrt_mono".to_owned(), FontData::from_owned(bytes));
            if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
                list.insert(0, "mrt_mono".to_owned());
            }
            break;
        }
    }

    let ui_candidates: &[&str] = if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\arial.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &["/System/Library/Fonts/SFNS.ttf"]
    } else {
        &[
            "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
    };

    for path in ui_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("mrt_ui".to_owned(), FontData::from_owned(bytes));
            if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
                list.insert(0, "mrt_ui".to_owned());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}
