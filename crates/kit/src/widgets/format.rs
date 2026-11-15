use super::*;

/// Relative age of a Unix timestamp.
/// "5 min ago"; the number never wraps away from its unit.
pub fn age(time: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match now.saturating_sub(time) {
        s @ 0..=59 => {
            if s < 5 {
                "just now".into()
            } else {
                format!("{s}\u{a0}s ago")
            }
        }
        s @ 60..=3599 => format!("{}\u{a0}min ago", s / 60),
        s @ 3600..=86399 => format!("{}\u{a0}h ago", s / 3600),
        s => format!("{}\u{a0}d ago", s / 86400),
    }
}

/// Rating color: red < 40 ≤ amber < 75 ≤ green.
pub fn rating_color(ui: &Ui, rating: Option<u8>) -> Color32 {
    let p = theme::palette(ui);
    match rating {
        Some(0..=39) => p.danger,
        Some(40..=74) => p.warning,
        Some(_) => p.success,
        None => p.faint,
    }
}

/// A QR code of `data`, `size` points square, dark modules on a white quiet zone
/// (white in both themes: phone cameras read it reliably).
pub fn qr_code(ui: &mut Ui, data: &str, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        hint(ui, "Too long for a QR code");
        return response;
    };
    let n = code.width();
    let quiet = 2;
    let cell = size / (n + 2 * quiet) as f32;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, crate::theme::RADIUS, Color32::WHITE);
    let ink = Color32::from_rgb(17, 19, 23);
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            let (x, y) = ((i % n + quiet) as f32, (i / n + quiet) as f32);
            let min = rect.min + egui::vec2(x * cell, y * cell);
            painter.rect_filled(
                Rect::from_min_size(min, egui::vec2(cell + 0.25, cell + 0.25)),
                0.,
                ink,
            );
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_tooltips_only_reveal_hidden_content_or_add_information() {
        for (text, width, help, expected) in [
            ("Ready", 300., None, false),
            ("Ready", 300., Some("Ready"), false),
            ("A long value that cannot fit", 60., None, true),
            ("A long value that cannot fit", 60., Some(""), false),
            (
                "TUN",
                300.,
                Some("Routes apps through a virtual network interface"),
                true,
            ),
        ] {
            let ctx = egui::Context::default();
            let mut target = None;
            let mut widget = None;
            for frame in 0..20 {
                let events = if frame == 1 {
                    vec![egui::Event::PointerMoved(target.unwrap())]
                } else {
                    vec![]
                };
                let out = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400., 300.))),
                        time: Some(frame as f64 * 0.1),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            ui.allocate_ui_with_layout(
                                vec2(width, CONTROL),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    let response = truncated(ui, text, help);
                                    target = Some(response.rect.center());
                                    widget = Some(response.id);
                                },
                            );
                        });
                    },
                );
                out.drop_without_applying_deltas();
            }
            assert_eq!(
                egui::Tooltip::was_tooltip_open_last_frame(&ctx, widget.unwrap()),
                expected,
                "{text}: {help:?}"
            );
        }
    }

    #[test]
    fn switch_works_with_keyboard_and_ignores_disabled_input() {
        let ctx = egui::Context::default();
        let mut value = false;
        let key = || egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let mut draw = |events: Vec<egui::Event>, enabled: bool, focus: bool| {
            let mut changed = false;
            let out = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.add_enabled_ui(enabled, |ui| {
                            let response = toggle(ui, &mut value, "Auto select");
                            if focus {
                                response.request_focus();
                            }
                            changed = response.changed();
                        });
                    });
                },
            );
            out.drop_without_applying_deltas();
            (value, changed)
        };
        draw(vec![], true, true);
        assert_eq!(draw(vec![key()], true, false), (true, true));
        draw(vec![], false, false);
        assert_eq!(draw(vec![key()], false, false), (true, false));
    }

    #[test]
    fn plural_and_age() {
        assert_eq!(plural(1, "server", "servers"), "1 server");
        assert_eq!(plural(3, "server", "servers"), "3 servers");
        assert_eq!(plural(0, "rule", "rules"), "0 rules");
    }
}
