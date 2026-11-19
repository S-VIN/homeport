//! Typed configuration fragments. Plugins describe what they need; the composer
//! merges fragments deterministically and is the only place that knows mihomo's
//! YAML/JSON shape.
use ipnet::IpNet;
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, net::IpAddr};

/// Where a connection goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Proxy(String),
    Group(String),
    Direct,
    Reject,
}
impl Target {
    pub fn wire(&self) -> String {
        match self {
            Self::Proxy(n) | Self::Group(n) => n.clone(),
            Self::Direct => "DIRECT".into(),
            Self::Reject => "REJECT".into(),
        }
    }
}

/// Rule order is decided by stage first, then by plugin order, then by entry order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// Reserved for the kernel (isolated check route).
    System,
    Early,
    User,
    Late,
    /// Exactly one final MATCH rule.
    Final,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    Match(Target),
    Domain(String, Target),
    DomainSuffix(String, Target),
    DomainKeyword(String, Target),
    DomainRegex(String, Target),
    IpCidr {
        net: IpNet,
        target: Target,
        no_resolve: bool,
    },
    GeoIp(String, Target),
    InName(String, Target),
    /// Traffic of one app, by its executable name (`Telegram`, `chrome.exe`).
    ProcessName(String, Target),
    /// Match a [`RuleProvider`] by name. IP sets resolve domains unless `no_resolve`.
    RuleSet {
        name: String,
        target: Target,
        no_resolve: bool,
    },
}
impl Rule {
    pub fn wire(&self) -> String {
        match self {
            Self::Match(t) => format!("MATCH,{}", t.wire()),
            Self::Domain(d, t) => format!("DOMAIN,{d},{}", t.wire()),
            Self::DomainSuffix(d, t) => format!("DOMAIN-SUFFIX,{d},{}", t.wire()),
            Self::DomainKeyword(d, t) => format!("DOMAIN-KEYWORD,{d},{}", t.wire()),
            Self::DomainRegex(r, t) => format!("DOMAIN-REGEX,{r},{}", t.wire()),
            Self::IpCidr {
                net,
                target,
                no_resolve,
            } => {
                let kind = if net.addr().is_ipv4() {
                    "IP-CIDR"
                } else {
                    "IP-CIDR6"
                };
                let suffix = if *no_resolve { ",no-resolve" } else { "" };
                format!("{kind},{net},{}{suffix}", target.wire())
            }
            Self::GeoIp(c, t) => format!("GEOIP,{c},{}", t.wire()),
            Self::InName(n, t) => format!("IN-NAME,{n},{}", t.wire()),
            Self::ProcessName(n, t) => format!("PROCESS-NAME,{n},{}", t.wire()),
            Self::RuleSet {
                name,
                target,
                no_resolve,
            } => {
                format!(
                    "RULE-SET,{name},{}{}",
                    target.wire(),
                    if *no_resolve { ",no-resolve" } else { "" }
                )
            }
        }
    }
    /// How the core names the rule in connections and logs (`DomainRegex ^…$`).
    pub fn reported(&self) -> String {
        let (kind, payload) = match self {
            Self::Match(_) => return "Match".into(),
            Self::Domain(d, _) => ("Domain", d.clone()),
            Self::DomainSuffix(d, _) => ("DomainSuffix", d.clone()),
            Self::DomainKeyword(d, _) => ("DomainKeyword", d.clone()),
            Self::DomainRegex(r, _) => ("DomainRegex", r.clone()),
            Self::IpCidr { net, .. } => ("IPCIDR", net.to_string()),
            Self::GeoIp(c, _) => ("GeoIP", c.clone()),
            Self::InName(n, _) => ("InName", n.clone()),
            Self::ProcessName(n, _) => ("ProcessName", n.clone()),
            Self::RuleSet { name, .. } => ("RuleSet", name.clone()),
        };
        format!("{kind} {payload}")
    }
    fn target(&self) -> &Target {
        match self {
            Self::Match(t)
            | Self::Domain(_, t)
            | Self::DomainSuffix(_, t)
            | Self::DomainKeyword(_, t)
            | Self::DomainRegex(_, t)
            | Self::GeoIp(_, t)
            | Self::InName(_, t)
            | Self::ProcessName(_, t) => t,
            Self::IpCidr { target, .. } | Self::RuleSet { target, .. } => target,
        }
    }
}

/// An outbound. `options` holds protocol fields already validated by the
/// plugin that imported them (TLS, transport, credentials …).
#[derive(Clone, Debug, PartialEq)]
pub struct Proxy {
    pub name: String,
    pub protocol: String,
    pub server: String,
    pub port: u16,
    pub options: Map<String, Value>,
    /// Dial the server through another proxy or group (a chain).
    pub dialer: Option<Target>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GroupKind {
    Select,
    UrlTest { url: String, interval_secs: u32 },
    Fallback { url: String, interval_secs: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub name: String,
    pub kind: GroupKind,
    pub members: Vec<Target>,
    /// For `Select` groups: the member that carries traffic. `None` blocks.
    pub selected: Option<Target>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenerKind {
    Mixed,
    Socks,
    Http,
    /// VLESS over REALITY (`Listener::users` is ignored; users live here).
    Vless(Box<VlessListener>),
}

/// A VLESS server: REALITY camouflage plus an optional XHTTP transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VlessListener {
    /// `(name, uuid)` of every client.
    pub users: Vec<(String, String)>,
    pub reality: Reality,
    /// `None` = raw TCP.
    pub xhttp: Option<Xhttp>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reality {
    /// Where unauthenticated connections go (`host:port` of a real TLS site).
    pub dest: String,
    pub private_key: String,
    pub short_ids: Vec<String>,
    pub server_names: Vec<String>,
}
/// Server side of XHTTP. Ranges like `"100-1000"` are randomized per request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Xhttp {
    pub path: String,
    pub padding_bytes: String,
    pub stream_up_server_secs: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Listener {
    pub name: String,
    pub kind: ListenerKind,
    pub listen: IpAddr,
    pub port: u16,
    pub udp: bool,
    pub users: Vec<(String, String)>,
    /// An optional listener that cannot bind is reported and skipped instead of
    /// failing the whole configuration.
    pub optional: bool,
}

/// The core's own resolver. With the tunnel it also answers the apps
/// (port 53 is captured only while a resolver is configured).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dns {
    pub enable: bool,
    /// Server addresses as mihomo takes them: `https://…`, `tls://…`, an IP or `system`.
    pub nameservers: Vec<String>,
    /// Answer apps with addresses from 198.18.0.0/16 and keep the real name
    /// for the rules (mihomo `fake-ip`); otherwise real addresses.
    pub fake_ip: bool,
    /// Fixed answers: domain (`*` and `+.` wildcards) → IP address.
    pub hosts: Vec<(String, String)>,
}

/// Names that must get real addresses in fake-IP mode: the local network,
/// time servers, STUN and the Windows connectivity check.
const FAKE_IP_FILTER: &[&str] = &[
    "*.lan",
    "*.local",
    "*.localdomain",
    "localhost",
    "+.ntp.org",
    "time.*.com",
    "+.stun.*.*",
    "+.stun.*.*.*",
    "+.msftconnecttest.com",
    "+.msftncsi.com",
];

/// What a rule set contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleBehavior {
    Domain,
    IpCidr,
}
/// A rule set downloaded and refreshed by the core (mihomo `rule-providers`).
/// Files are kept in the core's home folder (`rules/<name>.mrs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleProvider {
    pub name: String,
    pub behavior: RuleBehavior,
    /// URL of a binary `.mrs` rule set.
    pub url: String,
    pub interval_secs: u32,
}

/// Capture all traffic of the machine through a virtual interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tun {
    /// Block traffic that would bypass the tunnel (Linux, Windows).
    pub strict_route: bool,
}

/// One plugin's contribution.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Fragment {
    pub proxies: Vec<Proxy>,
    pub groups: Vec<Group>,
    pub rules: Vec<(Stage, Rule)>,
    pub listeners: Vec<Listener>,
    pub dns: Option<Dns>,
    /// Detect domains from TLS/HTTP/QUIC so domain rules match connections by IP.
    pub sniff: bool,
    pub rule_providers: Vec<RuleProvider>,
    /// At most one plugin may turn the tunnel on.
    pub tun: Option<Tun>,
    /// Names of this fragment in the user's words: proxies, groups, listeners
    /// and rules as the core reports them ([`Rule::reported`]). Not sent to the core.
    pub labels: Vec<(String, String)>,
}
impl Fragment {
    pub fn rules(stage: Stage, rules: impl IntoIterator<Item = Rule>) -> Self {
        Self {
            rules: rules.into_iter().map(|r| (stage, r)).collect(),
            ..Default::default()
        }
    }
    pub fn listeners(listeners: impl IntoIterator<Item = Listener>) -> Self {
        Self {
            listeners: listeners.into_iter().collect(),
            ..Default::default()
        }
    }
}

/// The merged, validated configuration.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Config {
    pub proxies: Vec<Proxy>,
    pub groups: Vec<Group>,
    pub rules: Vec<Rule>,
    pub listeners: Vec<Listener>,
    pub dns: Option<Dns>,
    pub sniff: bool,
    pub rule_providers: Vec<RuleProvider>,
    pub tun: Option<Tun>,
}

impl Config {
    /// Merge fragments in plugin order. `owner` names the plugin for error messages.
    pub fn compose<'a>(
        fragments: impl IntoIterator<Item = (&'a str, Fragment)>,
    ) -> Result<Self, String> {
        let mut cfg = Config::default();
        let mut rules: Vec<(Stage, usize, Rule)> = Vec::new();
        let mut names = BTreeSet::new();
        let mut dns_owner: Option<&str> = None;
        for (order, (owner, f)) in fragments.into_iter().enumerate() {
            for name in f
                .proxies
                .iter()
                .map(|p| &p.name)
                .chain(f.groups.iter().map(|g| &g.name))
            {
                if reserved(name, "test") {
                    return Err(format!("{owner}: proxy or group name {name} is reserved"));
                }
            }
            for p in &f.proxies {
                if !names.insert(p.name.clone()) {
                    return Err(format!("{owner}: duplicate proxy or group name {}", p.name));
                }
            }
            for g in &f.groups {
                if !names.insert(g.name.clone()) {
                    return Err(format!("{owner}: duplicate proxy or group name {}", g.name));
                }
            }
            if let Some(dns) = f.dns {
                if let Some(other) = dns_owner {
                    return Err(format!("DNS is configured by both {other} and {owner}"));
                }
                dns_owner = Some(owner);
                cfg.dns = Some(dns);
            }
            cfg.sniff |= f.sniff;
            if let Some(t) = f.tun {
                if cfg.tun.is_some() {
                    return Err(format!(
                        "{owner}: the tunnel is already configured by another plugin"
                    ));
                }
                cfg.tun = Some(t);
            }
            for p in f.rule_providers {
                match cfg.rule_providers.iter().find(|o| o.name == p.name) {
                    Some(o) if *o == p => {}
                    Some(_) => {
                        return Err(format!("{owner}: rule set {} is defined twice", p.name));
                    }
                    None => cfg.rule_providers.push(p),
                }
            }
            cfg.proxies.extend(f.proxies);
            cfg.groups.extend(f.groups);
            for l in f.listeners {
                if reserved(&l.name, "checks") || l.name == ROUTED {
                    return Err(format!("{owner}: listener name {} is reserved", l.name));
                }
                if let Some(other) = cfg
                    .listeners
                    .iter()
                    .find(|o| o.port == l.port && overlaps(o.listen, l.listen))
                {
                    return Err(format!(
                        "{owner}: port {} is already used by listener {}",
                        l.port, other.name
                    ));
                }
                cfg.listeners.push(l);
            }
            for (stage, rule) in f.rules {
                if stage == Stage::System {
                    return Err(format!("{owner}: the system rule stage is reserved"));
                }
                rules.push((stage, order, rule));
            }
        }
        if rules.iter().filter(|(s, ..)| *s == Stage::Final).count() > 1 {
            return Err("More than one plugin adds a final rule".into());
        }
        rules.sort_by_key(|(stage, order, _)| (*stage, *order));
        cfg.rules = rules.into_iter().map(|(.., r)| r).collect();
        let known = |t: &Target| match t {
            Target::Proxy(n) | Target::Group(n) => names.contains(n),
            _ => true,
        };
        if let Some(r) = cfg.rules.iter().find(|r| !known(r.target())) {
            return Err(format!("A rule references an unknown target: {}", r.wire()));
        }
        if let Some(r) = cfg.rules.iter().find(|r| matches!(r, Rule::RuleSet { name, .. } if !cfg.rule_providers.iter().any(|p| p.name == *name))) {
            return Err(format!("A rule references an unknown rule set: {}", r.wire()));
        }
        for p in &cfg.proxies {
            if p.options.contains_key("dialer-proxy") {
                return Err(format!("Proxy {} sets dialer-proxy in its options", p.name));
            }
            if let Some(via) = &p.dialer
                && (via.wire() == p.name || !known(via))
            {
                return Err(format!(
                    "Proxy {} is dialed through an unknown proxy {}",
                    p.name,
                    via.wire()
                ));
            }
        }
        for g in &cfg.groups {
            if let Some(m) = g.members.iter().find(|m| !known(m)) {
                return Err(format!(
                    "Group {} references an unknown member {}",
                    g.name,
                    m.wire()
                ));
            }
        }
        Ok(cfg)
    }

    /// Everything except the current choice of select groups.
    pub fn same_structure(&self, other: &Config) -> bool {
        let strip = |c: &Config| {
            let mut c = c.clone();
            for g in &mut c.groups {
                g.selected = None;
            }
            c
        };
        strip(self) == strip(other)
    }
}

/// Names of the kernel's check lanes: `test`/`test-N` groups, `checks`/`checks-N` listeners.
fn reserved(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| {
        rest.is_empty()
            || rest
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

pub fn overlaps(a: IpAddr, b: IpAddr) -> bool {
    a == b || a.is_unspecified() || b.is_unspecified()
}

/// The kernel's own SOCKS listener that follows the ordinary rules, for the
/// app's downloads that should go where user traffic goes.
pub const ROUTED: &str = "routed";
/// Listeners of homeport itself (checks, its own downloads): their connections
/// are not the user's traffic.
pub fn internal_listener(name: &str) -> bool {
    name == ROUTED || name.starts_with("checks-")
}
/// Groups of homeport's check lanes (`test-i`, see [`System`]).
pub fn internal_group(name: &str) -> bool {
    name.strip_prefix("test-")
        .is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
}

/// Kernel-owned additions: isolated check lanes. Lane `i` is the SOCKS listener
/// `checks-i` on `check_ports[i]`, routed to the select group `test-i`.
pub struct System<'a> {
    pub controller: &'a Value,
    pub check_ports: &'a [u16],
    pub check_password: &'a str,
    /// Current target of every lane (same length as `check_ports`).
    pub check_targets: &'a [Option<Target>],
    /// Port of the [`ROUTED`] listener (same login as the lanes); `None` = no listener.
    pub routed_port: Option<u16>,
}

pub fn render(cfg: &Config, sys: &System<'_>, listeners: &[&Listener]) -> Value {
    let proxies: Vec<Value> = cfg
        .proxies
        .iter()
        .map(|p| {
            let mut v = Value::Object(p.options.clone());
            v["name"] = json!(p.name);
            v["type"] = json!(p.protocol);
            v["server"] = json!(p.server);
            v["port"] = json!(p.port);
            if let Some(via) = &p.dialer {
                v["dialer-proxy"] = json!(via.wire());
            }
            v
        })
        .collect();
    let group = |name: &str, kind: &GroupKind, members: &[Target], selected: &Option<Target>| {
        let mut choices = Vec::new();
        if *kind == GroupKind::Select {
            // The first member is mihomo's default after a reload; keep the selection there.
            choices.push(
                selected
                    .as_ref()
                    .map(Target::wire)
                    .unwrap_or_else(|| "REJECT".into()),
            );
            if selected.is_some() {
                choices.push("REJECT".into());
            }
        }
        // Tested groups keep their member order: a REJECT member would always look alive.
        for m in members {
            let w = m.wire();
            if !choices.contains(&w) {
                choices.push(w);
            }
        }
        if choices.is_empty() {
            choices.push("REJECT".into());
        }
        let mut v = json!({"name": name, "proxies": choices});
        match kind {
            GroupKind::Select => v["type"] = json!("select"),
            GroupKind::UrlTest { url, interval_secs } => {
                v["type"] = json!("url-test");
                v["url"] = json!(url);
                v["interval"] = json!(interval_secs);
            }
            GroupKind::Fallback { url, interval_secs } => {
                v["type"] = json!("fallback");
                v["url"] = json!(url);
                v["interval"] = json!(interval_secs);
                // Re-test members after two failed dials instead of five, and even while idle.
                v["max-failed-times"] = json!(2);
                v["timeout"] = json!(3000);
                v["lazy"] = json!(false);
            }
        }
        v
    };
    let mut groups: Vec<Value> = cfg
        .groups
        .iter()
        .map(|g| group(&g.name, &g.kind, &g.members, &g.selected))
        .collect();
    let all: Vec<Target> = cfg
        .proxies
        .iter()
        .map(|p| Target::Proxy(p.name.clone()))
        .collect();
    let mut ls = Vec::new();
    let mut rules = Vec::new();
    for (lane, port) in sys.check_ports.iter().enumerate() {
        let target = sys.check_targets.get(lane).cloned().flatten();
        groups.push(group(
            &format!("test-{lane}"),
            &GroupKind::Select,
            &all,
            &target,
        ));
        ls.push(json!({"name": format!("checks-{lane}"), "type": "socks", "listen": "127.0.0.1", "port": port,
            "users": [{"username": "check", "password": sys.check_password}]}));
        rules.push(format!("IN-NAME,checks-{lane},test-{lane}"));
    }
    if let Some(port) = sys.routed_port {
        ls.push(
            json!({"name": ROUTED, "type": "socks", "listen": "127.0.0.1", "port": port,
            "users": [{"username": "check", "password": sys.check_password}]}),
        );
    }
    for l in listeners {
        let kind = match &l.kind {
            ListenerKind::Mixed => "mixed",
            ListenerKind::Socks => "socks",
            ListenerKind::Http => "http",
            ListenerKind::Vless(v) => {
                ls.push(vless_listener(l, v));
                continue;
            }
        };
        let mut v = json!({"name": l.name, "type": kind, "listen": l.listen.to_string(), "port": l.port, "udp": l.udp});
        if !l.users.is_empty() {
            v["users"] = Value::Array(
                l.users
                    .iter()
                    .map(|(u, p)| json!({"username": u, "password": p}))
                    .collect(),
            );
        }
        ls.push(v);
    }
    rules.extend(cfg.rules.iter().map(Rule::wire));
    if !cfg.rules.iter().any(|r| matches!(r, Rule::Match(_))) {
        rules.push("MATCH,REJECT".into());
    }
    let dns = cfg.dns.as_ref().map_or_else(
        || json!({"enable": false}),
        |d| {
            let mut v = json!({"enable": d.enable, "nameserver": d.nameservers, "ipv6": true,
                // Server names in the addresses above resolve through the system.
                "default-nameserver": ["system"],
                "enhanced-mode": if d.fake_ip { "fake-ip" } else { "redir-host" }});
            if d.fake_ip {
                v["fake-ip-range"] = json!("198.18.0.1/16");
                v["fake-ip-filter"] = json!(FAKE_IP_FILTER);
            }
            v
        },
    );
    let hosts: serde_json::Map<String, Value> = cfg
        .dns
        .iter()
        .flat_map(|d| &d.hosts)
        .map(|(domain, ip)| (domain.clone(), json!(ip)))
        .collect();
    // Fake addresses survive a restart of the core: apps keep them in their caches.
    let store_fake_ip = cfg.dns.as_ref().is_some_and(|d| d.enable && d.fake_ip);
    // Listeners bound to a non-loopback address are meant for other machines.
    let lan = listeners.iter().any(|l| !l.listen.is_loopback());
    // Looking up the app of a connection costs time: only with app rules.
    let apps = cfg.rules.iter().any(|r| matches!(r, Rule::ProcessName(..)));
    let find_process = if apps { "strict" } else { "off" };
    let mut root = json!({
        "log-level": log_level(), "mode": "rule", "allow-lan": lan, "find-process-mode": find_process,
        "dns": dns,
        "profile": {"store-selected": false, "store-fake-ip": store_fake_ip},
        "proxies": proxies, "proxy-groups": groups, "listeners": ls, "rules": rules,
    });
    root.as_object_mut()
        .unwrap()
        .extend(sys.controller.as_object().unwrap().clone());
    if !cfg.rule_providers.is_empty() {
        let providers: serde_json::Map<String, Value> = cfg
            .rule_providers
            .iter()
            .map(|p| {
                let behavior = match p.behavior {
                    RuleBehavior::Domain => "domain",
                    RuleBehavior::IpCidr => "ipcidr",
                };
                let v = json!({"type": "http", "behavior": behavior, "format": "mrs", "url": p.url,
                    "path": format!("rules/{}.mrs", p.name), "interval": p.interval_secs, "proxy": "DIRECT"});
                (p.name.clone(), v)
            })
            .collect();
        root["rule-providers"] = Value::Object(providers);
    }
    if !hosts.is_empty() {
        root["hosts"] = Value::Object(hosts);
    }
    if let Some(t) = &cfg.tun {
        // Without a resolver of the core, DNS queries go out like other traffic.
        let hijack = if cfg.dns.as_ref().is_some_and(|d| d.enable) {
            json!(["any:53", "tcp://any:53"])
        } else {
            json!([])
        };
        root["tun"] = json!({
            "enable": true, "stack": "mixed", "auto-route": true, "auto-detect-interface": true,
            "strict-route": t.strict_route, "dns-hijack": hijack,
        });
    }
    if cfg.sniff {
        root["sniffer"] = json!({
            "enable": true, "parse-pure-ip": true,
            "sniff": {
                "TLS": {"ports": [443, 8443]},
                "HTTP": {"ports": [80, "8080-8880"], "override-destination": true},
                "QUIC": {"ports": [443, 8443]},
            },
        });
    }
    root
}

/// mihomo's log level: `silent` unless `HOMEPORT_MIHOMO_LOG` asks for more.
pub fn log_level() -> &'static str {
    match std::env::var("HOMEPORT_MIHOMO_LOG").as_deref() {
        Ok("debug") => "debug",
        Ok("info") => "info",
        Ok("warning") => "warning",
        Ok("error") => "error",
        _ => "silent",
    }
}

fn vless_listener(l: &Listener, v: &VlessListener) -> Value {
    let mut out = json!({
        "name": l.name, "type": "vless", "listen": l.listen.to_string(), "port": l.port,
        "users": v.users.iter().map(|(name, uuid)| json!({"username": name, "uuid": uuid})).collect::<Vec<_>>(),
        "reality-config": {
            "dest": v.reality.dest, "private-key": v.reality.private_key,
            "short-id": v.reality.short_ids, "server-names": v.reality.server_names,
            // The camouflage site is reached directly, never through the rules
            // (on a relay node they would send it via another server).
            "proxy": "DIRECT",
        },
    });
    if let Some(x) = &v.xhttp {
        out["xhttp-config"] = json!({
            "path": x.path, "mode": "auto",
            "x-padding-bytes": x.padding_bytes,
            "sc-stream-up-server-secs": x.stream_up_server_secs,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stages_order_rules_and_reject_conflicts() {
        let a = Fragment {
            groups: vec![Group {
                name: "main".into(),
                kind: GroupKind::Select,
                members: vec![],
                selected: None,
            }],
            rules: vec![(Stage::Final, Rule::Match(Target::Group("main".into())))],
            ..Default::default()
        };
        let b = Fragment::rules(
            Stage::User,
            [Rule::Domain("example.com".into(), Target::Direct)],
        );
        let cfg = Config::compose([("servers", a.clone()), ("routing", b)]).unwrap();
        assert_eq!(cfg.rules.len(), 2);
        assert!(matches!(cfg.rules[0], Rule::Domain(..)));
        assert!(Config::compose([("x", a.clone()), ("y", a)]).is_err());
        let l = Listener {
            name: "a".into(),
            kind: ListenerKind::Mixed,
            listen: "0.0.0.0".parse().unwrap(),
            port: 1,
            udp: true,
            users: vec![],
            optional: true,
        };
        let mut l2 = l.clone();
        l2.name = "b".into();
        l2.listen = "127.0.0.1".parse().unwrap();
        assert!(Config::compose([("x", Fragment::listeners([l, l2]))]).is_err());
    }

    fn proxy(name: &str) -> Proxy {
        Proxy {
            name: name.into(),
            protocol: "http".into(),
            server: "127.0.0.1".into(),
            port: 1,
            options: Map::new(),
            dialer: None,
        }
    }

    #[test]
    fn fallback_groups_keep_member_order_without_reject() {
        let members = vec![Target::Proxy("b".into()), Target::Proxy("a".into())];
        let f = Fragment {
            proxies: vec![proxy("a"), proxy("b")],
            groups: vec![Group {
                name: "main".into(),
                kind: GroupKind::Fallback {
                    url: "http://x/".into(),
                    interval_secs: 30,
                },
                members: members.clone(),
                selected: Some(Target::Proxy("a".into())),
            }],
            ..Default::default()
        };
        let cfg = Config::compose([("servers", f)]).unwrap();
        let sys = System {
            controller: &json!({"external-controller-unix": "/s"}),
            check_ports: &[],
            check_password: "p",
            check_targets: &[],
            routed_port: None,
        };
        let doc = render(&cfg, &sys, &[]);
        let main = &doc["proxy-groups"][0];
        assert_eq!(main["type"], "fallback");
        assert_eq!(main["proxies"], json!(["b", "a"]));
        // A select group still starts with the selection, then REJECT.
        let mut select = cfg.clone();
        select.groups[0].kind = GroupKind::Select;
        assert_eq!(
            render(&select, &sys, &[])["proxy-groups"][0]["proxies"],
            json!(["a", "REJECT", "b"])
        );
    }

    #[test]
    fn check_lanes_have_own_listener_group_and_rule() {
        let cfg = Config::compose([(
            "servers",
            Fragment {
                proxies: vec![proxy("a"), proxy("b")],
                ..Default::default()
            },
        )])
        .unwrap();
        let targets = [None, Some(Target::Proxy("b".into()))];
        let sys = System {
            controller: &json!({"external-controller-unix": "/s"}),
            check_ports: &[1001, 1002],
            check_password: "p",
            check_targets: &targets,
            routed_port: None,
        };
        let doc = render(&cfg, &sys, &[]);
        let groups = doc["proxy-groups"].as_array().unwrap();
        assert_eq!(groups[0]["name"], "test-0");
        assert_eq!(groups[0]["proxies"], json!(["REJECT", "a", "b"]));
        assert_eq!(groups[1]["proxies"], json!(["b", "REJECT", "a"]));
        assert_eq!(doc["listeners"][1]["name"], "checks-1");
        assert_eq!(doc["listeners"][1]["port"], 1002);
        assert_eq!(
            doc["rules"],
            json!([
                "IN-NAME,checks-0,test-0",
                "IN-NAME,checks-1,test-1",
                "MATCH,REJECT"
            ])
        );
        // Lane names are reserved; similar names are not.
        let group = |name: &str| Fragment {
            groups: vec![Group {
                name: name.into(),
                kind: GroupKind::Select,
                members: vec![],
                selected: None,
            }],
            ..Default::default()
        };
        for name in ["test", "test-3"] {
            assert!(Config::compose([("x", group(name))]).is_err(), "{name}");
        }
        assert!(Config::compose([("x", group("tests"))]).is_ok());
        let l = Listener {
            name: "checks-2".into(),
            kind: ListenerKind::Mixed,
            listen: "127.0.0.1".parse().unwrap(),
            port: 1,
            udp: true,
            users: vec![],
            optional: true,
        };
        assert!(Config::compose([("x", Fragment::listeners([l]))]).is_err());
    }

    #[test]
    fn the_tunnel_captures_dns_only_with_a_resolver() {
        let sys = System {
            controller: &json!({}),
            check_ports: &[],
            check_password: "p",
            check_targets: &[],
            routed_port: None,
        };
        let tun = Fragment {
            tun: Some(Tun {
                strict_route: false,
            }),
            ..Default::default()
        };
        let doc = render(&Config::compose([("tun", tun.clone())]).unwrap(), &sys, &[]);
        assert_eq!(doc["tun"]["dns-hijack"], json!([]));
        assert_eq!(doc["dns"]["enable"], false);
        let dns = Fragment {
            dns: Some(Dns {
                enable: true,
                nameservers: vec!["https://1.1.1.1/dns-query".into()],
                fake_ip: true,
                hosts: vec![("nas.home".into(), "192.168.1.5".into())],
            }),
            ..Default::default()
        };
        let cfg = Config::compose([("tun", tun), ("dns", dns)]).unwrap();
        let doc = render(&cfg, &sys, &[]);
        assert_eq!(doc["tun"]["dns-hijack"][0], "any:53");
        assert_eq!(doc["dns"]["enhanced-mode"], "fake-ip");
        assert_eq!(doc["hosts"]["nas.home"], "192.168.1.5");
        assert_eq!(doc["profile"]["store-fake-ip"], true);
    }
}
