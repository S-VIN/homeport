//! Column captions that sort and filter a table: click a caption to sort by
//! it (again to reverse), open the funnel after it to filter.
use super::*;

/// Order of a sorted column. `Asc` is the column's natural order (A→Z, best first).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}
impl SortDir {
    pub fn flip(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

/// The sort of a table: which column, which way. A click on the sorted
/// column reverses it; on another column starts that one in its natural order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sort<K> {
    pub column: K,
    pub dir: SortDir,
}
impl<K: PartialEq + Copy> Sort<K> {
    pub fn new(column: K) -> Self {
        Self {
            column,
            dir: SortDir::Asc,
        }
    }
    /// The direction when `column` is the sorted one.
    pub fn of(&self, column: K) -> Option<SortDir> {
        (self.column == column).then_some(self.dir)
    }
    /// After a click on `column`'s caption.
    pub fn click(&mut self, column: K) {
        if self.column == column {
            self.dir = self.dir.flip();
        } else {
            *self = Self::new(column);
        }
    }
}

const ARROW: f32 = 10.;
const ARROW_GAP: f32 = 3.;

/// A column caption that sorts: muted text, an arrow while it is the sort
/// (a faint one under the pointer). `right` keeps the text on the right edge
/// (numbers), with the arrow before it. True when clicked.
pub fn sort_caption(ui: &mut Ui, text: &str, sorted: Option<SortDir>, right: bool) -> bool {
    let p = theme::palette(ui);
    let font = FontId::proportional(theme::CAPTION);
    let ink = if sorted.is_some() {
        p.foreground
    } else {
        p.muted
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ink);
    let size = vec2(galley.size().x + ARROW_GAP + ARROW, 18.);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let hovered = response.hovered();
    if ui.is_rect_visible(rect) {
        let (text_x, arrow_x) = if right {
            (rect.right() - galley.size().x, rect.left() + ARROW / 2.)
        } else {
            (rect.left(), rect.right() - ARROW / 2.)
        };
        let y = rect.center().y - galley.size().y / 2.;
        ui.painter().galley(pos2(text_x, y), galley, ink);
        let arrow = match sorted {
            Some(SortDir::Asc) => Some((Icon::Down, p.foreground)),
            Some(SortDir::Desc) => Some((Icon::Up, p.foreground)),
            None if hovered => Some((Icon::Down, p.faint)),
            None => None,
        };
        if let Some((icon, color)) = arrow {
            crate::icons::paint_sized(
                ui.painter(),
                pos2(arrow_x, rect.center().y),
                icon,
                ARROW,
                color,
            );
        }
    }
    let label = match sorted {
        Some(SortDir::Asc) => format!("{text}, sorted"),
        Some(SortDir::Desc) => format!("{text}, sorted in reverse"),
        None => format!("Sort by {text}"),
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    response.clicked()
}

/// A funnel after a caption that opens `menu` with the filters of the column.
/// `active`: how many filters are in use (the funnel turns accent and shows
/// the number).
pub fn filter_button(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    label: &str,
    active: usize,
    menu: impl FnOnce(&mut Ui),
) {
    let p = theme::palette(ui);
    let badge = (active > 0).then(|| active.to_string());
    let badge_galley = badge.map(|b| {
        ui.painter()
            .layout_no_wrap(b, FontId::proportional(theme::CAPTION), p.accent)
    });
    let width = 18. + badge_galley.as_ref().map_or(0., |g| g.size().x + 2.);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let popup = ui.make_persistent_id(("kit-filter", id));
    let open = egui::Popup::is_id_open(ui.ctx(), popup);
    if ui.is_rect_visible(rect) {
        if response.hovered() || open {
            ui.painter().rect_filled(rect, 4., p.hover);
        }
        let ink = if active > 0 {
            p.accent
        } else if response.hovered() || open {
            p.foreground
        } else {
            p.faint
        };
        crate::icons::paint_sized(
            ui.painter(),
            pos2(rect.left() + 9., rect.center().y),
            Icon::Filter,
            12.,
            ink,
        );
        if let Some(g) = badge_galley {
            let y = rect.center().y - g.size().y / 2.;
            ui.painter().galley(pos2(rect.left() + 18., y), g, p.accent);
        }
    }
    let name = if active > 0 {
        format!("{label}: {active} active")
    } else {
        label.to_owned()
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    let response = if open {
        response
    } else {
        response.on_hover_text(label)
    };
    egui::Popup::from_toggle_button_response(&response)
        .id(popup)
        .align(egui::RectAlign::BOTTOM_START)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(200.);
            ui.spacing_mut().item_spacing.y = 2.;
            menu(ui)
        });
}

/// A group title inside a filter menu ("Subscription", "Protocol").
pub fn filter_group(ui: &mut Ui, title: &str) {
    ui.add_space(theme::SPACE_S);
    caption(ui, title);
}

/// One choice in a filter menu; true when toggled.
pub fn filter_option(ui: &mut Ui, checked: &mut bool, label: &str) -> bool {
    ui.checkbox(checked, label).changed()
}
