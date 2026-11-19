//! # routing
//! Ordered routing rules: domains (with `*`), IPs, CIDR networks and IPv4 prefixes
//! like `192.168.*`, each sent via the selected server, directly or blocked.
//! Rules become typed mihomo rules in the `User` stage; the first match wins.
//!
//! Geo rules (`geosite:category-ru`, `geoip:ru`) use rule sets from the
//! MetaCubeX geo database. The core downloads them, keeps them in its home
//! folder and refreshes them daily; the page can refresh them on demand.
use homeport_kernel::mihomo::{RuleBehavior, RuleProvider};
use homeport_kernel::{kit, mihomo::ipnet::IpNet, prelude::*};
use plugin_servers as servers;

mod ui;
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
        Some(JobSpec::new("Update geo data").page(&PLUGIN))
    }
}
struct RefreshGeo;
impl Op for RefreshGeo {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Read geo data status").quiet())
    }
}
/// Apps can't be matched by process name inside a sandbox (Flatpak, Docker).
fn sandboxed() -> bool {
    Platform::is_flatpak() || Platform::is_docker()
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
            let v = all.get(&p.name).cloned().unwrap_or_default();
            (
                p.name,
                GeoStatus {
                    rules: v.rule_count,
                    updated: v.updated_at,
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
    requires: &[&plugin_servers::PLUGIN],
    required: true,
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

choices!(pub enum Route {
    Proxy => "Proxy",
    Direct => "Direct",
    Block => "Block",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "routing_rules")]
pub struct Rule {
    pub id: Id,
    pub pattern: String,
    pub action: Route,
}
pub static RULES: State<Vec<Rule>> = State::new("routing.rules");

/// Send traffic matching `pattern` this way: changes the route of the rule
/// with this pattern, or adds the rule at the top.
pub struct SetRoute {
    pub pattern: String,
    pub action: Route,
}
impl Op for SetRoute {
    type Reply = ();
    const SAVES: bool = true;
}
/// How traffic matching exactly `pattern` goes: the first rule with this
/// pattern or for everything decides.
pub fn route_for(snap: &Snapshot, pattern: &str) -> Route {
    snap.get(&RULES)
        .iter()
        .find(|r| r.pattern == pattern || r.pattern == "*")
        .map_or(Route::Proxy, |r| r.action)
}
async fn set_route(cx: Cx, op: SetRoute) -> Result<()> {
    let existing = cx
        .get(&RULES)
        .iter()
        .find(|r| r.pattern == op.pattern)
        .cloned();
    match existing {
        Some(r) => {
            let rule = Rule {
                action: op.action,
                ..r
            };
            cx.call(Save::new(&RULES, rule)).await.map(drop)
        }
        None => {
            let rule = Rule {
                id: Id::random(),
                pattern: op.pattern,
                action: op.action,
            };
            let add = Add {
                at: Some(0),
                ..Add::new(&RULES, rule)
            };
            cx.call(add).await.map(drop)
        }
    }
}

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
    } else if let Some(name) = pattern.strip_prefix("inbound:") {
        CoreRule::InName(name.to_owned(), t)
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
        // The sandbox hides other processes from the core.
        if sandboxed() {
            return Err(Error::msg(
                "App rules need the regular install: Flatpak and Docker hide other apps.",
            ));
        }
        if app.is_empty() || app.contains([',', '\n']) {
            return Err(Error::msg(
                "Enter an app name, for example Telegram or chrome.exe.",
            ));
        }
        return Ok((format!("app:{app}"), None));
    }
    if let Some(name) = pattern.strip_prefix("inbound:") {
        let name = name.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            return Err(Error::msg("Choose an inbound."));
        }
        return Ok((format!("inbound:{name}"), None));
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
    if p.starts_with("geoip:") {
        return Err(Error::msg("Choose a country."));
    }
    if p.starts_with("geosite:") {
        return Err(Error::msg("Enter a list name, for example category-ru."));
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
    p.list(&RULES)
        .validate(|_, rule, _, others| validate(rule, others));
    p.wrap(above_catch_all);
    p.handle(set_route);
    p.on(rule_added);
    p.on(rule_saved);
    p.on(rule_removed);
    p.live(&GEO, HashMap::new);
    p.core(fragment);
    p.handle(update_geo);
    p.handle(|cx: Cx, _: RefreshGeo| async move { read_geo(&cx).await });
    p.every(
        "geo-status",
        |s| (!geo_providers(s).is_empty()).then_some(Duration::from_secs(60)),
        || RefreshGeo,
    );
    p.watch(|s| s.get(&RULES), geo_after_edit).latest();
    p.page_fn(
        PageSpec {
            title: "Routing",
            icon: Icon::ArrowsSplit,
            order: 30,
        },
        ui::page,
    );
    Ok(())
}

/// A rule's pattern in its normal form, unless another rule has it.
fn validate(rule: &mut Rule, others: &[Rule]) -> Result<()> {
    let (pattern, network) = rule_pattern(&rule.pattern)?;
    let clash = others.iter().any(|r| {
        r.pattern == pattern
            || network.is_some() && rule_pattern(&r.pattern).ok().and_then(|(_, n)| n) == network
    });
    if clash {
        bail!("Routing pattern already exists.");
    }
    rule.pattern = pattern;
    Ok(())
}

async fn rule_added(_: Cx, e: Added<Rule>) -> Result<()> {
    info!("Rule added: {} → {}", e.item.pattern, e.item.action);
    Ok(())
}
async fn rule_saved(_: Cx, e: Saved<Rule>) -> Result<()> {
    info!("Rule changed: {} → {}", e.item.pattern, e.item.action);
    Ok(())
}
async fn rule_removed(_: Cx, e: Removed<Rule>) -> Result<()> {
    info!("Rule removed: {}", e.item.pattern);
    Ok(())
}

/// The rules in the core: the user's in order, then whatever no rule caught
/// goes via the server.
fn fragment(snap: &Snapshot) -> Fragment {
    let via = servers::route(snap);
    let (mut rules, mut labels) = (Vec::new(), Vec::new());
    for r in snap.get(&RULES).iter() {
        let Ok(core) = core_rule(r, &via) else {
            continue;
        };
        // Connections and Logs show the rule as the user wrote it.
        if r.pattern != "*" {
            labels.push((core.reported(), r.pattern.clone()));
        }
        rules.push(core);
    }
    labels.push(("Match".into(), "Everything else".into()));
    let mut fragment = Fragment::rules(Stage::User, rules);
    fragment.rules.push((Stage::Final, CoreRule::Match(via)));
    Fragment {
        rule_providers: geo_providers(snap),
        labels,
        ..fragment
    }
}

async fn update_geo(cx: Cx, _: UpdateGeo) -> Result<()> {
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
    info!("Geo lists updated: {}", sets.len());
    cx.summary(kit::plural(
        sets.len(),
        "geo list updated",
        "geo lists updated",
    ));
    Ok(())
}

/// New geo rules download in the background: show their state soon after
/// the last of several quick edits.
async fn geo_after_edit(cx: Cx, _: std::sync::Arc<Vec<Rule>>) -> Result<()> {
    if !geo_providers(&cx.snapshot()).is_empty() {
        tokio::time::sleep(Duration::from_secs(3)).await;
        read_geo(&cx).await?;
    }
    Ok(())
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
        assert_eq!(made.reported(), format!("DomainRegex {regex}"));
        let net = core_rule(&rule("10.0.0.0/8"), &Target::Direct).unwrap();
        assert_eq!(net.reported(), "IPCIDR 10.0.0.0/8");
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
            core_rule(&g, &Target::Group(servers::MAIN.into()))
                .unwrap()
                .wire(),
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
            core_rule(&r, &Target::Group(servers::MAIN.into()))
                .unwrap()
                .wire()
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
