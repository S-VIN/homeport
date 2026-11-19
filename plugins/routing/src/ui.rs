//! The Routing page: the rule list with its add bar, and where everything
//! else goes.
use crate::*;
use homeport_kernel::{
    core::Labels,
    ui::{Field, ListEditor},
};

/// What a pattern matches, for the small type badge.
fn kind(pattern: &str) -> &'static str {
    match rule_pattern(pattern) {
        Ok((p, _)) if p == "*" => "Everything",
        Ok((p, _)) if p.starts_with("app:") => "App",
        Ok((p, _)) if p.starts_with("inbound:") => "Inbound",
        Ok((p, _)) if p.starts_with("geosite:") => "GeoSite",
        Ok((p, _)) if p.starts_with("geoip:") => "GeoIP",
        Ok((_, Some(net))) if net.prefix_len() == net.max_prefix_len() => "IP",
        Ok((_, Some(_))) => "Network",
        _ => "Domain",
    }
}

choices!(
    /// The type picked in the add bar; it only decides the pattern's prefix.
    enum Kind {
        Domain => "Domain",
        Ip => "IP",
        GeoIp => "GeoIP",
        GeoSite => "GeoSite",
        App => "App",
        Inbound => "Inbound",
    }
);
impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::GeoIp => "geoip:",
            Kind::GeoSite => "geosite:",
            Kind::App => "app:",
            Kind::Inbound => "inbound:",
            _ => "",
        }
    }
    fn of(pattern: &str) -> Option<Kind> {
        let p = pattern.trim();
        Kind::ALL
            .iter()
            .copied()
            .find(|k| !k.prefix().is_empty() && p.starts_with(k.prefix()))
    }
}

/// GeoIP lists offered in the add bar; any other code still works typed as `geoip:xx`.
const COUNTRIES: &[(&str, &str)] = &[
    ("private", "Private networks (LAN)"),
    ("ru", "🇷🇺 Russia"),
    ("by", "🇧🇾 Belarus"),
    ("ua", "🇺🇦 Ukraine"),
    ("kz", "🇰🇿 Kazakhstan"),
    ("cn", "🇨🇳 China"),
    ("ir", "🇮🇷 Iran"),
    ("tr", "🇹🇷 Turkey"),
    ("us", "🇺🇸 United States"),
    ("gb", "🇬🇧 United Kingdom"),
    ("de", "🇩🇪 Germany"),
    ("nl", "🇳🇱 Netherlands"),
    ("fr", "🇫🇷 France"),
    ("fi", "🇫🇮 Finland"),
    ("se", "🇸🇪 Sweden"),
    ("pl", "🇵🇱 Poland"),
    ("jp", "🇯🇵 Japan"),
    ("kr", "🇰🇷 South Korea"),
    ("sg", "🇸🇬 Singapore"),
    ("hk", "🇭🇰 Hong Kong"),
    ("in", "🇮🇳 India"),
    ("ae", "🇦🇪 United Arab Emirates"),
    ("ca", "🇨🇦 Canada"),
    ("br", "🇧🇷 Brazil"),
];

/// The add bar: a type, then what it needs. The draft keeps the full pattern.
fn rule_fields(ui: &mut egui::Ui, inbounds: &[(String, String)], r: &mut Rule, width: f32) -> bool {
    let sandboxed = sandboxed();
    let kinds: Vec<Kind> = Kind::ALL
        .iter()
        .copied()
        .filter(|k| match k {
            Kind::App => !sandboxed,
            Kind::Inbound => !inbounds.is_empty(),
            _ => true,
        })
        .collect();
    let key = ui.scope_id().with("rule-kind");
    let mut kind: Kind = ui
        .data(|d| d.get_temp(key))
        .or_else(|| Kind::of(&r.pattern))
        .unwrap_or(Kind::Domain);
    let pattern = r.pattern.trim();
    let mut value = match Kind::of(pattern) {
        Some(k) if k == kind => pattern[k.prefix().len()..].to_owned(),
        _ => pattern.to_owned(),
    };
    let mut picked = kind;
    let fields = kit::Fields::new(width).choice("kind", &mut picked, &kinds);
    let fields = match kind {
        Kind::Domain => fields
            .input("pattern", &mut value, "example.org or *tube*")
            .help("example.org also covers its subdomains; =example.org only that name; * matches any text. A pasted link works too."),
        Kind::Ip => fields.input("pattern", &mut value, "10.0.0.0/8, 1.2.3.4 or 192.168.*"),
        Kind::GeoIp => {
            let options = COUNTRIES.iter().map(|(v, l)| (v.to_string(), l.to_string()));
            fields.options("country", &mut value, options.collect(), "Country")
        }
        Kind::GeoSite => fields
            .input("pattern", &mut value, "category-ru, youtube, google…")
            .help("A list name from the MetaCubeX geo database (meta-rules-dat)."),
        Kind::App => fields
            .input("pattern", &mut value, "Telegram or chrome.exe")
            .help("The program's process name, for example firefox or Telegram.exe."),
        Kind::Inbound => {
            fields.options("inbound", &mut value, inbounds.to_vec(), "Inbound")
        }
    };
    let enter = fields.choice("route", &mut r.action, Route::ALL).show(ui);
    if picked != kind {
        ui.data_mut(|d| d.insert_temp(key, picked));
        // A country or an inbound means nothing as typed text, and back.
        let closed = |k| matches!(k, Kind::GeoIp | Kind::Inbound);
        if closed(picked) || closed(kind) {
            value.clear();
        }
        kind = picked;
    }
    r.pattern = match kind {
        // Still typed: the validation names what this kind needs.
        k if value.trim().is_empty() => k.prefix().to_owned(),
        k => format!("{}{}", k.prefix(), value.trim()),
    };
    enter && !r.pattern.is_empty()
}

/// How a rule's pattern reads in the list: inbounds by their label.
/// A pattern as the list shows it: the type badge already says the prefix
/// (`geoip:ru` → "🇷🇺 Russia", `inbound:vless` → "VLESS clients").
fn pattern_label(labels: &Labels, pattern: &str) -> Option<String> {
    if let Some(name) = pattern.strip_prefix("inbound:") {
        return Some(labels.get(name).unwrap_or(name).to_owned());
    }
    if let Some(code) = pattern.strip_prefix("geoip:") {
        let country = COUNTRIES.iter().find(|(c, _)| *c == code);
        return Some(country.map_or(code, |(_, name)| *name).to_owned());
    }
    ["geosite:", "app:"]
        .iter()
        .find_map(|p| pattern.strip_prefix(p))
        .map(str::to_owned)
}

pub(crate) fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let geo_status = (*cx.live(&GEO)).clone();
    let n = cx.get(&RULES).len();
    let counter = kit::count(n, "rule", "rules");
    let has_geo = !geo_providers(cx.snapshot()).is_empty();
    cx.page_header(ui, &counter, |ui, cx| {
        if has_geo {
            cx.job_button(
                ui,
                Icon::Refresh,
                "Update geo data",
                cx.core_status().running(),
                || UpdateGeo,
            );
        }
    });
    // Listeners other plugins label (`vless` → "VLESS clients").
    let labels = cx.core().labels();
    let inbounds = labels.inbounds().to_vec();
    ListEditor::new(&RULES)
        .add_form(
            "Add routing rule",
            || Rule {
                id: Id::random(),
                pattern: String::new(),
                action: Route::Proxy,
            },
            move |ui, r, width| rule_fields(ui, &inbounds, r, width),
        )
        // The type leads, as in the add form.
        .leading("Type", 76., |ui, _, r: &Rule| {
            kit::badge(ui, kind(&r.pattern), kit::Tone::Neutral);
        })
        .column(
            Field::edit("Pattern", "Domain, IP or network", |r: &mut Rule| {
                &mut r.pattern
            })
            .display(move |r: &Rule| {
                pattern_label(&labels, &r.pattern).unwrap_or_else(|| r.pattern.clone())
            }),
        )
        .column(Field::choice(
            "Route",
            120.,
            |r: &mut Rule| &mut r.action,
            Route::ALL,
        ))
        .subtitle(move |rows, i| rule_note(rows, i, &geo_status))
        .empty(
            Icon::ArrowsSplit,
            "No routing rules",
            "All traffic goes to the connected server until you add a rule.",
        )
        .show(ui, cx);
    // Where the rest goes, always in sight; a `*` rule in the list says it itself.
    if !cx.get(&RULES).iter().any(|r| r.pattern == "*") {
        ui.add_space(kit::theme::SPACE_S);
        kit::settings_card(ui, |ui| {
            kit::settings_row(
                ui,
                "Everything else",
                "Traffic no rule above matches",
                |ui| {
                    let mut action = Route::Proxy;
                    if kit::choice(ui, "rest", &mut action, Route::ALL, 120., "") {
                        let rule = Rule {
                            id: Id::random(),
                            pattern: "*".into(),
                            action,
                        };
                        cx.submit("rest", Add::new(&RULES, rule));
                    }
                },
            );
        });
    }
}

/// The line under a rule: shadowed by an earlier `*`, or how many entries
/// its geo list has.
fn rule_note(
    rows: &[Rule],
    i: usize,
    geo: &HashMap<String, GeoStatus>,
) -> Option<(String, kit::Tone)> {
    if rows[..i].iter().any(|r| r.pattern == "*") {
        return Some((
            "Never used: the * rule above matches everything first".into(),
            kit::Tone::Warning,
        ));
    }
    // Say, imported from a desktop install.
    if rows[i].pattern.starts_with("app:") && (sandboxed()) {
        return Some((
            "Never used: Flatpak and Docker hide other apps".into(),
            kit::Tone::Warning,
        ));
    }
    let (kind, name) = self::geo(&rows[i].pattern)?;
    let what = if kind == Geo::Site {
        ("domain", "domains")
    } else {
        ("network", "networks")
    };
    Some(match geo.get(&provider(kind, name).name) {
        Some(GeoStatus {
            rules,
            updated: Some(_),
        }) if *rules > 0 => (
            kit::plural(*rules as usize, what.0, what.1),
            kit::Tone::Neutral,
        ),
        Some(GeoStatus {
            updated: Some(_), ..
        }) => (
            "The list is empty. Check the name".into(),
            kit::Tone::Warning,
        ),
        _ => (
            "Not downloaded yet · Update geo data".into(),
            kit::Tone::Warning,
        ),
    })
}
