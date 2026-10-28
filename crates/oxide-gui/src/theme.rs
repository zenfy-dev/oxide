//! Visual style: neutral surfaces, one accent colour, rounded controls.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme, Visuals,
};

/// Accent used for selection, focus and primary buttons.
pub const ACCENT: Color32 = Color32::from_rgb(214, 96, 46);

/// Installs light and dark styles and follows the OS theme by default.
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.window_margin = Margin::same(18);
        style.spacing.menu_margin = Margin::same(6);
        style.spacing.combo_width = 180.0;
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(19.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
    ctx.set_theme(egui::ThemePreference::System);
}

fn visuals(dark: bool) -> Visuals {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let (panel, window, faint, extreme, border, text) = if dark {
        (
            Color32::from_rgb(28, 28, 30),
            Color32::from_rgb(36, 36, 39),
            Color32::from_rgb(34, 34, 37),
            Color32::from_rgb(22, 22, 24),
            Color32::from_rgb(58, 58, 62),
            Color32::from_rgb(228, 228, 232),
        )
    } else {
        (
            Color32::from_rgb(246, 246, 247),
            Color32::from_rgb(255, 255, 255),
            Color32::from_rgb(240, 240, 242),
            Color32::from_rgb(255, 255, 255),
            Color32::from_rgb(218, 218, 223),
            Color32::from_rgb(28, 28, 30),
        )
    };
    v.panel_fill = panel;
    v.window_fill = window;
    v.faint_bg_color = faint;
    v.extreme_bg_color = extreme;
    v.override_text_color = Some(text);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(if dark { 0.28 } else { 0.16 });
    v.selection.stroke = Stroke::new(1.0, if dark { Color32::WHITE } else { Color32::BLACK });
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, border);
    v.window_shadow =
        Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 45 }) };
    v.popup_shadow =
        Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(if dark { 90 } else { 30 }) };
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    v.widgets.inactive.weak_bg_fill = window;
    v.widgets.inactive.bg_fill = faint;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    // Only the file table opts into stripes; forms stay clean.
    v.striped = false;
    v
}

/// A filled accent button for the primary action in a dialog.
pub fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(text).color(Color32::WHITE)).fill(ACCENT)
}

/// Muted secondary text colour for the current theme.
pub fn weak(ui: &egui::Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

/// The application icon at 256×256, as raw RGBA generated from
/// `assets/icon-source.png` by `packaging/gen_icons.py` (no image decoder needed).
const ICON_RGBA: &[u8] = include_bytes!("../../../assets/icon-256.rgba");
pub const ICON_SIZE: u32 = 256;

/// The window/taskbar icon.
pub fn app_icon() -> egui::IconData {
    egui::IconData { rgba: ICON_RGBA.to_vec(), width: ICON_SIZE, height: ICON_SIZE }
}

/// The icon as an image for drawing inside the window.
pub fn icon_image() -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([ICON_SIZE as usize; 2], ICON_RGBA)
}
