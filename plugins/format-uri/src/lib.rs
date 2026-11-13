//! # format-uri
//! Direct `vless://` links (one per line) and Base64-wrapped documents.
use plugin_servers::Proxy;
use plugin_subscriptions::{Parse, LINK};
use base64::{engine::general_purpose::{STANDARD, URL_SAFE}, Engine};
use percent_encoding::percent_decode_str;
use rproxy_kernel::prelude::*;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use url::Url;

pub static PLUGIN: Plugin = Plugin {
    id: "format-uri",
    name: "Links and Base64",
    description: "vless:// links, one per line, and Base64-encoded subscription bodies.",
    category: categories::FORMATS,
    requires: &["subscriptions"],
    required: false,
    default_enabled: true,
    setup,
};

fn invalid(m: impl Into<String>) -> Error {
    Error::msg(m)
}

fn plain(body: &str) -> &str {
    body.trim().trim_start_matches('\u{feff}').trim()
}
fn looks_base64(body: &str) -> bool {
    let b = plain(body);
    !b.is_empty() && !b.starts_with("vless://") && !b.contains([':', '{', '['])
}

/// Decode a Base64 (standard or URL-safe, padding optional) document.
pub fn decode_base64(body: &str) -> Result<String> {
    let mut compact: String = plain(body).split_whitespace().collect();
    while !compact.len().is_multiple_of(4) {
        compact.push('=');
    }
    let bytes = STANDARD.decode(&compact).or_else(|_| URL_SAFE.decode(&compact)).map_err(|_| invalid("Unrecognized subscription format."))?;
    String::from_utf8(bytes).map_err(|_| invalid("Subscription is not UTF-8."))
}

/// Parse `vless://` lines into proxies.
pub fn parse_links(body: &str) -> Result<Vec<Proxy>> {
    plain(body)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, l)| Proxy::from_value(link(l)?, i + 1))
        .collect()
}

fn setup(p: &mut Setup) -> Result<()> {
    p.handle(|cx: Cx, op: Parse| async move {
        let decoded = decode_base64(&op.body)?;
        if plain(&decoded).is_empty() {
            return Ok(Vec::new());
        }
        // Re-dispatch: the decoded text is claimed by whichever format knows it.
        cx.call(Parse { source: op.source, body: decoded }).await
    })
    .claim(|_, op| looks_base64(&op.body).then_some(Claim::content(90)));
    p.handle(|_: Cx, op: Parse| async move { parse_links(&op.body) })
        .claim(|_, op| plain(&op.body).starts_with("vless://").then_some(Claim::content(80)));
    p.hook(&LINK, |_, raw| raw.starts_with("vless://").then(|| link(raw).map(drop).map_err(|e| e.message)));
    Ok(())
}

fn link(raw: &str) -> Result<Value> {
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
        Some(raw) => serde_json::from_str(raw).map_err(|_| invalid("Invalid XHTTP extra settings."))?,
        None => Map::new(),
    };
    let range = |v: &Value| match v {
        Value::String(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '-') => Some(json!(s)),
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
        if x.get("maxConnections").and_then(range).is_some_and(|v| v != "0") {
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
