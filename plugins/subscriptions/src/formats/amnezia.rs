//! Amnezia VPN keys: `vpn://` + Base64url of a Qt-compressed JSON profile
//! (4-byte big-endian length, then zlib). Its `containers` carry the
//! connections; AmneziaWG and WireGuard hold a WireGuard config, Xray and
//! Shadowsocks an Xray profile. OpenVPN, IKEv2 and Cloak have no mihomo client.
use super::{wireguard, xray};
use crate::{LINK, MAX_BYTES, Parse};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use homeport_kernel::prelude::*;
use plugin_servers::Proxy;
use serde_json::Value;
use std::io::Read;

const PREFIX: &str = "vpn://";

fn invalid(m: impl Into<String>) -> Error {
    Error::msg(m)
}

fn is_key(body: &str) -> bool {
    body.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with(PREFIX)
}

pub(crate) fn register(p: &mut Setup) {
    p.handle(|_: Cx, op: Parse| async move { parse(&op.body) })
        .claim(|_, op| is_key(&op.body).then_some(Claim::content(80)));
    p.hook(&LINK, |_, raw| is_key(raw).then(|| parse(raw).map(drop)));
}

/// Every connection of every key (one key per line); a key that cannot be
/// read is skipped (see [`super::usable`]).
pub fn parse(body: &str) -> Result<Vec<Proxy>> {
    super::usable((body.split_whitespace().enumerate()).map(|(i, key)| {
        (
            format!("Key {}", i + 1),
            decode(key).and_then(|p| connections(&p)),
        )
    }))
}

/// The JSON profile inside a key.
fn decode(key: &str) -> Result<Value> {
    let data = key
        .trim_start_matches('\u{feff}')
        .strip_prefix(PREFIX)
        .ok_or_else(|| invalid("Expected an Amnezia key (vpn://…)."))?
        .trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| invalid("Invalid Amnezia key."))?;
    // Qt's qCompress: the uncompressed size, then a zlib stream. Old keys may be plain JSON.
    let mut json = Vec::new();
    let compressed = bytes.get(4..).is_some_and(|z| {
        flate2::read::ZlibDecoder::new(z)
            .take(MAX_BYTES as u64)
            .read_to_end(&mut json)
            .is_ok()
    });
    serde_json::from_slice(if compressed { &json } else { &bytes })
        .map_err(|_| invalid("Invalid Amnezia key."))
}

fn connections(profile: &Value) -> Result<Vec<Proxy>> {
    let Some(containers) = profile["containers"].as_array() else {
        return Err(invalid(
            if profile.get("api_key").is_some() || profile.get("auth_data").is_some() {
                "Amnezia Premium and Free keys connect through Amnezia's service and are not supported."
            } else {
                "This Amnezia key has no connections."
            },
        ));
    };
    let name = ["description", "name", "hostName"]
        .iter()
        .find_map(|k| profile[*k].as_str().filter(|s| !s.trim().is_empty()))
        .unwrap_or("Amnezia");
    let mut found: Vec<(&str, Vec<Proxy>)> = Vec::new();
    for container in containers {
        for (key, kind) in [
            ("awg", "AmneziaWG"),
            ("wireguard", "WireGuard"),
            ("xray", "Xray"),
            ("ssxray", "Shadowsocks"),
        ] {
            let Some(last) = container[key]["last_config"].as_str() else {
                continue;
            };
            let last: Value =
                serde_json::from_str(last).map_err(|_| invalid("Invalid Amnezia key."))?;
            // Native config text, or (Xray) the profile itself.
            let native = last["config"].as_str();
            let proxies = if matches!(key, "awg" | "wireguard") {
                let config =
                    native.ok_or_else(|| invalid("The Amnezia key has no WireGuard config."))?;
                vec![wireguard_proxy(config, &last, name)?]
            } else {
                let doc = match native {
                    Some(text) => serde_json::from_str(text)
                        .map_err(|_| invalid("Invalid Xray profile in the Amnezia key."))?,
                    None => last,
                };
                let mut proxies = Vec::new();
                for (_, servers) in xray::profiles(&doc)? {
                    for (i, mut v) in servers?.into_iter().enumerate() {
                        v["name"] = name.into();
                        proxies.push(Proxy::from_value(v, i + 1)?);
                    }
                }
                proxies
            };
            found.push((kind, proxies));
        }
    }
    if found.is_empty() {
        return Err(invalid(
            "This Amnezia key has no AmneziaWG, WireGuard or Xray connection.",
        ));
    }
    let several = found.len() > 1;
    Ok(found
        .into_iter()
        .flat_map(|(kind, proxies)| {
            proxies.into_iter().map(move |mut p| {
                if several {
                    p.name = format!("{name} · {kind}");
                }
                p
            })
        })
        .collect())
}

/// The client connects to `hostName`/`port` of the key, whatever the config's
/// endpoint says, and uses its MTU when the config has none.
fn wireguard_proxy(config: &str, last: &Value, name: &str) -> Result<Proxy> {
    let mut p = wireguard::parse(config, Some(name))?;
    if let Some(host) = last["hostName"].as_str().filter(|h| !h.is_empty()) {
        p.server = host.to_owned();
    }
    if let Some(port) = number(&last["port"]).filter(|p| *p > 0 && *p <= u16::MAX as i64) {
        p.port = port as u16;
    }
    if !p.options.contains_key("mtu")
        && let Some(mtu) = number(&last["mtu"]).filter(|m| *m > 0)
    {
        p.options.insert("mtu".into(), mtu.into());
    }
    Ok(p)
}

/// Amnezia writes numbers as strings.
fn number(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    /// A key the way Amnezia builds it.
    pub(crate) fn key(profile: &Value) -> String {
        let json = profile.to_string();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(json.as_bytes()).unwrap();
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(z.finish().unwrap());
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    const CONFIG: &str = "[Interface]\nAddress = 10.8.1.2/32\nPrivateKey = a2V5\nJc = 3\nH1 = 1\n\n[Peer]\nPublicKey = cHVi\nEndpoint = 10.0.0.1:1\n";

    #[test]
    fn awg_and_xray_containers_become_servers() {
        let awg =
            json!({"config": CONFIG, "hostName": "vpn.example", "port": "51820", "mtu": "1376"});
        let xray = json!({"outbounds": [{"protocol": "vless", "settings": {"vnext": [{"address": "vpn.example", "port": 443, "users": [{"id": "9e78dff8-0445-4b12-8822-276f98d21ea5", "encryption": "none"}]}]}}]});
        let k = key(&json!({
            "description": "Office",
            "containers": [
                {"container": "amnezia-awg", "awg": {"last_config": awg.to_string(), "port": "51820"}},
                {"container": "amnezia-openvpn", "openvpn": {"last_config": "{}"}},
                {"container": "amnezia-xray", "xray": {"last_config": xray.to_string()}},
            ],
        }));
        let servers = parse(&k).unwrap();
        assert_eq!(servers.len(), 2);
        let wg = &servers[0];
        assert_eq!(wg.name, "Office · AmneziaWG");
        assert_eq!((wg.server.as_str(), wg.port), ("vpn.example", 51820));
        assert_eq!(wg.options["mtu"], 1376);
        assert_eq!(wg.options["amnezia-wg-option"]["jc"], 3);
        assert_eq!(servers[1].name, "Office · Xray");
    }

    #[test]
    fn service_keys_and_garbage_are_explained() {
        let premium = parse(&key(&json!({"api_key": "x"}))).unwrap_err();
        assert!(premium.message.contains("Premium"), "{}", premium.message);
        let openvpn = key(&json!({"containers": [{"openvpn": {"last_config": "{}"}}]}));
        assert!(parse(&openvpn).is_err());
        assert!(parse("vpn://not-base64!").is_err());
    }
}
