use super::*;

/// A row of a settings card: label with an optional help tooltip on the left,
/// the control on the right; stacks under a narrow width. Rows after the first
/// get a divider.
pub fn settings_row(ui: &mut Ui, label: &str, help: &str, control: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    settings_item(
        ui,
        label,
        |ui| {
            let response =
                ui.add(egui::Label::new(RichText::new(label).color(p.foreground)).wrap());
            if !help.is_empty() {
                response.on_hover_text(help);
            }
        },
        control,
    );
}
/// [`settings_row`] with custom content on the left (title with badges, notes…).
pub fn settings_item(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    left: impl FnOnce(&mut Ui),
    control: impl FnOnce(&mut Ui),
) {
    let first =
        ui.data_mut(|d| std::mem::replace(d.get_temp_mut_or(settings_first(), false), false));
    if !first {
        separator(ui);
    }
    ui.add_space(SETTINGS_PAD);
    let id = ui.scope_id().with(("settings-row", id));
    // Sizes from the last frame: (control width, control height, text height).
    let (control_width, control_height, text_height): (f32, f32, f32) =
        ui.data(|d| d.get_temp(id)).unwrap_or((60., CONTROL, 18.));
    // Small controls (switches, short buttons) stay right of the label; wide ones
    // go under it when the label would get too little room.
    let narrow = ui.available_width() - control_width - theme::SPACE_L < 170.;
    let remember = |ui: &mut Ui, sizes: (f32, f32, f32)| {
        let old = (control_width, control_height, text_height);
        if (sizes.0 - old.0).abs() > 0.5
            || (sizes.1 - old.1).abs() > 0.5
            || (sizes.2 - old.2).abs() > 0.5
        {
            ui.data_mut(|d| d.insert_temp(id, sizes));
            ui.ctx().request_discard("settings row resized");
        }
    };
    // Text and control share one row height and are centered on it.
    let row = control_height.max(text_height);
    let text = |ui: &mut Ui, width: f32, height: f32, left: Box<dyn FnOnce(&mut Ui) + '_>| -> f32 {
        ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            ui.add_space(((height - text_height) / 2.).max(0.));
            ui.spacing_mut().item_spacing.y = 2.;
            ui.scope(left).response.rect.height()
        })
        .inner
    };
    let place_control = |ui: &mut Ui, height: f32| -> egui::Vec2 {
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), height),
            Layout::left_to_right(Align::Center),
            |ui| ui.scope(control).response.rect.size(),
        )
        .inner
    };
    if narrow {
        let t = text(ui, ui.available_width(), text_height, Box::new(left));
        ui.add_space(theme::SPACE_S);
        let c = place_control(ui, control_height);
        remember(ui, (c.x, c.y, t));
        ui.add_space(SETTINGS_PAD);
        return;
    }
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), row),
        Layout::left_to_right(Align::Center),
        |ui| {
            let total = ui.available_width();
            let label_width = (total - control_width - theme::SPACE_L).max(120.);
            let t = text(ui, label_width, row, Box::new(left));
            ui.add_space(
                (total - label_width - control_width - theme::HORIZONTAL_GAP * 2.).max(0.),
            );
            let c = place_control(ui, row);
            remember(ui, (c.x, c.y, t));
        },
    );
    ui.add_space(SETTINGS_PAD);
}
const SETTINGS_PAD: f32 = 7.;
/// A caption line at the top of a settings card (e.g. a plugin group).
pub fn settings_caption(ui: &mut Ui, text: &str, help: Option<&str>) {
    ui.add_space(SETTINGS_PAD);
    let r = ui.label(bold(text, theme::CAPTION).color(theme::palette(ui).muted));
    if let Some(h) = help {
        r.on_hover_text(h);
    }
}

const PERIODS: [(u32, &str); 8] = [
    (0, "Off"),
    (900, "Every 15 minutes"),
    (3600, "Every hour"),
    (10800, "Every 3 hours"),
    (21600, "Every 6 hours"),
    (43200, "Every 12 hours"),
    (86400, "Every day"),
    (604800, "Every week"),
];
/// Period picker: Off (0), presets, or a number of minutes or hours under "Custom".
/// Returns true when the user committed a new value.
pub fn interval(ui: &mut Ui, id: impl egui::AsIdSalt, value: &mut u32) -> bool {
    let key = egui::Id::unique(("kit-interval", id));
    let mut custom: bool = ui.data(|d| d.get_temp(key.with("custom"))).unwrap_or(false)
        || !PERIODS.iter().any(|(s, _)| s == value);
    let mut unit: u32 = ui
        .data(|d| d.get_temp(key.with("unit")))
        .unwrap_or(if value.is_multiple_of(3600) { 3600 } else { 60 });
    let mut amount: u32 = ui
        .data(|d| d.get_temp(key.with("amount")))
        .unwrap_or((*value / unit).max(1));
    let mut next = None;
    ui.push_id(key, |ui| {
        if custom {
            let r = ui.add_sized(
                [56., CONTROL],
                egui::DragValue::new(&mut amount)
                    .range(1..=if unit == 60 { 10_080 } else { 168 })
                    .speed(0.2),
            );
            if crate::widgets::committed(ui, &r) || r.drag_stopped() {
                next = Some(amount * unit);
            }
            let units = [(60, "minutes"), (3600, "hours")];
            let label = units
                .iter()
                .find(|(u, _)| *u == unit)
                .map_or("minutes", |(_, l)| l);
            select(ui, "unit", label, 92., |ui| {
                for (u, l) in units {
                    if ui.selectable_value(&mut unit, u, l).clicked() {
                        amount =
                            ((*value + u / 2) / u).clamp(1, if u == 60 { 10_080 } else { 168 });
                        next = Some(amount * u);
                    }
                }
            });
        }
        let label = if custom {
            "Custom".to_owned()
        } else {
            PERIODS
                .iter()
                .find(|(s, _)| s == value)
                .map_or("Custom", |(_, l)| l)
                .to_owned()
        };
        select(ui, "preset", &label, 150., |ui| {
            for (secs, text) in PERIODS {
                if ui
                    .selectable_label(!custom && *value == secs, text)
                    .clicked()
                {
                    custom = false;
                    next = Some(secs);
                }
            }
            if ui.selectable_label(custom, "Custom").clicked() {
                custom = true;
                unit = if value.is_multiple_of(3600) { 3600 } else { 60 };
                amount = (*value / unit).max(1);
            }
        });
    });
    ui.data_mut(|d| {
        d.insert_temp(key.with("custom"), custom);
        d.insert_temp(key.with("unit"), unit);
        d.insert_temp(key.with("amount"), amount);
    });
    match next {
        Some(secs) if secs != *value => {
            *value = secs;
            true
        }
        _ => false,
    }
}
