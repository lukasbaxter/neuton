//! Visual style, matched to the site so the launcher and the page look like one
//! product.

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(0x08, 0x09, 0x0b);
pub const RAISE: Color32 = Color32::from_rgb(0x0e, 0x10, 0x13);
pub const CARD: Color32 = Color32::from_rgb(0x10, 0x13, 0x17);
pub const LINE: Color32 = Color32::from_rgb(0x1b, 0x1f, 0x26);
pub const LINE2: Color32 = Color32::from_rgb(0x2a, 0x30, 0x38);
pub const FG: Color32 = Color32::from_rgb(0xe6, 0xe9, 0xee);
pub const MID: Color32 = Color32::from_rgb(0x9a, 0xa4, 0xb2);
pub const DIM: Color32 = Color32::from_rgb(0x5f, 0x68, 0x74);
pub const ACCENT: Color32 = Color32::from_rgb(0x4d, 0xe6, 0xc4);
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0xa3, 0x4a);
pub const DANGER: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x6c);

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = RAISE;
    v.extreme_bg_color = RAISE;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(FG);
    v.window_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MID);
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, FG);
    v.widgets.hovered.bg_fill = RAISE;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, LINE2);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);
    v.widgets.active.bg_fill = LINE;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.22);
    v.selection.stroke = Stroke::new(1.0, ACCENT);

    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    ctx.set_visuals(v);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
    });
}

/// A monospace label in the dim colour, for identifiers and metadata.
pub fn mono(text: impl Into<String>, color: Color32) -> egui::RichText {
    egui::RichText::new(text).monospace().color(color)
}
