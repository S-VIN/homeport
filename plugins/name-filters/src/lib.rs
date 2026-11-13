//! # name-filters
//! Exclude servers whose names match a pattern (`*` matches any text).
//! Adds a Filters button to the Servers toolbar and answers `servers::EXCLUDE`.
use plugin_servers::{self as servers, Exclusion};
use rproxy_kernel::{kit, prelude::*, ui::Column, ui::ListEditor};

pub static PLUGIN: Plugin = Plugin {
    id: "name-filters",
    name: "Name filters",
    description: "Hide and never use servers whose names match a pattern, e.g. *test* or RU*.",
    category: categories::SERVERS,
    requires: &["servers"],
    required: false,
    default_enabled: true,
    platforms: Platform::ALL,
    setup,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "name_filters")]
pub struct Filter {
    pub id: Id,
    pub pattern: String,
}
pub static FILTERS: State<Vec<Filter>> = State::new("name-filters.list");

/// Case-insensitive; `*` matches any sequence (including empty), everything else is literal.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<_> = pattern.split('*').collect();
    if parts.len() == 1 {
        return text == pattern;
    }
    let Some(rest) = text.strip_prefix(parts[0]) else { return false };
    let Some(mut rest) = rest.strip_suffix(parts[parts.len() - 1]) else { return false };
    for part in &parts[1..parts.len() - 1] {
        let Some((_, tail)) = rest.split_once(part) else { return false };
        rest = tail;
    }
    true
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&FILTERS).validate(|_, f, _, others| {
        f.pattern = f.pattern.trim().to_owned();
        if f.pattern.is_empty() || f.pattern.len() > 512 {
            bail!("Enter a name pattern of 1–512 bytes.");
        }
        if others.iter().any(|o| o.pattern.to_lowercase() == f.pattern.to_lowercase()) {
            bail!("Name filter already exists.");
        }
        Ok(())
    });
    p.hook(&servers::EXCLUDE, |snap, s| {
        snap.get(&FILTERS).iter().find(|f| wildcard(&f.pattern, &s.name)).map(|f| Exclusion {
            reason: format!("Name filtered by {}", f.pattern),
            disconnect: true,
        })
    });
    let open = || egui::Id::unique("name-filters-open");
    p.fill(&servers::TOOLBAR, "filters", 50, move |ui, cx, _| {
        let n = cx.get(&FILTERS).len();
        let mut is_open = ui.data(|d| d.get_temp::<bool>(open()).unwrap_or(false));
        // The page header gives the toolbar its room; drop the label in a narrow window.
        let label = if ui.ctx().content_rect().width() >= 560. { "Name filters" } else { "" };
        let r = kit::Button::secondary(label).icon(Icon::Filter).count((n > 0).then_some(n)).selected(is_open).show(ui);
        if r.on_hover_text("Name filters: never use servers whose names match").clicked() {
            is_open = !is_open;
            ui.data_mut(|d| d.insert_temp(open(), is_open));
        }
    });
    p.fill(&servers::TOOLS, "filters", 50, move |ui, cx, _| {
        if !ui.data(|d| d.get_temp::<bool>(open()).unwrap_or(false)) {
            return;
        }
        let snap = cx.snapshot().clone();
        let hidden = snap.get(&servers::SERVERS).iter().filter(|s| snap.get(&FILTERS).iter().any(|f| wildcard(&f.pattern, &s.name))).count();
        let pal = kit::colors(ui);
        kit::row_line(ui, 24., |ui| {
            ui.label(kit::bold("Name filters", kit::theme::BODY).color(pal.foreground));
            if hidden > 0 {
                kit::hint(ui, format!("{} excluded", kit::plural(hidden, "server", "servers")));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::icon_button(ui, Icon::Close, "Close name filters", true).clicked() {
                    ui.data_mut(|d| d.insert_temp(open(), false));
                }
            });
        });
        kit::helper(ui, "Servers whose whole name matches are never used. * matches any text.");
        ListEditor::new(&FILTERS)
            .unordered()
            .no_headers()
            .add_form("Add name filter", || Filter { id: Id::random(), pattern: String::new() }, |ui, f, width| {
                let r = kit::input(ui, "pattern", &mut f.pattern, "Name pattern, e.g. *test*", width, false);
                kit::submitted(ui, &r) && !f.pattern.trim().is_empty()
            })
            .column(Column::edit("Pattern", "Name pattern", |f: &mut Filter| &mut f.pattern))
            .column(Column::custom("Matches", 90., |ui, cx, f: &Filter| {
                let n = cx.get(&servers::SERVERS).iter().filter(|s| wildcard(&f.pattern, &s.name)).count();
                kit::hint(ui, kit::plural(n, "server", "servers"));
            }).right())
            .empty(Icon::Filter, "No name filters", "Add a pattern above to exclude servers by name.")
            .show(ui, cx);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wildcard;
    #[test]
    fn wildcard_matches_whole_names() {
        assert!(wildcard("us*east", "US · East"));
        assert!(!wildcard("us*east", "old US · East"));
        assert!(wildcard("*", ""));
        assert!(wildcard("a*b*c", "aXbYc"));
        assert!(!wildcard("abc", "abcd"));
    }
}
