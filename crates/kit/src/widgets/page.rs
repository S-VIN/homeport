use super::*;

/// Page header: title and a muted counter on the left, actions on the right.
/// Always one line, so every page's content starts at the same height: a
/// counter without room is cut short (the whole text on hover) or hidden.
pub fn page_header(ui: &mut Ui, title: &str, counter: &str, actions: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    let id = ui.scope_id().with(("page-header", title));
    let actions_width: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.);
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, 30.),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(width, 30.));
            ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
            let title =
                ui.add(egui::Label::new(bold(title, theme::TITLE).color(p.foreground)).truncate());
            let room = width - title.rect.width() - actions_width - 3. * theme::HORIZONTAL_GAP;
            if !counter.is_empty() && room >= 48. {
                ui.add_space(2.);
                ui.scope(|ui| {
                    ui.set_max_width(room - 2.);
                    ui.add(
                        egui::Label::new(RichText::new(counter).size(theme::SMALL).color(p.muted))
                            .truncate(),
                    );
                });
            }
            // Not `min_rect`: the row's minimum size already spans the width.
            // The cursor already stands a gap past the last widget.
            let left = ui.cursor().left();
            let free = (ui.max_rect().right() - left - actions_width).max(0.);
            ui.add_space(free);
            let w = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
                    let start = ui.cursor().left();
                    ui.data_mut(|d| d.insert_temp(in_header_id(), true));
                    actions(ui);
                    ui.data_mut(|d| d.remove::<bool>(in_header_id()));
                    ui.min_rect().right() - start
                })
                .inner;
            if (w - actions_width).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(id, w));
                ui.ctx().request_discard("page header actions resized");
            }
        },
    );
    ui.add_space(theme::SPACE_S);
}

/// Inputs and an "Add" button in one row above a list. `fields` gets the width
/// left for inputs and returns true on Enter. Returns true when the user adds.
/// While not `enabled` (an add is saving) only the button is off: the fields
/// keep focus, so a rejected entry can be corrected at once.
pub fn add_bar(
    ui: &mut Ui,
    label: &str,
    enabled: bool,
    fields: impl FnOnce(&mut Ui, f32) -> bool,
) -> bool {
    // A phone stacks the fields: a full-width button goes under them.
    let phone = phone(ui);
    let mut add = false;
    let bar = |ui: &mut Ui| {
        let button = if phone { ui.available_width() } else { 64. };
        let width = if phone {
            ui.available_width()
        } else {
            ui.available_width() - button - theme::HORIZONTAL_GAP
        };
        // The field being typed in: Enter gives up focus, the next entry wants it.
        let typing = ui.scope_id().with("add-bar-focus");
        let enter = fields(ui, width);
        if let Some(f) = ui.memory(|m| m.focused()) {
            ui.data_mut(|d| d.insert_temp(typing, f));
        }
        let response = Button::primary("Add")
            .icon(Icon::Add)
            .width(button)
            .enabled(enabled)
            .show(ui);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
        add = enabled && (enter || response.clicked());
        if enter && let Some(f) = ui.data(|d| d.get_temp::<egui::Id>(typing)) {
            ui.memory_mut(|m| m.request_focus(f));
        }
    };
    if phone {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = theme::SPACE_S;
            bar(ui)
        });
    } else {
        ui.horizontal(bar);
    }
    ui.add_space(theme::SPACE_M);
    add
}

fn card_frame(ui: &Ui, margin: f32) -> egui::Frame {
    let p = theme::palette(ui);
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1_f32, p.border))
        .corner_radius(RADIUS)
        .inner_margin(margin)
}
/// Raised surface grouping related content.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let r = card_frame(ui, INSET).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui)
    });
    ui.add_space(theme::SPACE_M);
    r
}
/// Plugin panels may have nothing to show. Empty contributions reserve no space.
pub fn optional_card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) -> bool {
    let mut frame = card_frame(ui, INSET).begin(ui);
    frame
        .content_ui
        .set_min_width(frame.content_ui.available_width());
    add(&mut frame.content_ui);
    if frame.content_ui.min_rect().height() <= 0. {
        return false;
    }
    frame.end(ui);
    ui.add_space(theme::SPACE_M);
    true
}
pub(crate) fn settings_first() -> egui::Id {
    egui::Id::unique("kit-settings-first-row")
}
/// Start a group of [`settings_row`]s outside [`settings_card`] (no divider above the first).
pub fn settings_start(ui: &Ui) {
    ui.data_mut(|d| d.insert_temp(settings_first(), true));
}
/// Card holding [`settings_row`]s (rows bring their own vertical padding).
pub fn settings_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.data_mut(|d| d.insert_temp(settings_first(), true));
    let r = card_frame(ui, 0.)
        .inner_margin(egui::Margin::symmetric(INSET as i8, 2))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(theme::SPACE_M);
    r
}
fn to_top() -> egui::Id {
    egui::Id::unique("kit-page-scroll-to-top")
}
/// The next [`page_scroll`] starts at the top (a page was just opened).
pub fn scroll_to_top(ui: &Ui) {
    ui.data_mut(|d| d.insert_temp(to_top(), true));
}
/// Vertical scroll for a page body; keeps card borders off the clip edge.
pub fn page_scroll<R>(ui: &mut Ui, id: impl egui::AsIdSalt, add: impl FnOnce(&mut Ui) -> R) -> R {
    let top = ui
        .data_mut(|d| d.remove_temp::<bool>(to_top()))
        .unwrap_or(false);
    // The bar runs in the page's right margin: content keeps the same right
    // edge with and without a scroll, and the bar never covers a card's border.
    let gutter = theme::SPACE_M;
    let mut rect = ui.available_rect_before_wrap();
    rect.max.x += gutter;
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        let mut area = egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false]);
        if top {
            area = area.vertical_scroll_offset(0.);
        }
        area.show(ui, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin {
                    left: 0,
                    right: gutter as i8,
                    top: 0,
                    bottom: 1,
                })
                .show(ui, add)
                .inner
        })
        .inner
    })
    .inner
}
/// Card for a list: rows from [`row`] fill it edge to edge.
pub fn list_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = card_frame(ui, theme::SPACE_S)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.;
            add(ui)
        })
        .inner;
    ui.add_space(theme::SPACE_M);
    r
}
/// Column captions at the top of a list card, followed by a divider.
pub fn list_header(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    // Same horizontal inset as the rows below, on both sides.
    let inset = INSET - theme::SPACE_S;
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 26.),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(ui.available_width(), 26.));
            ui.add_space(inset);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width() - inset, 26.),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_min_size(vec2(ui.available_width(), 26.));
                    add(ui);
                },
            );
        },
    );
    separator(ui);
    ui.add_space(theme::SPACE_XS);
}
/// Section title above a card. The only kind of group heading: cards never
/// carry their own captions.
pub fn section(ui: &mut Ui, title: &str, help: Option<&str>) {
    section_note(ui, title, "", help);
}
/// [`section`] with a short muted note on the right ("One at a time").
pub fn section_note(ui: &mut Ui, title: &str, note: &str, help: Option<&str>) {
    let p = theme::palette(ui);
    ui.add_space(theme::SPACE_S);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 18.),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_width(ui.available_width());
            ui.add_space(2.);
            let r = ui.label(bold(title, theme::SMALL).color(p.muted));
            if let Some(help) = help {
                r.on_hover_text(help);
            }
            if !note.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(2.);
                    ui.label(RichText::new(note).size(theme::CAPTION).color(p.muted));
                });
            }
        },
    );
    ui.add_space(theme::SPACE_XS);
}

/// What [`row`] tells its content.
#[derive(Clone, Copy)]
pub struct RowState {
    /// The pointer is over the row (show quiet actions in full).
    pub hovered: bool,
}
/// One list row: rounded hover fill, selected fill, min height [`theme::ROW`].
pub fn row<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: bool,
    add: impl FnOnce(&mut Ui, RowState) -> R,
) -> R {
    row_ex(ui, id, selected, 0., add)
}
/// [`row`] with a fading highlight (`0..=1`), e.g. for a row that was just added.
pub fn row_ex<R>(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    selected: bool,
    highlight: f32,
    add: impl FnOnce(&mut Ui, RowState) -> R,
) -> R {
    let p = theme::palette(ui);
    let id = ui.scope_id().with(("kit-row", id));
    let previous: Option<Rect> = ui.data(|d| d.get_temp(id));
    let hovered = previous.is_some_and(|r| ui.rect_contains_pointer(r))
        && !ui.ctx().is_being_dragged(egui::Id::NULL);
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric((INSET - theme::SPACE_S) as i8, 0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let outer = ui
                .ctx()
                .data_mut(|d| std::mem::replace(d.get_temp_mut_or(in_row_id(), false), true));
            let r = add(ui, RowState { hovered });
            ui.ctx().data_mut(|d| d.insert_temp(in_row_id(), outer));
            r
        });
    let rect = inner.response.rect;
    if previous != Some(rect) {
        ui.data_mut(|d| d.insert_temp(id, rect));
    }
    let base = if selected {
        Some(p.selected())
    } else if hovered {
        Some(p.hover)
    } else {
        None
    };
    let fill = if highlight > 0. {
        let flash = p.tint(p.accent, if p.dark { 0.3 } else { 0.2 });
        Some(
            base.unwrap_or(p.surface)
                .lerp_to_gamma(flash, highlight.min(1.)),
        )
    } else {
        base
    };
    if let Some(fill) = fill {
        ui.painter().set(
            background,
            egui::Shape::rect_filled(rect, RADIUS_CONTROL, fill),
        );
    }
    inner.inner
}
/// Draw `add` as full-size content even inside a list row (e.g. a row's
/// expanded details: its properties keep their labels and tags).
pub(crate) fn outside_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let outer = ui
        .ctx()
        .data_mut(|d| std::mem::replace(d.get_temp_mut_or(in_row_id(), false), false));
    let r = add(ui);
    ui.ctx().data_mut(|d| d.insert_temp(in_row_id(), outer));
    r
}
fn in_header_id() -> egui::Id {
    egui::Id::unique("kit-in-header")
}
/// Drawing the actions of a [`page_header`] on a phone: buttons show icons.
pub(crate) fn compact_header(ui: &Ui) -> bool {
    phone(ui) && ui.data(|d| d.get_temp(in_header_id()).unwrap_or(false))
}
fn in_row_id() -> egui::Id {
    egui::Id::unique("kit-in-row")
}
/// A phone-sized window (narrower than [`theme::PHONE`]).
pub fn phone(ui: &Ui) -> bool {
    ui.ctx().content_rect().width() < theme::PHONE
}
/// Inside a list [`row`] on a phone: widgets there turn compact by themselves
/// (buttons with an icon show only the icon, tags become colored dots).
pub fn compact_row(ui: &Ui) -> bool {
    phone(ui) && ui.ctx().data(|d| d.get_temp(in_row_id()).unwrap_or(false))
}
/// Cell for a row's trailing action button(s) of `width`; on phones it shrinks
/// to one icon button. Use the same call in the list header to keep columns aligned.
pub fn action_cell<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    cell_right(ui, if phone(ui) { CONTROL } else { width }, add)
}

/// The main line of a row: fixed height, vertically centered, left to right.
pub fn row_line<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(ui.available_width(), height));
            ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
            add(ui)
        },
    )
    .inner
}

/// Fixed-width cell of a row, vertically centered.
pub fn cell<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(width, CONTROL),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(width);
            add(ui)
        },
    )
    .inner
}
/// Right-aligned fixed-width cell (numbers).
pub fn cell_right<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(width, CONTROL),
        Layout::right_to_left(Align::Center),
        |ui| {
            ui.set_width(width);
            add(ui)
        },
    )
    .inner
}

/// Move up / move down buttons. `Some(true)` = up.
pub fn order(ui: &mut Ui, index: usize, len: usize, emphasis: Emphasis) -> Option<bool> {
    ui.spacing_mut().item_spacing.x = 0.;
    let button = |icon, label, enabled| {
        IconButton::new(icon, label)
            .enabled(enabled)
            .emphasis(emphasis)
    };
    let up = button(Icon::Up, "Move up", index > 0).show(ui).clicked();
    let down = button(Icon::Down, "Move down", index + 1 < len)
        .show(ui)
        .clicked();
    ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
    if up {
        Some(true)
    } else if down {
        Some(false)
    } else {
        None
    }
}

/// Delete button of a row: one click deletes, the caller offers Undo in a
/// toast ([`crate`] users: `UiCx::undo`). It turns red only under the pointer.
pub fn remove(ui: &mut Ui, emphasis: Emphasis) -> bool {
    IconButton::new(Icon::Delete, "Delete")
        .emphasis(emphasis)
        .danger()
        .show(ui)
        .clicked()
}

/// One button for all actions of a row (a phone has no room for several):
/// "Actions" opens `menu` of [`menu_item`]s.
const MENU_WIDTH: f32 = 168.;
pub fn row_menu(ui: &mut Ui, emphasis: Emphasis, menu: impl FnOnce(&mut Ui)) {
    let response = IconButton::new(Icon::More, "Actions")
        .emphasis(emphasis)
        .show(ui);
    // Hangs from the button's right edge, as wide as its longest item.
    egui::Popup::menu(&response)
        .align(egui::RectAlign::BOTTOM_END)
        .width(MENU_WIDTH)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_width(MENU_WIDTH);
            ui.spacing_mut().item_spacing.y = 0.;
            menu(ui)
        });
}

/// One line of a menu: icon and label; `danger` turns it red under the
/// pointer, like a delete button. True when clicked.
pub fn menu_item(ui: &mut Ui, icon: Icon, label: &str, enabled: bool, danger: bool) -> bool {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(
        vec2(MENU_WIDTH, 30.),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, enabled, label));
    let hovered = enabled && response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_CONTROL, p.hover);
    }
    let ink = match (enabled, hovered && danger) {
        (false, _) => p.faint,
        (true, true) => p.danger,
        (true, false) => p.foreground,
    };
    let icon_ink = if enabled && !(hovered && danger) {
        p.muted
    } else {
        ink
    };
    crate::icons::paint(
        ui.painter(),
        rect.left_center() + vec2(14., 0.),
        icon,
        icon_ink,
    );
    let text =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(theme::BODY), ink);
    ui.painter().galley(
        rect.left_center() + vec2(30., -text.size().y / 2.),
        text,
        ink,
    );
    enabled && response.clicked()
}

/// Save (✓) and Cancel (✕) of an inline edit, side by side. Returns
/// `Some(true)` on save, `Some(false)` on cancel.
pub fn edit_actions(ui: &mut Ui, can_save: bool) -> Option<bool> {
    ui.spacing_mut().item_spacing.x = 0.;
    let save = icon_button(ui, Icon::Check, "Save · Enter", can_save).clicked();
    let cancel = icon_button(ui, Icon::Close, "Cancel · Esc", true).clicked();
    ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
    if save {
        Some(true)
    } else if cancel {
        Some(false)
    } else {
        None
    }
}

/// A truncated one-line label with exactly one tooltip: `tooltip` when given,
/// otherwise the full text when it is cut off.
/// `text` shortened in the middle to fit the row ("/home/…/homeport"): for
/// paths and addresses whose ends matter. The full text is the tooltip.
pub fn middle(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let font = FontId::proportional(theme::BODY);
    let room = ui.available_width();
    let fits = |t: &str| {
        ui.painter()
            .layout_no_wrap(t.to_owned(), font.clone(), color)
            .size()
            .x
            <= room
    };
    let chars: Vec<char> = text.chars().collect();
    let mut shown = text.to_owned();
    let mut keep = chars.len();
    while !fits(&shown) && keep > 2 {
        keep -= 1;
        let head: String = chars[..keep / 2].iter().collect();
        let tail: String = chars[chars.len() - (keep - keep / 2)..].iter().collect();
        shown = format!("{head}…{tail}");
    }
    let r = ui.add(egui::Label::new(RichText::new(&shown).color(color)));
    if shown != text {
        r.on_hover_text(text)
    } else {
        r
    }
}
pub fn truncated(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    tooltip: Option<&str>,
) -> Response {
    let text = text.into();
    let full = text.text().to_owned();
    let (r, elided) = text_label_with_elision(ui, text, false, false);
    match tooltip {
        Some(t) if !t.is_empty() && (elided || t != full) => r.on_hover_ui(|ui| {
            text_label(ui, t, true, false);
        }),
        None if elided => r.on_hover_ui(|ui| {
            text_label(ui, full, true, false);
        }),
        _ => r,
    }
}

/// An action that is a small ghost button with an icon in a row of actions,
/// or an item of a context menu (`menu`). Returns true when clicked.
pub fn action_item(ui: &mut Ui, icon: Icon, label: &str, enabled: bool, menu: bool) -> bool {
    if menu {
        let p = theme::palette(ui);
        let width = ui.available_width().max(190.);
        let (rect, r) = ui.allocate_exact_size(
            vec2(width, CONTROL),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let hot = enabled && (r.hovered() || r.has_focus());
        if hot {
            ui.painter().rect_filled(rect, RADIUS_CONTROL, p.subtle);
        }
        let ink = if !enabled {
            p.faint
        } else if hot {
            p.foreground
        } else {
            p.muted
        };
        crate::icons::paint_sized(
            ui.painter(),
            rect.left_center() + vec2(14., 0.),
            icon,
            15.,
            ink,
        );
        let g = ui.painter().layout_no_wrap(
            label.to_owned(),
            FontId::proportional(theme::BODY),
            if enabled { p.foreground } else { p.faint },
        );
        ui.painter().galley(
            rect.left_center() + vec2(30., -g.size().y / 2.),
            g,
            p.foreground,
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, enabled, label));
        let clicked = r.clicked();
        if clicked {
            ui.close();
        }
        clicked
    } else {
        Button::secondary(label)
            .icon(icon)
            .enabled(enabled)
            .show(ui)
            .clicked()
    }
}

/// Compact empty state; callers can add the relevant action below.
pub fn empty(ui: &mut Ui, icon: Icon, title: &str) {
    empty_with(ui, icon, title, "");
}
/// [`empty`] with the next step under the title ("Paste a link above.").
pub fn empty_with(ui: &mut Ui, icon: Icon, title: &str, next: &str) {
    let p = theme::palette(ui);
    ui.add_space(theme::SPACE_XL);
    ui.vertical_centered(|ui| {
        crate::icons::paint_sized(
            ui.painter(),
            ui.cursor().center_top() + vec2(0., 13.),
            icon,
            26.,
            p.faint,
        );
        ui.add_space(30.);
        ui.label(bold(title, theme::BODY).color(p.foreground));
        if !next.is_empty() {
            ui.add_space(theme::SPACE_XS);
            ui.add(egui::Label::new(RichText::new(next).size(theme::SMALL).color(p.muted)).wrap());
        }
    });
    ui.add_space(theme::SPACE_XL);
}

/// Tinted strip with an icon, text and an optional action. Returns true when
/// the action is clicked.
pub fn banner(ui: &mut Ui, tone: Tone, text: &str, action_label: Option<&str>) -> bool {
    let p = theme::palette(ui);
    let ink = p.tone(tone);
    let mut clicked = false;
    egui::Frame::NONE
        .fill(p.soft(ink))
        .stroke(Stroke::new(1_f32, p.tint(ink, 0.3)))
        .corner_radius(RADIUS)
        .inner_margin(egui::Margin::symmetric(INSET as i8, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let icon_kind = match tone {
                    Tone::Danger => Icon::Error,
                    Tone::Warning => Icon::Warning,
                    Tone::Success => Icon::Connected,
                    _ => Icon::Info,
                };
                icon(ui, icon_kind, 16., ink);
                let action_w = if action_label.is_some() { 110. } else { 0. };
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width() - action_w, 0.),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.add(egui::Label::new(RichText::new(text).color(p.foreground)).wrap());
                    },
                );
                if let Some(a) = action_label {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        clicked = Button::secondary(a).show(ui).clicked();
                    });
                }
            });
        });
    ui.add_space(theme::SPACE_M);
    clicked
}
