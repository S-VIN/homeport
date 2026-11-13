//! # routing
//! Ordered routing rules: domains (with `*`), IPs, CIDR networks and IPv4 prefixes
//! like `192.168.*`, each sent via the selected server, directly or blocked.
//! Rules become typed mihomo rules in the `User` stage; the first match wins.
use plugin_servers as servers;
use rproxy_kernel::{kit, mihomo::ipnet::IpNet, prelude::*, ui::Column, ui::ListEditor};
use std::net::{IpAddr, Ipv4Addr};

pub static PLUGIN: Plugin = Plugin {
    id: "routing",
    name: "Routing",
    description: "Send domains and networks via the server, directly, or block them. The first match wins.",
    category: categories::NETWORK,
    requires: &["servers"],
    required: false,
    default_enabled: true,
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
    Ok(if pattern == "*" {
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
    if p == "*" {
        return Ok((p, None));
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
    p.core(|snap| {
        let via = servers::route(snap);
        Fragment::rules(Stage::User, snap.get(&RULES).iter().filter_map(|r| core_rule(r, &via).ok()))
    });
    p.page_fn(PageSpec { id: "routing", title: "Routing", icon: Icon::Routing, order: 30 }, page);
    Ok(())
}

/// What a pattern matches, for the small type badge.
fn kind(pattern: &str) -> &'static str {
    match rule_pattern(pattern) {
        Ok((p, _)) if p == "*" => "Everything",
        Ok((_, Some(net))) if net.prefix_len() == net.max_prefix_len() => "IP",
        Ok((_, Some(_))) => "Network",
        _ => "Domain",
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let n = cx.get(&RULES).len();
    let counter = if n == 0 { String::new() } else { kit::plural(n, "rule", "rules") };
    kit::page_header(ui, "Routing", &counter, |_| {});
    kit::helper(ui, "Rules apply top to bottom and the first match wins. Other traffic goes via the selected server.");
    ListEditor::new(&RULES)
        .add_form("Add routing rule", || Rule { id: Id::random(), pattern: String::new(), action: Route::Proxy }, |ui, r, width| {
            let w = (width - 120. - kit::theme::HORIZONTAL_GAP).max(60.);
            let e = kit::input(ui, "pattern", &mut r.pattern, "Domain, IP or network", w, false)
                .on_hover_text("Examples: *.example.org, 10.0.0.0/8, 192.168.*");
            kit::choice(ui, "route", &mut r.action, Route::ALL, 120., "Route");
            kit::submitted(ui, &e) && !r.pattern.trim().is_empty()
        })
        .column(Column::edit("Pattern", "Domain, IP or network", |r: &mut Rule| &mut r.pattern))
        .column(Column::custom("Type", 74., |ui, _, r: &Rule| {
            kit::badge(ui, kind(&r.pattern), kit::Tone::Neutral);
        }))
        .column(Column::choice("Route", 120., |r: &mut Rule| &mut r.action, Route::ALL))
        .subtitle(|rows, i| {
            rows[..i].iter().any(|r| r.pattern == "*").then(|| ("Never used: the * rule above matches everything first".into(), kit::Tone::Warning))
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
        let r = Rule { id: Id::random(), pattern: "*.example.org".into(), action: Route::Direct };
        assert_eq!(core_rule(&r, &servers::main_target()).unwrap().wire(), "DOMAIN-REGEX,^.*\\.example\\.org$,DIRECT");
    }
}
