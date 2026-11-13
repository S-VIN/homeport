//! # format-xray
//! Xray client configurations (JSON profiles with `outbounds`): VLESS, Shadowsocks, Hysteria 2.
use plugin_servers::Proxy;
use plugin_subscriptions::Parse;
use rproxy_kernel::prelude::*;
use serde_json::{json, Map, Value};

pub static PLUGIN: Plugin = Plugin {
    id: "format-xray",
    name: "Xray JSON",
    description: "Xray client profiles: VLESS, Shadowsocks and Hysteria 2 outbounds with their transports.",
    category: categories::FORMATS,
    requires: &["subscriptions"],
    required: false,
    default_enabled: true,
    setup,
};

fn setup(p: &mut Setup) -> Result<()> {
    p.handle(|_: Cx, op: Parse| async move { parse(&op.body) })
        .claim(|_, op| {
            let b = op.body.trim_start_matches('\u{feff}').trim_start();
            ((b.starts_with('{') || b.starts_with('[')) && (b.contains("outbounds") || b.contains("\"protocol\""))).then_some(Claim::content(70))
        });
    Ok(())
}

/// Parse an Xray document (one profile or a list of profiles).
pub fn parse(body: &str) -> Result<Vec<Proxy>> {
    let doc: Value = serde_json::from_str(body.trim_start_matches('\u{feff}')).map_err(|_| invalid("Invalid Xray JSON."))?;
    profiles(&doc)?.into_iter().enumerate().map(|(i, v)| Proxy::from_value(v, i + 1)).collect()
}

fn invalid(m: impl Into<String>) -> Error {
    Error::msg(m)
}
fn boolean(v: &Value) -> Result<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        v if v == "true" || v == "1" || v == &json!(1) => Ok(true),
        v if v == "false" || v == "0" || v == &json!(0) => Ok(false),
        _ => Err(invalid("Invalid boolean in subscription.")),
    }
}
type Object = Map<String, Value>;

fn object(v: &Value) -> Result<Object> {
    if v.is_null() {
        return Ok(Object::new());
    }
    v.as_object()
        .cloned()
        .ok_or_else(|| invalid("Expected an Xray settings object."))
}
fn array<'a>(v: &'a Value, field: &str) -> Result<&'a [Value]> {
    v[field]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(format!("Missing Xray {field} list.")))
}
fn transfer(src: &mut Object, dst: &mut Value, mapping: &[(&str, &str)]) {
    for &(from, to) in mapping {
        if let Some(v) = src.remove(from) {
            dst[to] = v;
        }
    }
}
fn consumed(src: &Object) -> Result<()> {
    if src.is_empty() {
        Ok(())
    } else {
        Err(invalid(
            "Unsupported Xray extension. The last working subscription was kept.",
        ))
    }
}
fn protocol_options(value: &Value, supported: &[&str]) -> Result<()> {
    let options = value
        .as_object()
        .ok_or_else(|| invalid("Expected an Xray settings object."))?;
    if options.keys().any(|key| !supported.contains(&key.as_str())) {
        return Err(invalid(
            "Unsupported Xray protocol extension. The last working subscription was kept.",
        ));
    }
    Ok(())
}
pub fn profiles(doc: &Value) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for profile in doc
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(std::slice::from_ref(doc))
    {
        let outbounds = if profile.get("outbounds").is_some() {
            array(profile, "outbounds")?
        } else {
            std::slice::from_ref(profile)
        };
        for out in outbounds {
            let cfg = &out["settings"];
            let stream = &out["streamSettings"];
            let mut add = |endpoint: &Value, mut p: Value| -> Result<()> {
                p["name"] = profile["remarks"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| out["tag"].as_str().filter(|s| !s.is_empty()))
                    .map(|s| json!(s))
                    .unwrap_or_else(|| endpoint["address"].clone());
                p["server"] = endpoint["address"].clone();
                p["port"] = endpoint["port"].clone();
                p["udp"] = json!(true);
                transport(&mut p, stream)?;
                result.push(p);
                Ok(())
            };
            match out["protocol"].as_str() {
                Some("freedom" | "dns" | "loopback" | "blackhole") => {}
                Some("vless") => {
                    for endpoint in array(cfg, "vnext")? {
                        for user in array(endpoint, "users")? {
                            let uuid = user["id"]
                                .as_str()
                                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                                .ok_or_else(|| invalid("Invalid VLESS UUID."))?;
                            let mut p = json!({"type":"vless","uuid":uuid});
                            let mut fields = object(user)?;
                            transfer(
                                &mut fields,
                                &mut p,
                                &[("flow", "flow"), ("encryption", "encryption")],
                            );
                            if p["flow"] == "xtls-rprx-vision-udp443" {
                                return Err(invalid("Unsupported VLESS flow."));
                            }
                            for k in ["id", "email", "level"] {
                                fields.remove(k);
                            }
                            consumed(&fields)?;
                            add(endpoint, p)?;
                        }
                    }
                }
                Some("shadowsocks") => {
                    for ep in cfg["servers"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or(std::slice::from_ref(cfg))
                    {
                        let cipher = ep["method"]
                            .as_str()
                            .ok_or_else(|| invalid("Missing Shadowsocks cipher."))?
                            .to_lowercase();
                        let cipher = match cipher.as_str() {
                            "plain" => "none",
                            "chacha20-poly1305" | "aead_chacha20_poly1305" => {
                                "chacha20-ietf-poly1305"
                            }
                            "xchacha20-poly1305" | "aead_xchacha20_poly1305" => {
                                "xchacha20-ietf-poly1305"
                            }
                            "aead_aes_128_gcm" => "aes-128-gcm",
                            "aead_aes_256_gcm" => "aes-256-gcm",
                            s => s,
                        };
                        let mut p = json!({"type":"ss","cipher":cipher,"password":ep["password"].as_str().ok_or_else(||invalid("Missing Shadowsocks password."))?});
                        if let Some(v) = ep.get("uot") {
                            p["udp-over-tcp"] = json!(boolean(v)?);
                        }
                        if let Some(v) = ep.get("uotVersion").or(ep.get("UoTVersion")) {
                            p["udp-over-tcp-version"] = v.clone();
                        }
                        protocol_options(
                            ep,
                            &[
                                "address",
                                "port",
                                "method",
                                "password",
                                "uot",
                                "uotVersion",
                                "UoTVersion",
                                "email",
                                "level",
                            ],
                        )?;
                        add(ep, p)?;
                    }
                }
                Some("hysteria")
                    if cfg["version"] == 2
                        && stream["hysteriaSettings"]
                            .get("version")
                            .is_none_or(|v| v == 2) =>
                {
                    protocol_options(cfg, &["address", "port", "version", "email", "level"])?;
                    let password = stream["hysteriaSettings"]["auth"]
                        .as_str()
                        .ok_or_else(|| invalid("Missing Hysteria 2 password."))?;
                    add(cfg, json!({"type":"hysteria2","password":password}))?;
                }
                _ => return Err(invalid("Unsupported Xray outbound protocol.")),
            }
        }
    }
    Ok(result)
}
fn transport(p: &mut Value, stream: &Value) -> Result<()> {
    let mut fields = object(stream)?;
    let net = fields.remove("network").unwrap_or(json!("tcp"));
    let net = net
        .as_str()
        .ok_or_else(|| invalid("Invalid Xray network."))?;
    let net = if net == "raw" { "tcp" } else { net };
    let security = fields.remove("security").unwrap_or(json!("none"));
    if p["type"] == "ss" {
        if net != "tcp" || security != "none" {
            return Err(invalid("Unsupported Shadowsocks transport."));
        }
        // Xray exporters often include an empty TCP settings block for plain SS.
        for name in ["tcpSettings", "rawSettings"] {
            if let Some(raw) = fields.remove(name) {
                let opts = object(&raw)?;
                if !opts.is_empty() && raw != json!({"header": {"type": "none"}}) {
                    return Err(invalid("Unsupported Shadowsocks TCP header."));
                }
            }
        }
        return consumed(&fields);
    }
    let hy = p["type"] == "hysteria2";
    if hy && (security != "tls" || !matches!(net, "tcp" | "hysteria")) {
        return Err(invalid("Hysteria 2 requires TLS and QUIC."));
    }
    if !hy {
        p["tls"] = json!(security != "none");
        p["network"] = json!(net);
    }
    let sec = match security.as_str() {
        Some("reality") => "realitySettings",
        Some("none" | "tls") => "tlsSettings",
        _ => return Err(invalid("Unsupported Xray security.")),
    };
    let mut tls = object(&fields.remove(sec).unwrap_or_default())?;
    if security == "reality" {
        let public_key = tls.remove("publicKey");
        let password = tls.remove("password");
        let key = public_key
            .as_ref()
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .or_else(|| {
                password
                    .as_ref()
                    .and_then(Value::as_str)
                    .filter(|key| !key.is_empty())
            })
            .ok_or_else(|| invalid("Missing Reality public key."))?;
        p["reality-opts"] =
            json!({"public-key":key,"short-id":tls.remove("shortId").unwrap_or(json!(""))});
        tls.remove("spiderX");
        tls.remove("show");
    }
    if hy {
        tls.remove("fingerprint");
    }
    transfer(
        &mut tls,
        p,
        &[
            ("serverName", if hy { "sni" } else { "servername" }),
            ("allowInsecure", "skip-cert-verify"),
            ("alpn", "alpn"),
            ("fingerprint", "client-fingerprint"),
        ],
    );
    consumed(&tls)?;
    if hy {
        let mut transport = object(&fields.remove("hysteriaSettings").unwrap_or_default())?;
        transport.remove("auth");
        transport.remove("version");
        consumed(&transport)?;
        let mut mask = object(&fields.remove("finalmask").unwrap_or_default())?;
        let mut quic = object(&mask.remove("quicParams").unwrap_or_default())?;
        // With no bandwidth set, mihomo uses BBR. Debug logging is local to Xray.
        if quic.get("congestion") == Some(&json!("bbr"))
            && !quic.contains_key("brutalUp")
            && !quic.contains_key("brutalDown")
        {
            quic.remove("congestion");
        }
        quic.remove("debug");
        transfer(
            &mut quic,
            p,
            &[
                ("brutalUp", "up"),
                ("brutalDown", "down"),
                ("initStreamReceiveWindow", "initial-stream-receive-window"),
                ("maxStreamReceiveWindow", "max-stream-receive-window"),
                (
                    "initConnectionReceiveWindow",
                    "initial-connection-receive-window",
                ),
                (
                    "maxConnectionReceiveWindow",
                    "max-connection-receive-window",
                ),
            ],
        );
        consumed(&quic)?;
        consumed(&mask)?;
    } else {
        let raw = fields.remove(&format!("{net}Settings")).unwrap_or_default();
        let mut opts = object(&raw)?;
        let mut target = json!({});
        match net {
            "tcp" => {
                if let Some(raw) = fields.remove("rawSettings") {
                    if !opts.is_empty() && raw != Value::Object(opts.clone()) {
                        return Err(invalid("Conflicting TCP settings."));
                    }
                    opts = object(&raw)?;
                }
                if opts.get("header") == Some(&json!({"type":"none"})) {
                    opts.remove("header");
                }
            }
            "ws" => {
                let host = opts.remove("host");
                transfer(
                    &mut opts,
                    &mut target,
                    &[
                        ("path", "path"),
                        ("headers", "headers"),
                        ("maxEarlyData", "max-early-data"),
                        ("earlyDataHeaderName", "early-data-header-name"),
                    ],
                );
                if let Some(host) = host {
                    if target["headers"].is_null() {
                        target["headers"] = json!({});
                    }
                    target["headers"]
                        .as_object_mut()
                        .ok_or_else(|| invalid("Expected WebSocket headers to be an object."))?
                        .insert("Host".into(), host);
                }
                p["ws-opts"] = target;
            }
            "grpc" => {
                if opts.remove("multiMode") == Some(json!(true))
                    || opts.remove("mode").is_some_and(|v| v != "gun")
                {
                    return Err(invalid("Unsupported gRPC mode."));
                }
                transfer(
                    &mut opts,
                    &mut target,
                    &[("serviceName", "grpc-service-name")],
                );
                p["grpc-opts"] = target;
            }
            "xhttp" => {
                let mut merged = object(&opts.remove("extra").unwrap_or_default())?;
                merged.append(&mut opts);
                opts = merged;
                // These fields only configure Xray's listening HTTP server.
                for key in ["noSSEHeader", "scMaxBufferedPosts", "scStreamUpServerSecs"] {
                    opts.remove(key);
                }
                for (src, dst) in [
                    ("path", "path"),
                    ("headers", "headers"),
                    ("host", "host"),
                    ("mode", "mode"),
                    ("noGRPCHeader", "no-grpc-header"),
                    ("xPaddingBytes", "x-padding-bytes"),
                    ("xPaddingObfsMode", "x-padding-obfs-mode"),
                    ("xPaddingKey", "x-padding-key"),
                    ("xPaddingHeader", "x-padding-header"),
                    ("xPaddingPlacement", "x-padding-placement"),
                    ("xPaddingMethod", "x-padding-method"),
                    ("uplinkHTTPMethod", "uplink-http-method"),
                    ("uplinkDataPlacement", "uplink-data-placement"),
                    ("uplinkDataKey", "uplink-data-key"),
                    ("uplinkChunkSize", "uplink-chunk-size"),
                    ("sessionPlacement", "session-placement"),
                    ("sessionKey", "session-key"),
                    ("seqPlacement", "seq-placement"),
                    ("seqKey", "seq-key"),
                    ("scMaxEachPostBytes", "sc-max-each-post-bytes"),
                    ("scMinPostsIntervalMs", "sc-min-posts-interval-ms"),
                ] {
                    transfer(&mut opts, &mut target, &[(src, dst)]);
                }
                for k in [
                    "x-padding-bytes",
                    "uplink-chunk-size",
                    "sc-max-each-post-bytes",
                    "sc-min-posts-interval-ms",
                ] {
                    stringify(&mut target, k);
                }
                if let Some(xmux) = opts.remove("xmux") {
                    let mut mux = object(&xmux)?;
                    let mut reuse = json!({});
                    for (from, to) in [
                        ("maxConcurrency", "max-concurrency"),
                        ("maxConnections", "max-connections"),
                        ("cMaxReuseTimes", "c-max-reuse-times"),
                        ("hMaxRequestTimes", "h-max-request-times"),
                        ("hMaxReusableSecs", "h-max-reusable-secs"),
                        ("hKeepAlivePeriod", "h-keep-alive-period"),
                    ] {
                        transfer(&mut mux, &mut reuse, &[(from, to)]);
                        if from != "hKeepAlivePeriod" {
                            stringify(&mut reuse, to);
                        }
                    }
                    consumed(&mux)?;
                    target["reuse-settings"] = reuse;
                }
                p["xhttp-opts"] = target;
            }
            _ => return Err(invalid("Unsupported Xray transport.")),
        }
        consumed(&opts)?;
    }
    consumed(&fields)
}
fn stringify(v: &mut Value, k: &str) {
    if v[k].is_number() {
        v[k] = Value::String(v[k].to_string());
    }
}
