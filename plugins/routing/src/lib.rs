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
        Some(JobSpec::keyed("routing/geo", "Update geo data").page("routing"))
    }
}
struct RefreshGeo;
impl Op for RefreshGeo {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed("routing/geo-status", "Read geo data status").quiet())
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
    description: "Send domains and networks via the server, directly, or block them. The first match wins.",
    category: categories::NETWORK,
    requires: &["servers"],
    required: false,
    default_enabled: true,
    platforms: Platform::ALL,
    setup,
};

choices!(pub enum Route {
    Proxy => "Via server",
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
    } else if let Some(net) = network {
        CoreRule::IpCidr { net, target: t, no_resolve: true }
    } else {
        CoreRule::DomainRegex(domain_regex(&pattern), t)
    })
}

fn domain_regex(pattern: &str) -> String {
    let mut regex = String::with_capacity(pattern.len() + 2);
    regex.push('^');
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
    let mut p = String::with_capacity(pattern.len());
    for c in pattern.trim().chars().flat_map(char::to_lowercase) {
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
        let octets = prefix
            .split('.')
            .map(|octet| {
                if octet.len() > 1 && octet.starts_with('0') {
                    return Err(Error::msg("Invalid IPv4 prefix."));
                }
                octet
                    .parse::<u8>()
                    .map_err(|_| Error::msg("Invalid IPv4 prefix."))
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::msg("Invalid IPv4 prefix."))?;
        if !(1..=3).contains(&octets.len()) {
            return Err(Error::msg("Invalid IPv4 prefix."));
        }
        let mut a = [0; 4];
        a[..octets.len()].copy_from_slice(&octets);
        return Ok((
            p,
            Some(IpNet::new(Ipv4Addr::from(a).into(), (octets.len() * 8) as u8).unwrap()),
        ));
    }
    if p.is_empty()
        || !p
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-*".contains(&c))
    {
        return Err(Error::msg(
            "Enter a domain pattern, IP address or CIDR network.",
        ));
    }
    Ok((p, None))
}
fn setup(p: &mut Setup) -> Result<()> {
    p.list(&RULES).validate(|_, rule, _, others| {
        let (pattern, network) = rule_pattern(&rule.pattern)?;
        let clash = others.iter().any(|r| {
            r.pattern == pattern || network.is_some() && rule_pattern(&r.pattern).ok().and_then(|(_, n)| n) == network
        });
        if clash {
            bail!("Routing pattern already exists.");
        }
        rule.pattern = pattern;
        Ok(())
    });
    p.live(&GEO, HashMap::new);
    p.core(|snap| {
        let via = servers::route(snap);
        Fragment {
            rule_providers: geo_providers(snap),
            ..Fragment::rules(
                Stage::User,
                snap.get(&RULES)
                    .iter()
                    .filter_map(|r| core_rule(r, &via).ok()),
            )
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
    // New geo rules download in the background; show their state soon after.
    p.on_change([RULES.name], |cx: Cx| async move {
        if !geo_providers(&cx.snapshot()).is_empty() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            read_geo(&cx).await?;
        }
        Ok(())
    });
    p.page_fn(PageSpec { id: "routing", title: "Routing", icon: Icon::Routing, order: 30 }, page);
    Ok(())
}

/// What a pattern matches, for the small type badge.
fn kind(pattern: &str) -> &'static str {
    match rule_pattern(pattern) {
        Ok((p, _)) if p == "*" => "Everything",
        Ok((p, _)) if p.starts_with("geosite:") => "Geo sites",
        Ok((p, _)) if p.starts_with("geoip:") => "Geo IPs",
        Ok((_, Some(net))) if net.prefix_len() == net.max_prefix_len() => "IP",
        Ok((_, Some(_))) => "Network",
        _ => "Domain",
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let geo_status = (*cx.live(&GEO)).clone();
    let n = cx.get(&RULES).len();
    let counter = if n == 0 {
        String::new()
    } else {
        kit::plural(n, "rule", "rules")
    };
    let has_geo = !geo_providers(cx.snapshot()).is_empty();
    kit::page_header(ui, "Routing", &counter, |ui| {
        if has_geo {
            cx.job_button(
                ui,
                Icon::Refresh,
                "Update geo data",
                140.,
                cx.core_status().running(),
                || UpdateGeo,
            );
        }
    });
    kit::helper(ui, "Rules apply top to bottom and the first match wins. Other traffic goes via the selected server. Geo lists: geosite:category-ru, geoip:ru; put site lists above IP lists.");
    ListEditor::new(&RULES)
        .add_form("Add routing rule", || Rule { id: Id::random(), pattern: String::new(), action: Route::Proxy }, |ui, r, width| {
            let w = (width - 120. - kit::theme::HORIZONTAL_GAP).max(60.);
            let e = kit::input(ui, "pattern", &mut r.pattern, "Domain, IP or network", w, false)
                .on_hover_text("Examples: *.example.org, 10.0.0.0/8, 192.168.*, geosite:category-ru, geoip:ru");
            kit::choice(ui, "route", &mut r.action, Route::ALL, 120., "Route");
            kit::submitted(ui, &e) && !r.pattern.trim().is_empty()
        })
        .column(Column::edit("Pattern", "Domain, IP or network", |r: &mut Rule| &mut r.pattern))
        .column(Column::custom("Type", 74., |ui, _, r: &Rule| {
            kit::badge(ui, kind(&r.pattern), kit::Tone::Neutral);
        }))
        .column(Column::choice("Route", 120., |r: &mut Rule| &mut r.action, Route::ALL))
        .subtitle(move |rows, i| {
            if rows[..i].iter().any(|r| r.pattern == "*") {
                return Some(("Never used: the * rule above matches everything first".into(), kit::Tone::Warning));
            }
            let (kind, name) = geo(&rows[i].pattern)?;
            let status = geo_status.get(&provider(kind, name).name).cloned();
            let what = if kind == Geo::Site { ("domain", "domains") } else { ("network", "networks") };
            Some(match status {
                Some(GeoStatus { rules, updated: Some(_) }) if rules > 0 => (kit::plural(rules as usize, what.0, what.1), kit::Tone::Neutral),
                Some(GeoStatus { updated: Some(_), .. }) => ("The list is empty. Check the name".into(), kit::Tone::Warning),
                _ => ("Not downloaded yet. Check the name or update geo data".into(), kit::Tone::Warning),
            })
        })
        .empty(Icon::Routing, "No routing rules", "All traffic goes via the selected server. Add a rule to send a domain or network directly or block it.")
        .show(ui, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patterns_normalize_without_ambiguous_prefixes() {
        assert!(rule_pattern("192.900.*").is_err());
        assert_eq!(rule_pattern("192.168.*").unwrap().1.unwrap().to_string(), "192.168.0.0/16");
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
        let r = Rule {
            id: Id::random(),
            pattern: "*.example.org".into(),
            action: Route::Direct,
        };
        assert_eq!(
            core_rule(&r, &servers::main_target()).unwrap().wire(),
            "DOMAIN-REGEX,^.*\\.example\\.org$,DIRECT"
        );
    }
}
