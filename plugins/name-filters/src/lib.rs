//! # name-filters
//! Exclude servers whose names match a pattern (`*` matches any text).
//! Its own page holds the patterns; the Servers header links to it. Answers `servers::EXCLUDE`.
use plugin_servers::{self as servers, Exclusion};
use rproxy_kernel::{kit, prelude::*, ui::Column, ui::ListEditor};

pub static PLUGIN: Plugin = Plugin {
    id: "name-filters",
    name: "Exclude by name",
    description: "Hide and never use servers whose names match a pattern, e.g. *test* or RU*.",
    category: categories::SERVERS,
    requires: &["servers"],
    setup,
    ..Plugin::DEFAULT
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
    let Some(rest) = text.strip_prefix(parts[0]) else {
        return false;
    };
    let Some(mut rest) = rest.strip_suffix(parts[parts.len() - 1]) else {
        return false;
    };
    for part in &parts[1..parts.len() - 1] {
        let Some((_, tail)) = rest.split_once(part) else {
            return false;
        };
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
        if others
            .iter()
            .any(|o| o.pattern.to_lowercase() == f.pattern.to_lowercase())
        {
            bail!("This pattern already exists.");
        }
        Ok(())
    });
    p.hook(&servers::EXCLUDE, |snap, s| {
        snap.get(&FILTERS)
            .iter()
            .find(|f| wildcard(&f.pattern, &s.name))
            .map(|f| Exclusion {
                reason: format!("Excluded by name: {}", f.pattern),
                group: "Excluded by name",
                permanent: true,
            })
    });
    // The Servers header keeps a shortcut with the number of patterns.
    p.fill(&servers::TOOLBAR, "filters", 50, |ui, cx, _| {
        let n = cx.get(&FILTERS).len();
        if kit::tool_button(ui, Icon::Filter, "Exclude by name", n, false).clicked() {
            cx.navigate("name-filters");
        }
    });
    p.page_fn(
        PageSpec {
            id: "name-filters",
            title: "Exclude by name",
            icon: Icon::Filter,
            order: 42,
        },
        page,
    );
    Ok(())
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let filters = snap.get(&FILTERS);
    let names: Vec<String> = snap
        .get(&servers::SERVERS)
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let hidden = names
        .iter()
        .filter(|n| filters.iter().any(|f| wildcard(&f.pattern, n)))
        .count();
    let counter = match (filters.len(), hidden) {
        (0, _) => String::new(),
        (n, h) => format!(
            "{} · {} excluded",
            kit::plural(n, "pattern", "patterns"),
            kit::plural(h, "server", "servers")
        ),
    };
    kit::page_header(ui, "Exclude by name", &counter, |_| {});
    let matches = names.clone();
    ListEditor::new(&FILTERS)
        .unordered()
        .subtitle(move |rows, i| hint(&rows[i].pattern, &names))
        .add_form(
            "Add pattern",
            || Filter {
                id: Id::random(),
                pattern: String::new(),
            },
            |ui, f, width| {
                let r = kit::input(
                    ui,
                    "pattern",
                    &mut f.pattern,
                    "Name pattern, e.g. *test*",
                    width,
                    false,
                )
                .on_hover_text("Matches the whole name; * matches any text. Case is ignored.");
                kit::submitted(ui, &r) && !f.pattern.trim().is_empty()
            },
        )
        .column(Column::edit("Pattern", "Name pattern", |f: &mut Filter| {
            &mut f.pattern
        }))
        .column(
            Column::custom("Matches", 90., move |ui, _, f: &Filter| {
                let n = matches.iter().filter(|n| wildcard(&f.pattern, n)).count();
                kit::hint(ui, kit::plural(n, "server", "servers"));
            })
            .right(),
        )
        .empty(Icon::Filter, "No exclusions", "Add a name pattern above.")
        .show(ui, cx);
}

/// A pattern without `*` matches whole names only: when it finds nothing but
/// `*pattern*` would, say so.
fn hint(pattern: &str, names: &[String]) -> Option<(String, kit::Tone)> {
    let wider = format!("*{}*", pattern.trim());
    let count = |p: &str| names.iter().filter(|n| wildcard(p, n)).count();
    (!pattern.contains('*') && count(pattern) == 0 && count(&wider) > 0).then(|| {
        (
            format!("Matches nothing: names must match whole. Did you mean {wider}?"),
            kit::Tone::Warning,
        )
    })
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
