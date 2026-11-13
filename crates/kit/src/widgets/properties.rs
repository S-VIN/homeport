use super::*;

/// One "label  value" line with the shared label column ([`theme::LABEL_WIDTH`]).
/// Long values are truncated; hovering the line offers a copy button.
pub fn property(ui: &mut Ui, label: &str, value: &str) {
    property_with(ui, label, |ui| {
        let p = theme::palette(ui);
        let long = value.chars().count() > 24;
        let id = ui.scope_id().with(("property", label));
        let hovered = ui.data(|d| d.get_temp::<Rect>(id)).is_some_and(|r| ui.rect_contains_pointer(r));
        let width = ui.available_width() - if long { 26. } else { 0. };
        let r = ui.allocate_ui_with_layout(vec2(width, 20.), Layout::left_to_right(Align::Center), |ui| {
            truncated(ui, RichText::new(value).color(p.foreground), None);
        });
        ui.data_mut(|d| d.insert_temp(id, r.response.rect.expand2(vec2(30., 2.))));
        if long && (hovered || ui.ctx().memory(|m| m.has_focus(id))) {
            let c = icon_button_ex(ui, Icon::Copy, "Copy", true, Emphasis::Normal, false);
            if c.clicked() {
                ui.ctx().copy_text(value.to_owned());
            }
        }
    });
}
/// A property line with a custom value (a tag, colored text…).
pub fn property_with(ui: &mut Ui, label: &str, value: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_height(22.);
        ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
        cell(ui, theme::LABEL_WIDTH, |ui| {
            ui.add(egui::Label::new(RichText::new(label).size(theme::SMALL).color(p.muted)).truncate());
        });
        value(ui);
    });
}
/// Label → value lines.
pub fn properties(ui: &mut Ui, id: impl egui::AsIdSalt, rows: &[(String, String)]) {
    ui.push_id(id, |ui| {
        ui.spacing_mut().item_spacing.y = 0.;
        for (k, v) in rows {
            property(ui, k, v);
        }
    });
}
/// A "More details" disclosure; `add` draws the hidden content.
pub fn disclosure(ui: &mut Ui, id: impl egui::AsIdSalt, label: &str, add: impl FnOnce(&mut Ui)) {
    let id = egui::Id::unique(("kit-disclosure", id));
    let mut open: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
    let p = theme::palette(ui);
    let r = ui.horizontal(|ui| {
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width().min(160.), 22.), Sense::click());
        let ink = if r.hovered() { p.foreground } else { p.muted };
        crate::icons::paint_sized(ui.painter(), pos2(rect.left() + 6., rect.center().y), if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12., ink);
        let g = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(theme::SMALL), ink);
        ui.painter().galley(pos2(rect.left() + 16., rect.center().y - g.size().y / 2.), g, ink);
        focus_ring(ui, &r);
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, label));
        r
    });
    if r.inner.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    if open {
        add(ui);
    }
}
/// Labelled metrics side by side: caption above a value.
pub fn stats(ui: &mut Ui, items: &[(&str, String, Option<Color32>)]) {
    let p = theme::palette(ui);
    if items.is_empty() {
        return;
    }
    let gap = theme::SPACE_S;
    let width = ((ui.available_width() - gap * (items.len() as f32 - 1.)) / items.len() as f32).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (label, value, color) in items {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 40.), Sense::hover());
            ui.painter().rect_filled(rect, RADIUS_CONTROL, if p.dark { p.subtle } else { p.background });
            let inner = rect.shrink2(vec2(8., 5.));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
            child.spacing_mut().item_spacing.y = 1.;
            child.add(egui::Label::new(RichText::new(*label).size(theme::CAPTION).color(p.muted)).truncate());
            child.add(egui::Label::new(bold(value.clone(), theme::BODY).color(color.unwrap_or(p.foreground))).truncate());
        }
    });
}
