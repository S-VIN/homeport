//! WireGuard and AmneziaWG configurations (`[Interface]` … `[Peer]`), as the
//! AmneziaWG and WireGuard apps export them. A config pasted into the one-line
//! subscription field arrives with its lines joined by spaces; both forms parse.
use crate::{LINK, Parse};
use homeport_kernel::prelude::*;
use plugin_servers::Proxy;
use serde_json::{Map, Value, json};
use std::net::IpAddr;

fn invalid(m: impl Into<String>) -> Error {
    Error::msg(m)
}

/// The document looks like a WireGuard config.
pub fn is_config(body: &str) -> bool {
    let b = body.trim_start_matches('\u{feff}').trim_start();
    b.starts_with("[Interface]") || (b.starts_with('#') && b.contains("[Interface]"))
}

pub(crate) fn register(p: &mut Setup) {
    p.handle(|_: Cx, op: Parse| async move { Ok(vec![parse(&op.body, None)?]) })
        .claim(|_, op| is_config(&op.body).then_some(Claim::content(85)));
    p.hook(&LINK, |_, raw| {
        is_config(raw).then(|| parse(raw, None).map(drop))
    });
}

/// Keys the config may have. In a joined config only these start a new line.
const KEYS: &[&str] = &[
    "PrivateKey",
    "Address",
    "DNS",
    "MTU",
    "ListenPort",
    "Table",
    "FwMark",
    "PreUp",
    "PostUp",
    "PreDown",
    "PostDown",
    "SaveConfig",
    "Jc",
    "Jmin",
    "Jmax",
    "S1",
    "S2",
    "S3",
    "S4",
    "H1",
    "H2",
    "H3",
    "H4",
    "I1",
    "I2",
    "I3",
    "I4",
    "I5",
    "J1",
    "J2",
    "J3",
    "Itime",
    "HeaderProtectionKey",
    "ContentPaddingAddition",
    "RekeyAfterTime",
    "RekeyTimeout",
    "RejectAfterTime",
    "KeepaliveTimeout",
    "MaxHandshakeAttempts",
    "RandomTrailers",
    "DisableCookies",
    "PublicKey",
    "PresharedKey",
    "AllowedIPs",
    "Endpoint",
    "PersistentKeepalive",
];
/// AmneziaWG options: config key, mihomo key, kind.
const AMNEZIA: &[(&str, &str, Kind)] = &[
    ("Jc", "jc", Kind::Int),
    ("Jmin", "jmin", Kind::Int),
    ("Jmax", "jmax", Kind::Int),
    ("S1", "s1", Kind::Int),
    ("S2", "s2", Kind::Int),
    ("S3", "s3", Kind::Int),
    ("S4", "s4", Kind::Int),
    ("H1", "h1", Kind::Text),
    ("H2", "h2", Kind::Text),
    ("H3", "h3", Kind::Text),
    ("H4", "h4", Kind::Text),
    ("I1", "i1", Kind::Text),
    ("I2", "i2", Kind::Text),
    ("I3", "i3", Kind::Text),
    ("I4", "i4", Kind::Text),
    ("I5", "i5", Kind::Text),
    ("J1", "j1", Kind::Text),
    ("J2", "j2", Kind::Text),
    ("J3", "j3", Kind::Text),
    ("Itime", "itime", Kind::Int),
    ("HeaderProtectionKey", "header-protection-key", Kind::V3),
    (
        "ContentPaddingAddition",
        "content-padding-addition",
        Kind::V3,
    ),
    ("RekeyAfterTime", "rekey-after-time", Kind::V3),
    ("RekeyTimeout", "rekey-timeout", Kind::V3),
    ("RejectAfterTime", "reject-after-time", Kind::V3),
    ("KeepaliveTimeout", "keepalive-timeout", Kind::V3),
    ("MaxHandshakeAttempts", "max-handshake-attempts", Kind::V3),
    ("RandomTrailers", "random-trailers", Kind::Switch),
    ("DisableCookies", "disable-cookies", Kind::Switch),
];
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Int,
    Text,
    /// AmneziaWG 3 text option.
    V3,
    /// AmneziaWG 3 `on`/`off`.
    Switch,
}

/// Restore the lines of a config whose line breaks became spaces.
fn split_lines(body: &str) -> String {
    if body.trim().contains('\n') {
        return body.to_owned();
    }
    let lower = body.to_ascii_lowercase();
    let mut out = String::with_capacity(body.len() + 64);
    let mut last = 0;
    for (i, _) in body.char_indices() {
        let at_word = i == 0 || body[..i].ends_with(char::is_whitespace);
        if !at_word {
            continue;
        }
        let rest = &lower[i..];
        let starts = rest.starts_with("[interface]")
            || rest.starts_with("[peer]")
            || KEYS.iter().any(|k| {
                rest.starts_with(&k.to_ascii_lowercase())
                    && rest[k.len()..].trim_start().starts_with('=')
            });
        if starts && i > 0 {
            out.push_str(&body[last..i]);
            out.push('\n');
            last = i;
        }
    }
    out.push_str(&body[last..]);
    out
}

/// One WireGuard outbound. `name` overrides the endpoint host as its name.
pub fn parse(body: &str, name: Option<&str>) -> Result<Proxy> {
    let mut interface = Map::new();
    let mut peer: Option<Map<String, Value>> = None;
    let mut section = "";
    for line in split_lines(body.trim_start_matches('\u{feff}')).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            section = match line.to_ascii_lowercase().as_str() {
                "[interface]" => "interface",
                // Only the first peer is used: one server is one endpoint.
                "[peer]" if peer.is_none() => {
                    peer = Some(Map::new());
                    "peer"
                }
                "[peer]" => "",
                _ => return Err(invalid(format!("Unknown WireGuard section {line}."))),
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(invalid("Invalid WireGuard config line."));
        };
        let (key, value) = (key.trim(), value.trim());
        let Some(key) = KEYS.iter().find(|k| k.eq_ignore_ascii_case(key)) else {
            continue;
        };
        let target = match section {
            "interface" => &mut interface,
            "peer" => peer.as_mut().expect("peer section"),
            _ => continue,
        };
        if !value.is_empty() {
            target.insert((*key).into(), json!(value));
        }
    }
    let peer = peer.ok_or_else(|| invalid("The WireGuard config has no [Peer]."))?;
    let text =
        |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
    let required = |m: &Map<String, Value>, k: &str| {
        text(m, k).ok_or_else(|| invalid(format!("The WireGuard config has no {k}.")))
    };
    let endpoint = required(&peer, "Endpoint")?;
    let (host, port) = endpoint
        .rsplit_once(':')
        .and_then(|(h, p)| Some((h.trim_matches(['[', ']']), p.parse::<u16>().ok()?)))
        .filter(|(h, p)| !h.is_empty() && *p > 0)
        .ok_or_else(|| invalid("Invalid WireGuard endpoint."))?;
    let mut p = json!({
        "name": name.filter(|n| !n.trim().is_empty()).unwrap_or(host),
        "type": "wireguard",
        "server": host,
        "port": port,
        "private-key": required(&interface, "PrivateKey")?,
        "public-key": required(&peer, "PublicKey")?,
        "udp": true,
    });
    for address in required(&interface, "Address")?.split(',') {
        let ip = address.trim().split('/').next().unwrap_or_default();
        match ip.parse::<IpAddr>() {
            Ok(IpAddr::V4(_)) if p.get("ip").is_none() => p["ip"] = json!(ip),
            Ok(IpAddr::V6(_)) if p.get("ipv6").is_none() => p["ipv6"] = json!(ip),
            Ok(_) => {}
            Err(_) => return Err(invalid("Invalid WireGuard address.")),
        }
    }
    if let Some(v) = text(&peer, "PresharedKey") {
        p["pre-shared-key"] = json!(v);
    }
    if let Some(v) = text(&peer, "AllowedIPs") {
        p["allowed-ips"] = json!(
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        );
    }
    for (from, to, map) in [
        ("MTU", "mtu", &interface),
        ("PersistentKeepalive", "persistent-keepalive", &peer),
    ] {
        if let Some(v) = text(map, from) {
            p[to] = json!(int(from, &v)?);
        }
    }
    let mut amnezia = Map::new();
    for &(from, to, kind) in AMNEZIA {
        let Some(v) = text(&interface, from) else {
            continue;
        };
        amnezia.insert(
            to.into(),
            match kind {
                Kind::Int => json!(int(from, &v)?),
                Kind::Text | Kind::V3 => json!(v),
                Kind::Switch => match v.to_ascii_lowercase().as_str() {
                    "on" | "true" | "1" => json!(true),
                    "off" | "false" | "0" => json!(false),
                    _ => return Err(invalid(format!("Invalid AmneziaWG {from}."))),
                },
            },
        );
    }
    let v3 = AMNEZIA.iter().any(|(_, to, kind)| {
        matches!(kind, Kind::V3 | Kind::Switch)
            && amnezia.get(*to).is_some_and(|v| v != &json!(false))
    });
    if v3 {
        amnezia.insert("version".into(), json!(3));
    }
    if !amnezia.is_empty() {
        p["amnezia-wg-option"] = Value::Object(amnezia);
    }
    Proxy::from_value(p, 1)
}

fn int(key: &str, v: &str) -> Result<i64> {
    v.parse()
        .map_err(|_| invalid(format!("Invalid WireGuard {key}: {v}.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    const AWG: &str = "[Interface]
Address = 10.8.1.2/32, fd00::2/128
DNS = $PRIMARY_DNS, $SECONDARY_DNS
PrivateKey = cHJpdmF0ZWtleXByaXZhdGVrZXlwcml2YXRla2V5cHI=
Jc = 4
Jmin = 10
Jmax = 50
S1 = 64
S2 = 17
H1 = 100000-100010
H2 = 200000
H3 = 300000
H4 = 400000
I1 = <b 0xc0ff><r 16>
I2 =

[Peer]
PublicKey = cHVibGlja2V5cHVibGlja2V5cHVibGlja2V5cHVibGk=
PresharedKey = cHNrcHNrcHNrcHNrcHNrcHNrcHNrcHNrcHNrcHNrcHM=
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = 203.0.113.7:41820
PersistentKeepalive = 25
";

    #[test]
    fn amneziawg_config_becomes_a_wireguard_outbound() {
        let p = parse(AWG, Some("Home")).unwrap();
        assert_eq!(p.name, "Home");
        assert_eq!((p.server.as_str(), p.port), ("203.0.113.7", 41820));
        assert_eq!(p.options["ip"], "10.8.1.2");
        assert_eq!(p.options["ipv6"], "fd00::2");
        assert_eq!(p.options["allowed-ips"], json!(["0.0.0.0/0", "::/0"]));
        assert_eq!(p.options["persistent-keepalive"], 25);
        let awg = &p.options["amnezia-wg-option"];
        assert_eq!(awg["jc"], 4);
        assert_eq!(awg["h1"], "100000-100010");
        assert_eq!(awg["i1"], "<b 0xc0ff><r 16>");
        assert!(awg.get("i2").is_none() && awg.get("version").is_none());
    }

    #[test]
    fn a_config_pasted_into_one_line_parses_the_same() {
        let joined = AWG.replace('\n', " ");
        assert!(is_config(&joined));
        assert_eq!(parse(&joined, None).unwrap(), parse(AWG, None).unwrap());
    }

    #[test]
    fn plain_wireguard_has_no_amnezia_options_and_v3_sets_the_version() {
        let plain: String = AWG
            .lines()
            .filter(|l| !l.starts_with(['J', 'S', 'H', 'I']))
            .map(|l| format!("{l}\n"))
            .collect();
        let p = parse(&plain, None).unwrap();
        assert_eq!(p.name, "203.0.113.7");
        assert!(p.options.get("amnezia-wg-option").is_none());
        let v3 = AWG.replace(
            "Jc = 4",
            "Jc = 4\nHeaderProtectionKey = a2V5\nRandomTrailers = on",
        );
        let awg = &parse(&v3, None).unwrap().options["amnezia-wg-option"];
        assert_eq!(awg["version"], 3);
        assert_eq!(awg["random-trailers"], true);
        assert!(parse(&AWG.replace("Endpoint", "# Endpoint"), None).is_err());
    }
}
