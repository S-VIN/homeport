use super::*;

/// An inline icon of `size` points.
pub fn icon(ui: &mut Ui, icon: Icon, size: f32, ink: Color32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    crate::icons::paint_sized(ui.painter(), rect.center(), icon, size, ink);
    r
}

/// How an icon button looks at rest.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Emphasis {
    /// Muted ink.
    #[default]
    Normal,
    /// Faint until its row is hovered: keeps repeated row actions calm.
    Quiet,
}

/// 26×26 ghost icon button with a tooltip and an accessible label.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, enabled: bool) -> Response {
    icon_button_ex(ui, icon, label, enabled, Emphasis::Normal, false)
}
/// Icon button for row actions; `danger` turns it red on hover only.
pub fn icon_button_ex(ui: &mut Ui, icon: Icon, label: &str, enabled: bool, emphasis: Emphasis, danger: bool) -> Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(CONTROL, CONTROL), if enabled { Sense::click() } else { Sense::hover() });
    let active = enabled && (response.hovered() || response.has_focus());
    let ink = if !enabled {
        p.faint.gamma_multiply(0.6)
    } else if active {
        if danger { p.danger } else { p.foreground }
    } else if emphasis == Emphasis::Quiet {
        quiet_ink(&p)
    } else {
        p.muted
    };
    if active {
        let fill = if danger { p.soft(p.danger) } else { p.subtle };
        ui.painter().rect_filled(rect, RADIUS_CONTROL, fill);
    }
    let size = if matches!(icon, Icon::Up | Icon::Down) { 14. } else { crate::icons::SIZE };
    crate::icons::paint_sized(ui.painter(), rect.center(), icon, size, ink);
    focus_ring(ui, &response);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if enabled {
        response.on_hover_text(label)
    } else {
        response
    }
}

/// Ink of row actions at rest: lighter than `muted` but still ≥ 4.5:1 on the card.
pub fn quiet_ink(p: &crate::palette::Palette) -> Color32 {
    p.muted.lerp_to_gamma(p.faint, 0.35)
}

pub fn focus_ring(ui: &Ui, response: &Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(response.rect, RADIUS_CONTROL, Stroke::new(2_f32, theme::palette(ui).accent), egui::StrokeKind::Inside);
    }
}
