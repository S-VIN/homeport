//! The Subscriptions page, the button on an empty Servers page and the
//! refresh interval in Settings.
use crate::*;

pub(crate) fn register(p: &mut Setup) {
    p.page_fn(
        PageSpec {
            title: "Subscriptions",
            icon: Icon::Rss,
            order: 20,
        },
        page,
    );
    p.fill(&servers::EMPTY, "add", 0, |ui, cx, _| {
        let sources = cx.get(&SOURCES);
        if let Some(e) = sources.iter().find_map(|s| s.error.as_deref()) {
            let short = e.lines().next().unwrap_or(e).trim_end_matches('.');
            kit::tone_text(ui, format!("Subscription failed: {short}"), Tone::Warning);
            ui.add_space(theme::SPACE_M);
        } else if sources.is_empty() {
            kit::hint(ui, "Add a subscription to get servers.");
            ui.add_space(theme::SPACE_M);
        }
        let open = if sources.is_empty() {
            kit::Button::primary("Add subscription").icon(Icon::Add)
        } else {
            kit::Button::secondary("Open Subscriptions")
        };
        if open.show(ui).clicked() {
            cx.navigate(&PLUGIN);
        }
    });
    p.fill(&SETTINGS_SECTIONS, "refresh", 0, |ui, cx, _| {
        cx.setting(
            ui,
            "Auto update",
            &SETTINGS,
            |s| &mut s.refresh_secs,
            |ui, v| kit::interval(ui, "refresh", v),
        );
    });
}

/// Traffic and term of the plan, when the provider reports them.
pub(crate) fn plan(s: &Source) -> Option<(String, bool)> {
    let traffic = match (s.used, s.total) {
        (Some(used), Some(total)) => Some(format!("{} of {}", kit::bytes(used), kit::bytes(total))),
        // Without a limit only real use says something.
        (Some(used), None) if used > 0 => Some(format!("{} used", kit::bytes(used))),
        _ => None,
    };
    // Providers put a far-away date for "no end"; past ten years it is that.
    let left = s
        .expires
        .map(|end| i64::try_from(end).unwrap_or(i64::MAX) - now() as i64)
        .filter(|left| *left < 10 * 365 * 86_400);
    let term = left.map(|left| match (left + 43_200) / 86_400 {
        _ if left <= 0 => "expired".to_owned(),
        0 => "expires today".to_owned(),
        days => format!("expires in {}", kit::plural(days as usize, "day", "days")),
    });
    let alarming =
        left.is_some_and(|l| l <= 3 * 86_400) || s.used.zip(s.total).is_some_and(|(u, t)| u >= t);
    let text: Vec<String> = traffic.into_iter().chain(term).collect();
    (!text.is_empty()).then(|| (text.join(" · "), alarming))
}

fn status(s: &Source, loading: bool) -> (String, Tone) {
    if !s.remote() {
        return match &s.error {
            Some(e) => (e.trim_end_matches('.').to_owned(), Tone::Warning),
            None => ("Added manually".into(), Tone::Neutral),
        };
    }
    let (text, tone) = load_status(s, loading);
    match plan(s).filter(|_| !loading) {
        Some((plan, alarming)) => {
            let tone = if alarming { Tone::Warning } else { tone };
            // The plan first, so a narrow row cuts "updated …" instead;
            // a failed refresh still leads.
            let text = match s.error {
                Some(_) => format!("{text} · {plan}"),
                None => format!("{plan} · {}", text.to_lowercase()),
            };
            (text, tone)
        }
        None => (text, tone),
    }
}

fn load_status(s: &Source, loading: bool) -> (String, Tone) {
    let age = s.updated.filter(|&t| t > 0).map(kit::age);
    match (&s.error, loading) {
        (_, true) => ("Loading…".into(), Tone::Neutral),
        (Some(e), _) => (
            match age {
                Some(a) => format!(
                    "Refresh failed · {} · last loaded {a}",
                    e.trim_end_matches('.')
                ),
                None => format!("Refresh failed · {}", e.trim_end_matches('.')),
            },
            Tone::Warning,
        ),
        (None, _) if s.skipped > 0 => (
            format!(
                "{} · {} skipped: {}",
                age.map_or_else(|| "Loaded".into(), |a| format!("Updated {a}")),
                kit::plural(s.skipped as usize, "server", "servers"),
                s.skipped_why
                    .as_deref()
                    .unwrap_or("unreadable")
                    .trim_end_matches('.')
            ),
            Tone::Warning,
        ),
        (None, _) => (
            age.map_or_else(|| "Not loaded yet".into(), |a| format!("Updated {a}")),
            Tone::Neutral,
        ),
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let sources = snap.get(&SOURCES);
    let servers = snap.get(&servers::SERVERS);
    let counter = if sources.is_empty() {
        String::new()
    } else {
        format!(
            "{} · {}",
            kit::plural(sources.len(), "subscription", "subscriptions"),
            kit::plural(servers.len(), "server", "servers")
        )
    };
    cx.page_header(ui, &counter, |ui, cx| {
        cx.job_button(
            ui,
            Icon::Refresh,
            "Refresh all",
            !sources.is_empty(),
            || RefreshAll,
        );
    });

    let loading = cx.live(&LOADING);
    ListEditor::new(&SOURCES)
        .add_form(
            "Add subscription",
            || Source::new(""),
            |ui, s, width| {
                let r = kit::input(
                    ui,
                    "url",
                    &mut s.url,
                    "Subscription URL or server link",
                    width,
                );
                kit::submitted(ui, &r) && !s.url.trim().is_empty()
            },
        )
        .leading("", 8., |ui, cx, s: &Source| {
            kit::tag_dot(ui, servers::source(cx.snapshot(), s.id).hue)
                .on_hover_text("Tag color of its servers");
        })
        .column(
            Field::edit("Subscription", "Subscription URL", |s: &mut Source| {
                &mut s.url
            })
            .display(move |s: &Source| label(&sources, s.id).unwrap_or_else(|| s.label())),
        )
        .column(
            Field::custom("Servers", 56., |ui, cx, s: &Source| {
                if loading.contains(&s.id) {
                    kit::spinner(ui);
                } else {
                    let n = cx
                        .get(&servers::SERVERS)
                        .iter()
                        .filter(|x| x.source == s.id)
                        .count();
                    ui.label(n.to_string());
                }
            })
            .right(),
        )
        .column(
            Field::custom("", 30., move |ui, cx, s: &Source| {
                // A link or config is kept as pasted: nothing to refresh.
                if !s.remote() {
                    return;
                }
                let idle = !cx.live(&LOADING).contains(&s.id);
                if kit::icon_button(ui, Icon::Refresh, "Refresh", idle).clicked() {
                    cx.run(RefreshSource { id: s.id });
                }
            })
            .right(),
        )
        .subtitle(|rows, i| Some(status(&rows[i], loading.contains(&rows[i].id))))
        .empty(
            Icon::Rss,
            "No subscriptions",
            "Paste a subscription link or a vless:// link above.",
        )
        .show(ui, cx);
    cx.slot(ui, &SECTIONS, &());
}
