//! # name-filters
//! Exclude servers whose names match a pattern (`*` matches any text).
//! Adds a Filters button to the Servers toolbar and answers `servers::EXCLUDE`.
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
                disconnect: true,
            })
    });
    let tool = Tool {
        id: "filters",
        label: "Exclude by name",
        icon: Icon::Filter,
        order: 50,
    };
    p.tool(
        &servers::TOOLBAR,
        &servers::TOOLS,
        tool,
        |snap| snap.get(&FILTERS).len(),
        |ui, cx| {
            let snap = cx.snapshot().clone();
            let hidden = snap
                .get(&servers::SERVERS)
                .iter()
                .filter(|s| {
                    snap.get(&FILTERS)
                        .iter()
                        .any(|f| wildcard(&f.pattern, &s.name))
                })
                .count();
            if hidden > 0 {
                kit::hint(
                    ui,
                    format!("{} excluded", kit::plural(hidden, "server", "servers")),
                );
            }
            let names: Vec<String> = cx
                .get(&servers::SERVERS)
                .iter()
                .map(|s| s.name.clone())
                .collect();
            ListEditor::new(&FILTERS)
                .unordered()
                .no_headers()
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
                        .on_hover_text(
                            "Excludes matching names. Matches the whole name; * matches any text.",
                        );
                        kit::submitted(ui, &r) && !f.pattern.trim().is_empty()
                    },
                )
                .column(Column::edit("Pattern", "Name pattern", |f: &mut Filter| {
                    &mut f.pattern
                }))
                .column(
                    Column::custom("Matches", 90., |ui, cx, f: &Filter| {
                        let n = cx
                            .get(&servers::SERVERS)
                            .iter()
                            .filter(|s| wildcard(&f.pattern, &s.name))
                            .count();
                        kit::hint(ui, kit::plural(n, "server", "servers"));
                    })
                    .right(),
                )
                .empty(
                    Icon::Filter,
                    "No exclusions",
                    "Hide servers whose names match, like exclude-filter in Clash: e.g. *test*.",
                )
                .show(ui, cx);
        },
    );
    Ok(())
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
