//! # routing
//! Ordered routing rules: domains (with `*`), IPs, CIDR networks and IPv4 prefixes
//! like `192.168.*`, each sent via the selected server, directly or blocked.
//! Rules become typed mihomo rules in the `User` stage; the first match wins.
//!
//! Geo rules (`geosite:category-ru`, `geoip:ru`) use rule sets from the
//! MetaCubeX geo database. The core downloads them, keeps them in its home
//! folder and refreshes them daily; the page can refresh them on demand.
use plugin_servers as servers;
use rproxy_kernel::mihomo::{RuleBehavior, RuleProvider};
use rproxy_kernel::{kit, mihomo::ipnet::IpNet, prelude::*, ui::Column, ui::ListEditor};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

/// Where geo rule sets come from (a CDN mirror of MetaCubeX/meta-rules-dat).
pub const GEO_SOURCE: &str = "https://cdn.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@meta/geo";
const GEO_REFRESH: u32 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Geo {
    Site,
    Ip,
}
/// `geosite:NAME` or `geoip:CODE` (already normalized to lowercase).
pub fn geo(pattern: &str) -> Option<(Geo, &str)> {
    let (kind, name) = pattern.split_once(':')?;
    let kind = match kind {
        "geosite" => Geo::Site,
        "geoip" => Geo::Ip,
        _ => return None,
    };
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_!@.".contains(&c));
    valid.then_some((kind, name))
}
/// The rule set behind a geo pattern.
pub fn provider(kind: Geo, name: &str) -> RuleProvider {
    let (folder, prefix, behavior) = match kind {
        Geo::Site => ("geosite", "geosite", RuleBehavior::Domain),
        Geo::Ip => ("geoip", "geoip", RuleBehavior::IpCidr),
    };
    RuleProvider {
        name: format!("{prefix}-{name}"),
        behavior,
        url: format!("{GEO_SOURCE}/{folder}/{name}.mrs"),
        interval_secs: GEO_REFRESH,
    }
}

/// Download state of one geo rule set, from the core.
#[derive(Clone, Debug, PartialEq)]
pub struct GeoStatus {
    pub rules: u64,
    pub updated: Option<String>,
}
/// Rule set name → state (refreshed every minute while geo rules exist).
pub static GEO: State<HashMap<String, GeoStatus>> = State::new("routing.geo");

/// Download every geo rule set again now (job "Update geo data").
pub struct UpdateGeo;
impl Op for UpdateGeo {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Update geo data").page("routing"))
    }
}
struct RefreshGeo;
impl Op for RefreshGeo {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Read geo data status").quiet())
    }
}
fn geo_providers(snap: &Snapshot) -> Vec<RuleProvider> {
    let mut out: Vec<RuleProvider> = Vec::new();
    for r in snap.get(&RULES).iter() {
        if let Some((kind, name)) = geo(&r.pattern) {
            let p = provider(kind, name);
            if !out.iter().any(|o| o.name == p.name) {
                out.push(p);
            }
        }
    }
    out
}
async fn read_geo(cx: &Cx) -> Result<()> {
    let all = cx.core().rule_providers().await?;
    let status: HashMap<String, GeoStatus> = geo_providers(&cx.snapshot())
        .into_iter()
        .map(|p| {
            let v = &all[&p.name];
            let updated = v["updatedAt"]
                .as_str()
                .filter(|t| !t.starts_with("0001"))
                .map(str::to_owned);
            (
                p.name,
                GeoStatus {
                    rules: v["ruleCount"].as_u64().unwrap_or(0),
                    updated,
                },
            )
        })
        .collect();
    cx.set_live(&GEO, status);
    Ok(())
}

pub static PLUGIN: Plugin = Plugin {
    id: "routing",
    name: "Routing",
    description: "Send domains and networks through the proxy, directly, or block them. The first match wins.",
    requires: &["servers"],
    setup,
    ..Plugin::DEFAULT
};

choices!(pub enum Route {
    Proxy => "Proxy" | "Via server",
    Direct => "Direct",
    Block => "Block",
});

choices!(
    /// The proxy mode of Clash-family apps: follow the rules, or send all
    /// traffic one way for a while without touching them.
    pub enum Mode {
        Rule => "Rule",
        Global => "Global",
        Direct => "Direct",
    }
);
// `choices!` takes no per-value attributes, so no `#[default]`.
#[allow(clippy::derivable_impls)]
impl Default for Mode {
    fn default() -> Self {
        Mode::Rule
    }
}
pub static MODE: State<Mode> = State::new("routing.mode");

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "routing_rules")]
pub struct Rule {
    pub id: Id,
    pub pattern: String,
    pub action: Route,
}
pub static RULES: State<Vec<Rule>> = State::new("routing.rules");

fn target(r: Route, via: &Target) -> Target {
    match r {
        Route::Proxy => via.clone(),
        Route::Direct => Target::Direct,
        Route::Block => Target::Reject,
    }
}

/// Translate a normalized pattern into a typed core rule.
/// `via` is where traffic sent via the server goes ([`servers::route`]).
pub fn core_rule(rule: &Rule, via: &Target) -> Result<CoreRule> {
    let (pattern, network) = rule_pattern(&rule.pattern)?;
    let t = target(rule.action, via);
    Ok(if let Some((kind, name)) = geo(&pattern) {
        // IP sets resolve domains, so they also match sites by name.
        CoreRule::RuleSet {
            name: provider(kind, name).name,
            target: t,
            no_resolve: false,
        }
    } else if pattern == "*" {
        CoreRule::Match(t)
    } else if let Some(app) = pattern.strip_prefix("app:") {
        CoreRule::ProcessName(app.to_owned(), t)
    } else if let Some(net) = network {
        CoreRule::IpCidr {
            net,
            target: t,
            no_resolve: true,
        }
    } else {
        CoreRule::DomainRegex(domain_regex(&pattern), t)
    })
}

/// Whether the core's description of a rule (`DomainRegex ^…$`) is `rule`.
fn same_rule(rule: &CoreRule, reported: &str) -> bool {
    let (kind, payload) = reported.split_once(' ').unwrap_or((reported, ""));
    match rule {
        CoreRule::DomainRegex(regex, _) => kind == "DomainRegex" && regex == payload,
        CoreRule::IpCidr { net, .. } => kind.starts_with("IPCIDR") && net.to_string() == payload,
        CoreRule::RuleSet { name, .. } => kind == "RuleSet" && name == payload,
        CoreRule::ProcessName(app, _) => kind == "ProcessName" && app == payload,
        _ => false,
    }
}

/// Like domain rules in other proxy apps: `example.com` and `*.example.com`
/// match the domain and its subdomains, `=example.com` only that name.
fn domain_regex(pattern: &str) -> String {
    let mut regex = String::with_capacity(pattern.len() + 12);
    regex.push('^');
    let domain = pattern.strip_prefix("*.").unwrap_or(pattern);
    let pattern = if let Some(exact) = pattern.strip_prefix('=') {
        exact
    } else if !domain.contains('*') {
        regex.push_str("(?:.*\\.)?");
        domain
    } else {
        pattern
    };
    let mut parts = pattern.split('*').enumerate().peekable();
    while let Some((index, part)) = parts.next() {
        let middle = index > 0 && parts.peek().is_some();
        if index > 0 {
            // Match each interior part once, like the name-filter wildcard.
            regex.push_str(if middle { "(?>.*?" } else { ".*" });
        }
        for c in part.chars() {
            if c == '.' {
                regex.push('\\');
            }
            regex.push(c);
        }
        if middle {
            regex.push(')');
        }
    }
    regex.push('$');
    regex
}

/// Normalize a pattern: lowercase, collapse `**`, canonical networks.
pub fn rule_pattern(pattern: &str) -> Result<(String, Option<IpNet>)> {
    // A pasted address: keep its host (https://www.youtube.com/watch → www.youtube.com).
    let pattern = pattern.trim();
    let pattern = match pattern.split_once("://") {
        Some((_, rest)) => rest.split(['/', '?', '#']).next().unwrap_or(rest),
        None => pattern,
    };
    let pattern = pattern
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(pattern, |(host, _)| host);
    // An app by its executable name; names keep their case.
    if let Some(app) = pattern.strip_prefix("app:") {
        let app = app.trim();
        if app.is_empty() || app.contains([',', '\n']) {
            return Err(Error::msg(
                "Enter an app name, for example app:Telegram or app:chrome.exe.",
            ));
        }
        return Ok((format!("app:{app}"), None));
    }
    let mut p = String::with_capacity(pattern.len());
    for c in pattern.chars().flat_map(char::to_lowercase) {
        if c != '*' || !p.ends_with('*') {
            p.push(c);
        }
    }
    if p == "*" || geo(&p).is_some() {
        return Ok((p, None));
    }
    if p.starts_with("geosite:") || p.starts_with("geoip:") {
        return Err(Error::msg(
            "Enter a geo list name, for example geosite:category-ru or geoip:ru.",
        ));
    }
    if let Ok(net) = p.parse::<IpNet>() {
        return Ok((net.trunc().to_string(), Some(net.trunc())));
    }
    if let Ok(ip) = p.parse::<IpAddr>() {
        return Ok((ip.to_string(), Some(ip.into())));
    }
    if p.bytes().any(|c| c.is_ascii_digit())
        && p.bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'*'))
    {
        let prefix = p
            .strip_suffix(".*")
            .ok_or_else(|| Error::msg("Use an IP, a CIDR network or a prefix like 192.168.*."))?;
        let octets: Vec<u8> = prefix
            .split('.')
            .map(|octet| {
                octet
                    .parse()
                    .ok()
                    .filter(|_| octet.len() == 1 || !octet.starts_with('0'))
            })
            .collect::<Option<_>>()
            .filter(|o: &Vec<u8>| (1..=3).contains(&o.len()))
            .ok_or_else(|| Error::msg("Invalid IPv4 prefix."))?;
        let mut a = [0; 4];
        a[..octets.len()].copy_from_slice(&octets);
        return Ok((
            p,
            Some(IpNet::new(Ipv4Addr::from(a).into(), (octets.len() * 8) as u8).unwrap()),
        ));
    }
    let name = p.strip_prefix('=').unwrap_or(&p);
    if name.is_empty()
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-*".contains(&c))
    {
        return Err(Error::msg(
            "Enter a domain pattern, IP address or CIDR network.",
        ));
    }
    Ok((p, None))
}
/// A new rule goes above the catch-all `*`: below it, nothing would ever reach it.
async fn above_catch_all(cx: Cx, mut op: Add<Rule>, next: Next<Add<Rule>>) -> Result<Id> {
    if op.at.is_none() && op.item.pattern.trim() != "*" {
        op.at = cx.get(&RULES).iter().position(|r| r.pattern.trim() == "*");
    }
    next.run(&cx, op).await
}

fn setup(p: &mut Setup) -> Result<()> {
    p.wrap(above_catch_all);
    p.list(&RULES).validate(|_, rule, _, others| {
        let (pattern, network) = rule_pattern(&rule.pattern)?;
        let clash = others.iter().any(|r| {
            r.pattern == pattern
                || network.is_some()
                    && rule_pattern(&r.pattern).ok().and_then(|(_, n)| n) == network
        });
        if clash {
            bail!("Routing pattern already exists.");
        }
        rule.pattern = pattern;
        Ok(())
    });
    p.live(&GEO, HashMap::new);
    p.editable_setting(&MODE);
    p.core(|snap| {
        let via = servers::route(snap);
        let rules: Vec<CoreRule> = match *snap.get(&MODE) {
            Mode::Rule => snap
                .get(&RULES)
                .iter()
                .filter_map(|r| core_rule(r, &via).ok())
                .collect(),
            // The final rule already sends everything through the proxy.
            Mode::Global => Vec::new(),
            Mode::Direct => vec![CoreRule::Match(Target::Direct)],
        };
        Fragment {
            rule_providers: geo_providers(snap),
            ..Fragment::rules(Stage::User, rules)
        }
    });
    p.fill(&servers::CONNECTION, "mode", 5, |ui, cx, _| {
        let mode = *cx.get(&MODE);
        if mode != Mode::Rule {
            kit::tone_text(ui, mode_help(mode), kit::Tone::Warning);
        }
    });
    p.fill(&rproxy_kernel::shell::STATUS, "mode", 3, |ui, cx, _| {
        let mode = *cx.get(&MODE);
        if mode != Mode::Rule {
            let pal = kit::colors(ui);
            let tip = mode_help(mode);
            cx.status_chip(
                ui,
                kit::Lead::Icon(Icon::Routing, pal.warning),
                &format!("{mode} mode"),
                tip,
            );
        }
    });
    p.handle(|cx: Cx, _: UpdateGeo| async move {
        let sets = geo_providers(&cx.snapshot());
        let total = Some(sets.len() as u64);
        let mut failed = Vec::new();
        for (i, p) in sets.iter().enumerate() {
            cx.checkpoint()?;
            if cx.core().update_rule_provider(&p.name).await.is_err() {
                failed.push(p.name.clone());
            }
            cx.progress(i as u64 + 1, total);
        }
        let _ = read_geo(&cx).await;
        if !failed.is_empty() {
            bail!("Could not download {}.", failed.join(", "));
        }
        cx.summary(kit::plural(
            sets.len(),
            "geo list updated",
            "geo lists updated",
        ));
        Ok(())
    });
    p.handle(|cx: Cx, _: RefreshGeo| async move { read_geo(&cx).await });
    p.every(
        "geo-status",
        |s| (!geo_providers(s).is_empty()).then_some(Duration::from_secs(60)),
        || RefreshGeo,
    );
    // New geo rules download in the background; show their state soon after
    // the last of several quick edits.
    p.on_change([RULES.name], |cx: Cx| async move {
        if !geo_providers(&cx.snapshot()).is_empty() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            read_geo(&cx).await?;
        }
        Ok(())
    })
    .latest();
    p.page_fn(
        PageSpec {
            id: "routing",
            title: "Routing",
            icon: Icon::Routing,
            order: 30,
        },
        page,
    );
    p.hook(&servers::RULE_LABEL, |snap, rule| {
        let via = servers::route(snap);
        let made = |r: &Rule| core_rule(r, &via).is_ok_and(|c| same_rule(&c, rule));
        snap.get(&RULES)
            .iter()
            .find(|r| made(r))
            .map(|r| r.pattern.clone())
    });
    Ok(())
}

/// What a pattern matches, for the small type badge.
fn kind(pattern: &str) -> &'static str {
    match rule_pattern(pattern) {
        Ok((p, _)) if p == "*" => "Catch-all",
        Ok((p, _)) if p.starts_with("app:") => "App",
        Ok((p, _)) if p.starts_with("geosite:") => "GeoSite",
        Ok((p, _)) if p.starts_with("geoip:") => "GeoIP",
        Ok((_, Some(net))) if net.prefix_len() == net.max_prefix_len() => "IP",
        Ok((_, Some(_))) => "Network",
        _ => "Domain",
    }
}

fn mode_help(mode: Mode) -> &'static str {
    match mode {
        Mode::Rule => "Rule: traffic follows the routing rules below.",
        Mode::Global => "Rules are paused: Global mode sends all traffic through the proxy.",
        Mode::Direct => "Rules are paused: Direct mode sends no traffic through the proxy.",
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let geo_status = (*cx.live(&GEO)).clone();
    let n = cx.get(&RULES).len();
    let counter = kit::count(n, "rule", "rules");
    let has_geo = !geo_providers(cx.snapshot()).is_empty();
    kit::page_header(ui, "Routing", &counter, |ui| {
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
    let mut mode = *cx.get(&MODE);
    kit::settings_card(ui, |ui| {
        kit::settings_row(ui, "Mode", "", |ui| {
            let options = [
                (Mode::Rule, "Rule"),
                (Mode::Global, "Global"),
                (Mode::Direct, "Direct"),
            ];
            if kit::segmented(ui, &mut mode, &options) {
                cx.update("mode", &MODE, move |m| *m = mode);
            }
        });
    });
    kit::page_note(
        ui,
        if mode == Mode::Rule {
            "Rules are checked from the top; the first match wins. Traffic no rule matches goes through the proxy."
        } else {
            mode_help(mode)
        },
    );

    // Paused rules stay editable but look it.
    if mode != Mode::Rule {
        ui.multiply_opacity(0.5);
    }
    ListEditor::new(&RULES)
        .add_form("Add routing rule", || Rule { id: Id::random(), pattern: String::new(), action: Route::Proxy }, |ui, r, width| {
            kit::Fields::new(width)
                .input("pattern", &mut r.pattern, "Domain, IP, network or app:Name")
                .help("example.org also covers its subdomains; =example.org only that name; *tube* any name containing tube. Also 10.0.0.0/8, geosite:category-ru, geoip:ru, app:Telegram (an app's traffic), or a pasted link.")
                .choice("route", &mut r.action, Route::ALL)
                .show(ui)
                && !r.pattern.trim().is_empty()
        })
        .column(Column::edit("Pattern", "Domain, IP or network", |r: &mut Rule| &mut r.pattern))
        .column(Column::custom("Type", 74., |ui, _, r: &Rule| {
            kit::badge(ui, kind(&r.pattern), kit::Tone::Neutral);
        }))
        .column(Column::choice("Route", 120., |r: &mut Rule| &mut r.action, Route::ALL))
        .subtitle(move |rows, i| rule_note(rows, i, &geo_status))
        .empty(Icon::Routing, "No routing rules", "Everything goes through the proxy.")
        .show(ui, cx);
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
            "Not downloaded yet. Check the name or update geo data".into(),
            kit::Tone::Warning,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_rules_map_back_to_the_users_pattern() {
        let rule = |pattern: &str| Rule {
            id: Id::random(),
            pattern: pattern.into(),
            action: Route::Direct,
        };
        let made = core_rule(&rule("*.youtube.com"), &Target::Direct).unwrap();
        let CoreRule::DomainRegex(regex, _) = &made else {
            panic!("{made:?}")
        };
        assert!(same_rule(&made, &format!("DomainRegex {regex}")));
        assert!(!same_rule(&made, "DomainRegex ^other$"));
        let net = core_rule(&rule("10.0.0.0/8"), &Target::Direct).unwrap();
        assert!(same_rule(&net, "IPCIDR 10.0.0.0/8"));
    }
    #[test]
    fn patterns_normalize_without_ambiguous_prefixes() {
        assert!(rule_pattern("192.900.*").is_err());
        assert_eq!(
            rule_pattern("192.168.*").unwrap().1.unwrap().to_string(),
            "192.168.0.0/16"
        );
        assert_eq!(rule_pattern("**.Example.ORG").unwrap().0, "*.example.org");
        assert_eq!(rule_pattern("10.1.2.3/8").unwrap().0, "10.0.0.0/8");
        assert_eq!(
            rule_pattern("GeoSite:Category-RU").unwrap().0,
            "geosite:category-ru"
        );
        assert!(rule_pattern("geoip:").is_err() && rule_pattern("geosite:a b").is_err());
        let g = Rule {
            id: Id::random(),
            pattern: "geoip:ru".into(),
            action: Route::Direct,
        };
        assert_eq!(
            core_rule(&g, &servers::main_target()).unwrap().wire(),
            "RULE-SET,geoip-ru,DIRECT"
        );
        assert_eq!(
            provider(Geo::Site, "category-ru").url,
            format!("{GEO_SOURCE}/geosite/category-ru.mrs")
        );
        let wire = |pattern: &str| {
            let r = Rule {
                id: Id::random(),
                pattern: pattern.into(),
                action: Route::Direct,
            };
            core_rule(&r, &servers::main_target()).unwrap().wire()
        };
        // A domain also covers its subdomains, as in other proxy apps.
        let suffix = "DOMAIN-REGEX,^(?:.*\\.)?example\\.org$,DIRECT";
        assert_eq!(wire("*.example.org"), suffix);
        assert_eq!(wire("example.org"), suffix);
        assert_eq!(wire("https://Example.org:8443/path?q=1"), suffix);
        assert_eq!(wire("=example.org"), "DOMAIN-REGEX,^example\\.org$,DIRECT");
        assert_eq!(wire("app: Telegram"), "PROCESS-NAME,Telegram,DIRECT");
        assert!(rule_pattern("app:").is_err());
        assert_eq!(wire("*tube*"), "DOMAIN-REGEX,^(?>.*?tube).*$,DIRECT");
    }
}
