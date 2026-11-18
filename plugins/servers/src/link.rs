//! Share links (`vless://`, `hysteria2://`): one server each.
use crate::Proxy;
use homeport_kernel::{Error, Result};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use url::Url;

fn invalid(m: impl Into<String>) -> Error {
    Error::msg(m)
}

/// A link of a protocol [`Proxy::from_link`] reads.
pub fn is_link(raw: &str) -> bool {
    ["vless://", "hysteria2://", "hy2://"]
        .iter()
        .any(|prefix| raw.starts_with(prefix))
}

impl Proxy {
    /// The server of a share link (see [`is_link`]).
    pub fn from_link(raw: &str) -> Result<Self> {
        Self::from_value(link(raw.trim())?, 1)
    }
}

fn link(raw: &str) -> Result<Value> {
    if raw.starts_with("hysteria2://") || raw.starts_with("hy2://") {
        return hysteria2(raw);
    }
    let u = Url::parse(raw).map_err(|_| invalid("Invalid VLESS link."))?;
    if u.scheme() != "vless" || u.password().is_some() {
        return Err(invalid("Expected a VLESS link."));
    }
    let host = u
        .host_str()
        .ok_or_else(|| invalid("Missing VLESS host."))?
        .trim_matches(['[', ']']);
    let port = u
        .port()
        .filter(|p| *p > 0)
        .ok_or_else(|| invalid("Missing VLESS port."))?;
    let username = percent_decode_str(u.username())
        .decode_utf8()
        .map_err(|_| invalid("Invalid VLESS UUID."))?;
    let uuid = uuid::Uuid::parse_str(&username).map_err(|_| invalid("Invalid VLESS UUID."))?;
    let name = percent_decode_str(u.fragment().filter(|s| !s.is_empty()).unwrap_or(host))
        .decode_utf8_lossy();
    let mut p =
        json!({"name":name,"type":"vless","server":host,"port":port,"uuid":uuid,"udp":true});
    let mut seen = HashSet::new();
    let mut q = Map::new();
    for (k, v) in u.query_pairs() {
        if !seen.insert(k.to_string()) {
            return Err(invalid("Duplicate VLESS parameter."));
        }
        q.insert(k.into_owned(), Value::String(v.into_owned()));
    }
    let network = q.get("type").and_then(Value::as_str).unwrap_or("tcp");
    let network = if network == "raw" { "tcp" } else { network };
    p["network"] = json!(network);
    let sec = q.get("security").and_then(Value::as_str).unwrap_or("none");
    if !matches!(sec, "tls" | "reality" | "none") {
        return Err(invalid("Unsupported VLESS security."));
    }
    p["tls"] = json!(sec != "none");
    for (k, to) in [
        ("sni", "servername"),
        ("flow", "flow"),
        ("fp", "client-fingerprint"),
        ("encryption", "encryption"),
    ] {
        if let Some(v) = q.get(k) {
            p[to] = v.clone();
        }
    }
    if p["flow"] == "xtls-rprx-vision-udp443" {
        return Err(invalid("Unsupported VLESS flow."));
    }
    if let Some(v) = q.get("allowInsecure") {
        p["skip-cert-verify"] = json!(boolean(v)?);
    }
    if let Some(v) = q.get("alpn").and_then(Value::as_str) {
        p["alpn"] = json!(v.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>());
    }
    if sec == "reality" {
        p["reality-opts"] = json!({"public-key":q.get("pbk").and_then(Value::as_str).filter(|s|!s.is_empty()).ok_or_else(||invalid("Reality needs a public key."))?,"short-id":q.get("sid").and_then(Value::as_str).unwrap_or("")});
    }
    match network {
        "tcp" => {}
        "ws" => {
            let mut opts = json!({});
            if let Some(v) = q.get("path") {
                opts["path"] = v.clone();
            }
            if let Some(v) = q.get("host") {
                opts["headers"] = json!({"Host":v});
            }
            p["ws-opts"] = opts;
        }
        "grpc" => {
            if q.get("mode").is_some_and(|m| m != "gun") {
                return Err(invalid("Unsupported gRPC mode."));
            }
            p["grpc-opts"] = json!({"grpc-service-name":q.get("serviceName").and_then(Value::as_str).unwrap_or("")});
        }
        "xhttp" => p["xhttp-opts"] = xhttp_opts(&q)?,
        _ => return Err(invalid("Unsupported VLESS transport.")),
    }
    Ok(p)
}

fn hysteria2(raw: &str) -> Result<Value> {
    let u = Url::parse(raw).map_err(|_| invalid("Invalid Hysteria 2 link."))?;
    let host = u
        .host_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("Missing Hysteria 2 host."))?
        .trim_matches(['[', ']']);
    let port = u.port().unwrap_or(443);
    if port == 0 {
        return Err(invalid("Invalid Hysteria 2 port."));
    }
    let decode = |value: &str| {
        percent_decode_str(value)
            .decode_utf8()
            .map(|s| s.into_owned())
            .map_err(|_| invalid("Invalid UTF-8 in Hysteria 2 link."))
    };
    let mut password = decode(u.username())?;
    if let Some(p) = u.password() {
        password.push(':');
        password.push_str(&decode(p)?);
    }
    let name = decode(u.fragment().filter(|s| !s.is_empty()).unwrap_or(host))?;
    let mut proxy =
        json!({"name":name,"type":"hysteria2","server":host,"port":port,"password":password});
    let mut mask = None;
    let mut seen = HashSet::new();
    for (key, value) in u.query_pairs() {
        if !seen.insert(key.to_string()) {
            return Err(invalid("Duplicate Hysteria 2 parameter."));
        }
        match key.as_ref() {
            "fm" => {
                mask = Some(
                    serde_json::from_str::<Value>(&value)
                        .map_err(|_| invalid("Invalid Hysteria 2 final mask."))?,
                )
            }
            "sni" | "obfs-password" => proxy[&*key] = json!(value),
            "obfs" => {
                if !matches!(value.as_ref(), "salamander" | "gecko") {
                    return Err(invalid("Unsupported Hysteria 2 obfuscation."));
                }
                proxy["obfs"] = json!(value);
            }
            "insecure" | "allowInsecure" => {
                proxy["skip-cert-verify"] = json!(boolean(&json!(value))?)
            }
            "pinSHA256" => proxy["fingerprint"] = json!(value),
            "alpn" => proxy["alpn"] = json!(value.split(',').collect::<Vec<_>>()),
            "ech" => proxy["ech-opts"] = json!({"enable":true,"config":value}),
            // Newer client options: the server works without them.
            _ => homeport_kernel::log::debug!("Ignored Hysteria 2 parameter {key}"),
        }
    }
    if let Some(mask) = mask {
        let object = mask
            .as_object()
            .ok_or_else(|| invalid("Invalid Hysteria 2 final mask."))?;
        if object.keys().any(|k| k != "udp") {
            return Err(invalid("Unsupported Hysteria 2 final mask options."));
        }
        let udp = mask["udp"]
            .as_array()
            .ok_or_else(|| invalid("Invalid Hysteria 2 UDP mask."))?;
        if udp.len() != 1 || udp[0]["type"] != "salamander" {
            return Err(invalid(
                "This Hysteria 2 final mask is not supported by the core.",
            ));
        }
        let settings = udp[0]["settings"]
            .as_object()
            .ok_or_else(|| invalid("Invalid Salamander settings."))?;
        if settings.keys().any(|k| k != "password") {
            return Err(invalid("Unsupported Salamander settings."));
        }
        let secret = settings
            .get("password")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Missing Salamander password."))?;
        if proxy.get("obfs").is_some_and(|v| v != "salamander")
            || proxy.get("obfs-password").is_some_and(|v| v != secret)
        {
            return Err(invalid("Conflicting Hysteria 2 obfuscation settings."));
        }
        proxy["obfs"] = json!("salamander");
        proxy["obfs-password"] = json!(secret);
    }
    if proxy.get("obfs").is_some() && proxy["obfs-password"].as_str().is_none_or(str::is_empty) {
        return Err(invalid("Hysteria 2 obfuscation needs a password."));
    }
    Ok(proxy)
}
/// XHTTP client options. Connection reuse (XMUX) is always explicit and ranged:
/// core defaults change between versions and a burst of parallel handshakes to
/// one SNI is a known DPI signal. Xray's `extra` JSON may override the ranges.
fn xhttp_opts(q: &Map<String, Value>) -> Result<Value> {
    let mut opts = json!({});
    for k in ["host", "path", "mode"] {
        if let Some(v) = q.get(k) {
            opts[k] = v.clone();
        }
    }
    let extra: Map<String, Value> = match q.get("extra").and_then(Value::as_str) {
        Some(raw) => {
            serde_json::from_str(raw).map_err(|_| invalid("Invalid XHTTP extra settings."))?
        }
        None => Map::new(),
    };
    let range = |v: &Value| match v {
        Value::String(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-') => {
            Some(json!(s))
        }
        Value::Number(n) => Some(json!(n.to_string())),
        _ => None,
    };
    if let Some(v) = extra.get("xPaddingBytes").and_then(range) {
        opts["x-padding-bytes"] = v;
    }
    let mut reuse = json!({"max-concurrency": "16-32", "h-max-request-times": "600-900", "h-max-reusable-secs": "1800-3000"});
    if let Some(Value::Object(x)) = extra.get("xmux") {
        for (from, to) in [
            ("maxConcurrency", "max-concurrency"),
            ("maxConnections", "max-connections"),
            ("cMaxReuseTimes", "c-max-reuse-times"),
            ("hMaxRequestTimes", "h-max-request-times"),
            ("hMaxReusableSecs", "h-max-reusable-secs"),
        ] {
            if let Some(v) = x.get(from).and_then(range) {
                reuse[to] = v;
            }
        }
        // mihomo rejects both limits at once; an explicit connection count wins.
        if x.get("maxConnections")
            .and_then(range)
            .is_some_and(|v| v != "0")
        {
            reuse.as_object_mut().map(|m| m.remove("max-concurrency"));
        }
    }
    opts["reuse-settings"] = reuse;
    Ok(opts)
}

fn boolean(v: &Value) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        v if v == "true" || v == "1" || v == &json!(1) => Ok(true),
        v if v == "false" || v == "0" || v == &json!(0) => Ok(false),
        _ => Err(invalid("Invalid boolean in subscription.")),
    }
}
