//! Design tokens, fonts and the egui style. Every size in the kit comes from here:
//! plugins never hard-code spacing, heights or colors.
pub use crate::palette;

use egui::{
    Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, Style, TextStyle,
    Ui, Vec2, Visuals, epaint::Shadow,
};
use std::sync::Arc;

// ---- spacing scale (points) -------------------------------------------------
pub const SPACE_XS: f32 = 2.0;
pub const SPACE_S: f32 = 4.0;
pub const SPACE_M: f32 = 8.0;
pub const SPACE_L: f32 = 12.0;
pub const SPACE_XL: f32 = 16.0;

/// Height of every interactive control.
pub const CONTROL: f32 = 26.0;
/// Height of one list row.
pub const ROW: f32 = 34.0;
/// Vertical gap between stacked items.
pub const GAP: f32 = SPACE_S;
/// Horizontal gap between items in a row.
pub const HORIZONTAL_GAP: f32 = 6.0;
/// Page padding.
pub const PADDING: f32 = 10.0;
/// Space between blocks of a page (header, filters, cards).
pub const SECTION_GAP: f32 = SPACE_L;
/// Inner horizontal padding of list rows and cards.
pub const INSET: f32 = 10.0;
/// Status bar height (top of the window).
pub const STATUS_BAR: f32 = 34.0;
/// Bottom navigation height.
pub const NAVIGATION: f32 = 48.0;
/// Width of the side panel column in a wide window.
pub const UTILITY_WIDTH: f32 = 300.0;
/// From this window width the side panel is docked on the right.
pub const UTILITY_BREAKPOINT: f32 = 960.0;
/// Phone-sized window: list rows trade labels for icons (see `kit::compact_row`).
pub const PHONE: f32 = 480.0;

/// Fingers need about 40 px targets. On phones a tap that lands on nothing
/// clickable goes to the closest control within this radius, so a 26 px
/// button takes taps 40 px wide; a direct hit always wins.
pub fn fit_pointer(ctx: &egui::Context) {
    let radius = if ctx.content_rect().width() < PHONE {
        8.
    } else {
        5.
    };
    if ctx.global_style().interaction.interact_radius != radius {
        ctx.all_styles_mut(|s| s.interaction.interact_radius = radius);
    }
}
/// Width of the label column in property lists (server details, About).
pub const LABEL_WIDTH: f32 = 96.0;
/// Corner radius of cards, popovers and toasts.
pub const RADIUS: u8 = 8;
/// Corner radius of controls (buttons, inputs, selects).
pub const RADIUS_CONTROL: u8 = 6;

// ---- type scale ---------------------------------------------------------------
/// Body text, inputs and buttons.
pub const BODY: f32 = 13.0;
/// Secondary lines, counters, badges.
pub const SMALL: f32 = 12.0;
/// Column captions and fine print.
pub const CAPTION: f32 = 11.0;
/// Page and card titles.
pub const TITLE: f32 = 15.0;
/// Bold family name.
pub const BOLD: &str = "Ubuntu Bold";

/// Call once during app initialization, not on every frame.
pub fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    for (name, data) in [
        (
            "Ubuntu",
            include_bytes!("../../../assets/fonts/Ubuntu-R.ttf").as_slice(),
        ),
        (
            BOLD,
            include_bytes!("../../../assets/fonts/Ubuntu-B.ttf").as_slice(),
        ),
        (
            "Ubuntu Mono",
            include_bytes!("../../../assets/fonts/UbuntuMono-R.ttf").as_slice(),
        ),
        (
            "Symbols",
            include_bytes!("../../../assets/fonts/SourceCodePro-Regular.otf").as_slice(),
        ),
        (
            crate::icons::REGULAR,
            include_bytes!("../../../assets/fonts/Phosphor.ttf").as_slice(),
        ),
        (
            crate::icons::FILL,
            include_bytes!("../../../assets/fonts/Phosphor-Fill.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(data)));
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "Ubuntu".into());
    proportional.insert(1, "Symbols".into());
    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    monospace.insert(0, "Ubuntu Mono".into());
    monospace.insert(1, "Symbols".into());
    let mut bold = fonts.families[&FontFamily::Proportional].clone();
    bold.insert(0, BOLD.into());
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold);
    // Icon fonts are separate families: their ASCII glyphs are ligature blanks.
    for name in [crate::icons::REGULAR, crate::icons::FILL] {
        fonts
            .families
            .insert(FontFamily::Name(name.into()), vec![name.into()]);
    }
    ctx.set_fonts(fonts);
}

/// Apply on initialization or theme change. Keep application state outside this function.
pub fn style(dark: bool, reduced_motion: bool) -> Style {
    let p = if dark { palette::DARK } else { palette::LIGHT };
    let mut s = Style {
        visuals: if dark {
            Visuals::dark()
        } else {
            Visuals::light()
        },
        ..Style::default()
    };
    s.text_styles
        .insert(TextStyle::Body, FontId::proportional(BODY));
    s.text_styles
        .insert(TextStyle::Button, FontId::proportional(BODY));
    s.text_styles
        .insert(TextStyle::Small, FontId::proportional(SMALL));
    s.text_styles.insert(
        TextStyle::Heading,
        FontId::new(TITLE, FontFamily::Name(BOLD.into())),
    );
    s.text_styles
        .insert(TextStyle::Monospace, FontId::monospace(BODY));
    s.spacing.item_spacing = Vec2::new(HORIZONTAL_GAP, GAP);
    s.spacing.button_padding = Vec2::new(10.0, 4.0);
    s.spacing.interact_size = Vec2::new(CONTROL, CONTROL);
    s.spacing.combo_height = 300.0;
    s.spacing.tooltip_width = 320.0;
    s.spacing.menu_margin = egui::Margin::same(4);
    s.spacing.window_margin = egui::Margin::same(PADDING as i8);
    s.spacing.icon_width = 14.0;
    s.spacing.icon_width_inner = 8.0;
    // Floating bar without a track: a faint handle at rest, full on hover.
    s.spacing.scroll = egui::style::ScrollStyle {
        floating_width: 4.0,
        bar_width: 8.0,
        bar_inner_margin: 2.0,
        bar_outer_margin: 0.0,
        floating_allocated_width: 0.0,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.0,
        interact_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 0.7,
        interact_handle_opacity: 0.9,
        ..egui::style::ScrollStyle::thin()
    };
    s.animation_time = if reduced_motion { 0.0 } else { 0.12 };
    s.interaction.selectable_labels = false;
    let v = &mut s.visuals;
    v.override_text_color = None;
    v.panel_fill = p.background;
    v.window_fill = p.surface;
    v.faint_bg_color = p.hover;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.code_bg_color = p.subtle;
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.danger;
    v.warn_fg_color = p.warning;
    v.weak_text_color = Some(p.muted);
    v.selection.bg_fill = p.tint(p.accent, if dark { 0.35 } else { 0.22 });
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.window_corner_radius = CornerRadius::same(RADIUS);
    v.menu_corner_radius = CornerRadius::same(RADIUS);
    let shadow = Shadow {
        offset: [0, 6],
        blur: 20,
        spread: 0,
        color: p.shadow,
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.window_stroke = Stroke::new(1_f32, p.border);
    v.striped = false;
    v.indent_has_left_vline = false;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = p.surface;
        w.weak_bg_fill = p.surface;
        w.fg_stroke = Stroke::new(1.0_f32, p.foreground);
        w.bg_stroke = Stroke::new(1.0_f32, p.control_border);
        w.corner_radius = CornerRadius::same(RADIUS_CONTROL);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1_f32, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1_f32, p.foreground);
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1_f32, p.faint);
    v.widgets.active.weak_bg_fill = p.subtle;
    v.widgets.active.bg_fill = p.subtle;
    v.widgets.active.bg_stroke = Stroke::new(1_f32, p.accent);
    v.widgets.open.weak_bg_fill = p.surface;
    v.widgets.open.bg_stroke = Stroke::new(1_f32, p.accent);
    s
}

pub fn palette(ui: &Ui) -> palette::Palette {
    if ui.visuals().dark_mode {
        palette::DARK
    } else {
        palette::LIGHT
    }
}

/// Bold font at `size`.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}
