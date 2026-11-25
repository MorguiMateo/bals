use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

use crate::config::UiTheme;

/// Paleta semántica completa para el chasis hardware CRT exterior y la pantalla CRT interior.
#[derive(Debug, Clone, Copy)]
pub struct RetroPalette {
    // Carcasa física exterior (estilo Opción C)
    pub chassis_bg: Color32,
    pub chassis_highlight: Color32,
    pub chassis_shadow: Color32,
    pub chassis_outline: Color32,
    pub chassis_text: Color32,
    pub chassis_muted: Color32,
    pub chassis_btn_bg: Color32,
    pub chassis_btn_hover: Color32,

    // Pantalla CRT interior (estilo Opción A + C)
    pub crt_bg: Color32,
    pub crt_recess: Color32,
    pub crt_panel_bg: Color32,
    pub crt_panel_border: Color32,
    pub crt_widget_bg: Color32,
    pub crt_widget_hover: Color32,
    pub crt_text: Color32,
    pub crt_muted: Color32,
    pub crt_scanline: Color32,

    // Acentos fósforo, LEDs y alertas
    pub accent: Color32,
    pub accent_bright: Color32,
    pub accent_text: Color32,
    pub amber: Color32,
    pub danger: Color32,
    pub danger_text: Color32,
    pub led_green: Color32,
    pub led_red: Color32,
}

impl RetroPalette {
    pub const fn for_theme(theme: UiTheme) -> Self {
        match theme {
            UiTheme::CrtPhosphorGreen => Self {
                chassis_bg: Color32::from_rgb(224, 212, 186),
                chassis_highlight: Color32::from_rgb(245, 238, 220),
                chassis_shadow: Color32::from_rgb(174, 159, 130),
                chassis_outline: Color32::from_rgb(40, 33, 27),
                chassis_text: Color32::from_rgb(36, 30, 24),
                chassis_muted: Color32::from_rgb(106, 94, 76),
                chassis_btn_bg: Color32::from_rgb(235, 225, 203),
                chassis_btn_hover: Color32::from_rgb(246, 239, 222),

                crt_bg: Color32::from_rgb(15, 21, 19),
                crt_recess: Color32::from_rgb(9, 13, 12),
                crt_panel_bg: Color32::from_rgb(21, 29, 26),
                crt_panel_border: Color32::from_rgb(216, 206, 181),
                crt_widget_bg: Color32::from_rgb(31, 43, 39),
                crt_widget_hover: Color32::from_rgb(42, 58, 52),
                crt_text: Color32::from_rgb(240, 231, 209),
                crt_muted: Color32::from_rgb(142, 163, 151),
                crt_scanline: Color32::from_rgba_premultiplied(0, 0, 0, 38),

                accent: Color32::from_rgb(52, 211, 153),
                accent_bright: Color32::from_rgb(84, 240, 179),
                accent_text: Color32::from_rgb(9, 25, 19),
                amber: Color32::from_rgb(245, 158, 11),
                danger: Color32::from_rgb(209, 67, 67),
                danger_text: Color32::from_rgb(255, 242, 242),
                led_green: Color32::from_rgb(74, 222, 128),
                led_red: Color32::from_rgb(248, 86, 86),
            },
            UiTheme::CrtAmber80s => Self {
                chassis_bg: Color32::from_rgb(58, 52, 46),
                chassis_highlight: Color32::from_rgb(84, 76, 68),
                chassis_shadow: Color32::from_rgb(34, 30, 26),
                chassis_outline: Color32::from_rgb(18, 15, 12),
                chassis_text: Color32::from_rgb(242, 224, 194),
                chassis_muted: Color32::from_rgb(168, 150, 124),
                chassis_btn_bg: Color32::from_rgb(74, 66, 58),
                chassis_btn_hover: Color32::from_rgb(92, 82, 72),

                crt_bg: Color32::from_rgb(18, 13, 8),
                crt_recess: Color32::from_rgb(11, 8, 5),
                crt_panel_bg: Color32::from_rgb(27, 20, 12),
                crt_panel_border: Color32::from_rgb(220, 160, 68),
                crt_widget_bg: Color32::from_rgb(43, 32, 19),
                crt_widget_hover: Color32::from_rgb(58, 43, 25),
                crt_text: Color32::from_rgb(252, 232, 196),
                crt_muted: Color32::from_rgb(176, 144, 96),
                crt_scanline: Color32::from_rgba_premultiplied(0, 0, 0, 42),

                accent: Color32::from_rgb(255, 176, 20),
                accent_bright: Color32::from_rgb(255, 204, 84),
                accent_text: Color32::from_rgb(24, 15, 4),
                amber: Color32::from_rgb(255, 140, 32),
                danger: Color32::from_rgb(220, 68, 55),
                danger_text: Color32::from_rgb(255, 245, 238),
                led_green: Color32::from_rgb(255, 196, 48),
                led_red: Color32::from_rgb(255, 84, 72),
            },
            UiTheme::ClassicConsole => Self {
                chassis_bg: Color32::from_rgb(209, 208, 215),
                chassis_highlight: Color32::from_rgb(236, 235, 241),
                chassis_shadow: Color32::from_rgb(154, 152, 165),
                chassis_outline: Color32::from_rgb(35, 34, 48),
                chassis_text: Color32::from_rgb(34, 33, 46),
                chassis_muted: Color32::from_rgb(95, 93, 114),
                chassis_btn_bg: Color32::from_rgb(224, 223, 230),
                chassis_btn_hover: Color32::from_rgb(238, 237, 244),

                crt_bg: Color32::from_rgb(17, 19, 32),
                crt_recess: Color32::from_rgb(10, 11, 20),
                crt_panel_bg: Color32::from_rgb(25, 28, 46),
                crt_panel_border: Color32::from_rgb(158, 152, 214),
                crt_widget_bg: Color32::from_rgb(38, 42, 68),
                crt_widget_hover: Color32::from_rgb(51, 56, 90),
                crt_text: Color32::from_rgb(238, 236, 248),
                crt_muted: Color32::from_rgb(148, 146, 180),
                crt_scanline: Color32::from_rgba_premultiplied(0, 0, 0, 36),

                accent: Color32::from_rgb(138, 122, 255),
                accent_bright: Color32::from_rgb(168, 156, 255),
                accent_text: Color32::from_rgb(14, 12, 28),
                amber: Color32::from_rgb(250, 184, 45),
                danger: Color32::from_rgb(222, 56, 86),
                danger_text: Color32::from_rgb(255, 244, 246),
                led_green: Color32::from_rgb(74, 222, 128),
                led_red: Color32::from_rgb(244, 63, 94),
            },
            UiTheme::ArcadeNeon => Self {
                chassis_bg: Color32::from_rgb(30, 34, 42),
                chassis_highlight: Color32::from_rgb(50, 56, 68),
                chassis_shadow: Color32::from_rgb(16, 18, 23),
                chassis_outline: Color32::from_rgb(8, 10, 13),
                chassis_text: Color32::from_rgb(226, 236, 245),
                chassis_muted: Color32::from_rgb(126, 142, 162),
                chassis_btn_bg: Color32::from_rgb(40, 46, 56),
                chassis_btn_hover: Color32::from_rgb(54, 62, 76),

                crt_bg: Color32::from_rgb(9, 12, 17),
                crt_recess: Color32::from_rgb(5, 7, 10),
                crt_panel_bg: Color32::from_rgb(15, 21, 30),
                crt_panel_border: Color32::from_rgb(56, 189, 248),
                crt_widget_bg: Color32::from_rgb(23, 33, 46),
                crt_widget_hover: Color32::from_rgb(33, 47, 66),
                crt_text: Color32::from_rgb(230, 246, 255),
                crt_muted: Color32::from_rgb(124, 162, 184),
                crt_scanline: Color32::from_rgba_premultiplied(0, 0, 0, 40),

                accent: Color32::from_rgb(34, 211, 238),
                accent_bright: Color32::from_rgb(103, 232, 249),
                accent_text: Color32::from_rgb(7, 22, 28),
                amber: Color32::from_rgb(250, 204, 21),
                danger: Color32::from_rgb(244, 63, 94),
                danger_text: Color32::from_rgb(255, 242, 246),
                led_green: Color32::from_rgb(52, 245, 168),
                led_red: Color32::from_rgb(251, 79, 110),
            },
        }
    }
}

/// Aplica el estilo tipográfico monoespaciado retro y los colores de la paleta al contexto de `egui`.
pub fn apply_egui_theme(ctx: &egui::Context, p: &RetroPalette) {
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();

    // Tipografía 100% monoespaciada (Hack embebida) para una estética nítida de consola CRT.
    style.text_styles = [
        (TextStyle::Heading, FontId::new(17.5, FontFamily::Monospace)),
        (TextStyle::Body, FontId::new(12.5, FontFamily::Monospace)),
        (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        (TextStyle::Button, FontId::new(12.0, FontFamily::Monospace)),
        (TextStyle::Small, FontId::new(10.5, FontFamily::Monospace)),
    ]
    .into();

    style.spacing.item_spacing = egui::vec2(7.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.slider_width = 180.0;

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = p.crt_bg;
    visuals.window_fill = p.crt_bg;
    visuals.extreme_bg_color = p.crt_recess;
    visuals.faint_bg_color = p.crt_panel_bg;
    visuals.override_text_color = Some(p.crt_text);

    visuals.selection.bg_fill = p.accent;
    visuals.selection.stroke = Stroke::new(1.0, p.accent_text);

    let r = CornerRadius::same(3);
    visuals.widgets.noninteractive.bg_fill = p.crt_panel_bg;
    visuals.widgets.noninteractive.weak_bg_fill = p.crt_panel_bg;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.crt_widget_hover);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.crt_text);
    visuals.widgets.noninteractive.corner_radius = r;

    visuals.widgets.inactive.bg_fill = p.crt_widget_bg;
    visuals.widgets.inactive.weak_bg_fill = p.crt_widget_bg;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, p.crt_widget_hover);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, p.crt_text);
    visuals.widgets.inactive.corner_radius = r;

    visuals.widgets.hovered.bg_fill = p.crt_widget_hover;
    visuals.widgets.hovered.weak_bg_fill = p.crt_widget_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.2, p.accent);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.2, p.crt_text);
    visuals.widgets.hovered.corner_radius = r;

    visuals.widgets.active.bg_fill = p.accent;
    visuals.widgets.active.weak_bg_fill = p.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.2, p.accent_bright);
    visuals.widgets.active.fg_stroke = Stroke::new(1.2, p.accent_text);
    visuals.widgets.active.corner_radius = r;

    style.visuals = visuals;
    ctx.set_style_of(egui::Theme::Dark, style);
}
