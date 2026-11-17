//! End-to-end tests with the bundled mihomo and local HTTP fixtures.
use plugin_health_checks as health;
use plugin_local_proxy::{PROXY, Proxy, SaveProxy};
use plugin_routing::{RULES, Route, Rule};
use plugin_servers::{SELECTED, SERVERS, Select};
use plugin_subscriptions::{Parse, RefreshAll, SOURCES, Source};
use rproxy_kernel::{Host, Id, Kernel, list::Add, list::Remove};
use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Tiny HTTP server; also acts as an HTTP proxy for CONNECT to 127.0.0.1.
async fn fixture(body: Arc<RwLock<String>>, status: u16) -> (u16, Arc<AtomicUsize>) {
    let (port, count, _) = stoppable_fixture(body, status).await;
    (port, count)
}
/// [`fixture`] that stops accepting connections when its task is aborted.
async fn stoppable_fixture(
    body: Arc<RwLock<String>>,
    status: u16,
) -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let hits = count.clone();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let (body, hits) = (body.clone(), hits.clone());
            tokio::spawn(async move {
                let mut req = vec![0; 8192];
                let Ok(n) = stream.read(&mut req).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                hits.fetch_add(1, Ordering::SeqCst);
                let txt = body.read().unwrap().clone();
                let (code, txt) = if req.starts_with(b"CONNECT 127.0.0.1:") {
                    if stream
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if stream.read(&mut req).await.unwrap_or(0) == 0 {
                        return;
                    }
                    (status, txt)
                } else if req.starts_with(b"CONNECT ") {
                    (502, String::new())
                } else {
                    (status, txt)
                };
                let msg = format!(
                    "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{txt}",
                    txt.len()
                );
                // mihomo reports a delay test that took 0 ms as failed.
                tokio::time::sleep(Duration::from_millis(3)).await;
                let _ = stream.write_all(msg.as_bytes()).await;
            });
        }
    });
    (port, count, task)
}

async fn until(k: &Kernel, what: &str, ok: impl Fn(&Kernel) -> bool) {
    let mut changes = k.changes();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ok(k) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Start with all plugins and move the seeded listener to a free port.
async fn start(dir: &std::path::Path) -> (Arc<Host>, Proxy) {
    let host = Host::start(rproxy::catalog(), dir).await.unwrap();
    let k = host.kernel();
    // Tests run in parallel: move the proxy to a free port.
    let mut inbound = (*k.snapshot().get(&PROXY)).clone();
    inbound.port = free_port().await;
    k.cx_for("local-proxy")
        .call(SaveProxy(inbound.clone()))
        .await
        .unwrap();
    (host, inbound)
}

fn proxy_client(port: u16) -> reqwest::Client {
    rproxy_kernel::mihomo::http()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn formats_are_claimed_by_their_plugins() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(rproxy::catalog(), dir.path()).await.unwrap();
    let cx = host.kernel().cx_for("subscriptions");
    let parse = |body: String| {
        let cx = cx.clone();
        async move {
            cx.call(Parse {
                source: Id::random(),
                body,
            })
            .await
        }
    };
    let link = "vless://9e78dff8-0445-4b12-8822-276f98d21ea5@example.org:443?type=ws&security=tls&sni=host.example&path=%2Fa&host=cdn.example#West";
    for body in [
        link.to_string(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(link),
    ] {
        let p = parse(body).await.unwrap().pop().unwrap();
        assert_eq!(p.name, "West");
        assert_eq!(p.options["ws-opts"]["headers"]["Host"], "cdn.example");
    }
    let xray = r#"[{"remarks":"Lab","outbounds":[{"protocol":"freedom"},{"protocol":"shadowsocks","settings":{"servers":[{"address":"127.0.0.1","port":1234,"method":"aead_aes_128_gcm","password":"example","uot":true}]}}]},{"protocol":"hysteria","settings":{"address":"node","port":443,"version":2},"streamSettings":{"network":"hysteria","security":"tls","tlsSettings":{"serverName":"node"},"hysteriaSettings":{"auth":"example"},"finalmask":{"quicParams":{"brutalUp":100}}}}]"#;
    let peers = parse(xray.into()).await.unwrap();
    assert_eq!(peers.len(), 2);
    assert_eq!(peers[0].options["cipher"], "aes-128-gcm");
    let clash =
        parse("proxies:\n  - { name: a, type: http, server: localhost, port: 8080 }".into())
            .await
            .unwrap();
    assert_eq!(clash[0].server, "localhost");
    assert!(
        parse("proxies:\n  - { name: bad, type: http, server: localhost, port: 0 }".into())
            .await
            .is_err()
    );
    assert!(
        parse("Cg==".into()).await.unwrap().is_empty(),
        "Base64 of an empty document is an empty subscription"
    );
    for invalid in ["=", "====", "eA==", "not a subscription!"] {
        assert!(parse(invalid.into()).await.is_err(), "{invalid}");
    }
    // A direct link is validated by the format plugin when added.
    let add = |url: &str| {
        host.kernel()
            .cx_for("subscriptions")
            .call(Add::new(&SOURCES, Source::new(url)))
    };
    assert!(add("vless://%FF@example.org:443").await.is_err());
    assert!(add("ftp://example.org/list").await.is_err());
    // AmneziaWG: a config pasted into the one-line field, and an Amnezia VPN key.
    let config = "[Interface]\nAddress = 10.8.1.2/32\nPrivateKey = a2V5\nJc = 4\nH1 = 1\n\n[Peer]\nPublicKey = cHVi\nEndpoint = 203.0.113.7:41820\n";
    add(&config.replace('\n', " ")).await.unwrap();
    let profile = serde_json::json!({"description": "Office", "containers": [
        {"container": "amnezia-awg", "awg": {"last_config": serde_json::json!({"config": config}).to_string()}}
    ]});
    let key = format!(
        "vpn://{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(profile.to_string())
    );
    add(&key).await.unwrap();
    let wg = parse(key).await.unwrap().pop().unwrap();
    assert_eq!((wg.name.as_str(), wg.port), ("Office", 41820));
    assert_eq!(wg.options["amnezia-wg-option"]["jc"], 4);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn real_core_refresh_select_route_listeners_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (host, inbound) = start(dir.path()).await;
    let k = host.kernel();
    let port = inbound.port;
    let (proxy, hits) = fixture(Arc::new(RwLock::new("via upstream".into())), 200).await;
    let (origin, _) = fixture(Arc::new(RwLock::new("from origin".into())), 200).await;
    let body = Arc::new(RwLock::new(format!(
        "proxies:\n  - {{name: West, type: http, server: 127.0.0.1, port: {proxy}}}"
    )));
    let (provider, _) = fixture(body.clone(), 200).await;
    let subs = k.cx_for("subscriptions");
    subs.call(Add::new(
        &SOURCES,
        Source::new(format!("http://127.0.0.1:{provider}/subscription")),
    ))
    .await
    .unwrap();
    until(&k, "servers", |k| !k.snapshot().get(&SERVERS).is_empty()).await;
    let server = k.snapshot().get(&SERVERS)[0].id;
    k.cx_for("servers")
        .call(Select::manual(Some(server)))
        .await
        .unwrap();
    let client = proxy_client(port);
    let url = format!("http://127.0.0.1:{origin}/probe");
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "via upstream"
    );

    // Routing: whole-domain wildcard match, order matters.
    let routing = k.cx_for("routing");
    let rule = Rule {
        id: Id::random(),
        pattern: "*AB*cd*end.invalid".into(),
        action: Route::Block,
    };
    routing.call(Add::new(&RULES, rule.clone())).await.unwrap();
    assert_eq!(k.snapshot().get(&RULES)[0].pattern, "*ab*cd*end.invalid");
    for blocked in ["AB-cd-END.invalid", "abcdend.invalid"] {
        let before = hits.load(Ordering::SeqCst);
        let r = client.get(format!("http://{blocked}/probe")).send().await;
        assert!(
            !r.as_ref().is_ok_and(|r| r.status().is_success()),
            "{blocked} was forwarded"
        );
        assert_eq!(hits.load(Ordering::SeqCst), before);
    }
    for passed in ["ab-cd-end.invalid.extra", "cd-ab-end.invalid"] {
        let before = hits.load(Ordering::SeqCst);
        let _ = client.get(format!("http://{passed}/probe")).send().await;
        assert!(hits.load(Ordering::SeqCst) > before, "{passed}");
    }
    routing.call(Remove::new(&RULES, rule.id)).await.unwrap();

    // A broken document keeps the last working catalog and the connection.
    let working = body.read().unwrap().clone();
    *body.write().unwrap() = r#"{"protocol":"shadowsocks","settings":{"address":"127.0.0.1","port":1,"method":"aes-128-gcm","password":"x","unsupportedOption":true}}"#.into();
    assert!(subs.call(RefreshAll).await.is_err());
    assert_eq!(*k.snapshot().get(&SELECTED), Some(server));
    assert!(k.snapshot().get(&SOURCES)[0].error.is_some());
    // An empty document is valid and clears the catalog (and the selection).
    for empty in [" \n", "Cg=="] {
        *body.write().unwrap() = empty.into();
        subs.call(RefreshAll).await.unwrap();
        assert!(k.snapshot().get(&SERVERS).is_empty());
        assert!(k.snapshot().get(&SELECTED).is_none());
        assert!(k.snapshot().get(&SOURCES)[0].error.is_none());
        *body.write().unwrap() = working.clone();
        subs.call(RefreshAll).await.unwrap();
        assert_eq!(
            k.snapshot().get(&SERVERS)[0].id,
            server,
            "ids are stable across refreshes"
        );
        k.cx_for("servers")
            .call(Select::manual(Some(server)))
            .await
            .unwrap();
    }

    // Editing a listener onto a busy port is rejected; the working one stays.
    let inbounds = k.cx_for("local-proxy");
    let busy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let moved = Proxy {
        port: busy.local_addr().unwrap().port(),
        ..inbound.clone()
    };
    assert!(inbounds.call(SaveProxy(moved)).await.is_err());
    assert_eq!(k.snapshot().get(&PROXY).port, port);
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        "via upstream"
    );
    drop(busy);

    // Credentials are enforced on the real endpoint, for HTTP and SOCKS5.
    let protected = Proxy {
        login: Some("user".into()),
        password: Some("secret".into()),
        ..inbound.clone()
    };
    inbounds.call(SaveProxy(protected)).await.unwrap();
    assert_eq!(
        client.get(&url).send().await.unwrap().status().as_u16(),
        407
    );
    for scheme in ["http", "socks5h"] {
        let c = rproxy_kernel::mihomo::http()
            .no_proxy()
            .proxy(
                reqwest::Proxy::all(format!("{scheme}://127.0.0.1:{port}"))
                    .unwrap()
                    .basic_auth("user", "secret"),
            )
            .timeout(Duration::from_secs(4))
            .build()
            .unwrap();
        assert_eq!(
            c.get(&url).send().await.unwrap().text().await.unwrap(),
            "via upstream",
            "{scheme}"
        );
    }
    // An empty password keeps the saved one.
    let edited = Proxy {
        password: None,
        ..(*k.snapshot().get(&PROXY)).clone()
    };
    inbounds.call(SaveProxy(edited)).await.unwrap();
    assert_eq!(k.snapshot().get(&PROXY).password.as_deref(), Some("secret"));

    // State survives a restart.
    drop(k);
    host.restart().await;
    let k = host.kernel();
    assert_eq!(*k.snapshot().get(&SELECTED), Some(server));
    assert_eq!(k.snapshot().get(&SOURCES).len(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn health_checks_use_the_isolated_route() {
    let dir = tempfile::tempdir().unwrap();
    let (host, inbound) = start(dir.path()).await;
    let k = host.kernel();
    let (proxy, _) = fixture(Arc::new(RwLock::new("via upstream".into())), 204).await;
    let (origin, _) = fixture(Arc::new(RwLock::new(String::new())), 204).await;
    let body = Arc::new(RwLock::new(format!(
        "proxies:\n  - {{name: Up, type: http, server: 127.0.0.1, port: {proxy}}}"
    )));
    let (provider, _) = fixture(body, 200).await;
    k.cx_for("subscriptions")
        .call(Add::new(
            &SOURCES,
            Source::new(format!("http://127.0.0.1:{provider}/s")),
        ))
        .await
        .unwrap();
    until(&k, "servers", |k| !k.snapshot().get(&SERVERS).is_empty()).await;
    let server = k.snapshot().get(&SERVERS)[0].clone();
    k.cx_for("health-checks")
        .call(Add::new(
            &health::TESTS,
            health::Test {
                id: Id::random(),
                url: format!("http://127.0.0.1:{origin}/generate_204"),
                expected: health::Expected::NoContent,
            },
        ))
        .await
        .unwrap();
    let h = k
        .cx_for("health-checks")
        .call(health::CheckServer { id: server.id })
        .await
        .unwrap()
        .unwrap();
    assert!(h.ping_ms.is_some());
    assert!(h.rating.unwrap() > 0);
    let results = k.snapshot().get(&health::RESULTS);
    assert!(
        results.iter().all(|r| r.passed && r.server == server.id),
        "{results:?}"
    );
    // Checks never touch the user's route: nothing is selected, so user traffic is blocked.
    let r = proxy_client(inbound.port)
        .get(format!("http://127.0.0.1:{origin}/x"))
        .send()
        .await;
    assert!(!r.is_ok_and(|r| r.status().as_u16() == 204));
    // Removing the source cascades to its servers' health.
    let source = k.snapshot().get(&SOURCES)[0].id;
    k.cx_for("subscriptions")
        .call(Remove::new(&SOURCES, source))
        .await
        .unwrap();
    until(&k, "cascade", |k| {
        k.snapshot().get(&health::HEALTH).is_empty()
    })
    .await;
    host.shutdown().await;
}

/// With a window open, Logs shows the user's traffic in the user's words.
#[tokio::test(flavor = "multi_thread")]
async fn logs_follow_traffic_while_a_window_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let (host, inbound) = start(dir.path()).await;
    let k = host.kernel();
    let (origin, _) = fixture(Arc::new(RwLock::new("from origin".into())), 200).await;
    let _window = host.session(&egui::Context::default());
    let url = format!("http://127.0.0.1:{origin}/probe");
    let _ = proxy_client(inbound.port).get(&url).send().await;
    until(&k, "a log line for the request", |k| {
        k.snapshot()
            .live(&plugin_logs::LINES)
            .iter()
            .any(|l| !l.internal && l.text.contains(&format!("127.0.0.1:{origin}")))
    })
    .await;
    host.shutdown().await;
}

/// Two subscriptions that load together each get their new servers tested.
#[tokio::test(flavor = "multi_thread")]
async fn new_servers_of_every_subscription_are_tested() {
    let dir = tempfile::tempdir().unwrap();
    let (host, _) = start(dir.path()).await;
    let k = host.kernel();
    let load = |name: &'static str| {
        let k = k.clone();
        async move {
            let source = Id::random();
            // Nothing listens on port 1: the test fails fast but leaves a result.
            let body =
                format!("proxies:\n  - {{name: {name}, type: socks5, server: 127.0.0.1, port: 1}}");
            let proxies = k
                .cx_for("subscriptions")
                .call(Parse { source, body })
                .await
                .unwrap();
            k.cx_for("subscriptions")
                .call(plugin_servers::ReplaceFromSource {
                    source,
                    proxies,
                    still_current: None,
                })
                .await
                .unwrap();
        }
    };
    tokio::join!(load("First"), load("Second"));
    // A source that loads while a full pass runs is tested after it.
    let pass = k.cx_for("health-checks").call(health::CheckAll);
    let (passed, ()) = tokio::join!(pass, load("Third"));
    passed.unwrap();
    until(&k, "every server tested", |k| {
        let snap = k.snapshot();
        let servers = snap.get(&SERVERS);
        servers.len() == 3
            && servers
                .iter()
                .all(|s| health::health(&snap, s.id).is_some_and(|h| h.checked.is_some()))
    })
    .await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_dependencies_disable_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let catalog: Vec<_> = rproxy::catalog()
        .into_iter()
        .filter(|p| p.id != "routing")
        .collect();
    let host = Host::start(catalog, dir.path()).await.unwrap();
    let ids: Vec<_> = host.kernel().plugins().iter().map(|p| p.id).collect();
    assert!(
        !ids.contains(&"vless-inbound"),
        "vless-inbound requires routing"
    );
    // Auto select and the chain only need servers.
    assert!(ids.contains(&"health-checks") && ids.contains(&"chain-exit"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_select_picks_a_healthy_server_and_manual_choice_turns_it_off() {
    use plugin_health_checks::auto::{AUTO, SetAuto};
    let dir = tempfile::tempdir().unwrap();
    let (host, _) = start(dir.path()).await;
    let k = host.kernel();
    let (good, _) = fixture(Arc::new(RwLock::new(String::new())), 204).await;
    let dead = free_port().await;
    let (origin, _) = fixture(Arc::new(RwLock::new(String::new())), 204).await;
    let doc = format!(
        "proxies:\n  - {{name: Good, type: http, server: 127.0.0.1, port: {good}}}\n  - {{name: Dead, type: http, server: 127.0.0.1, port: {dead}}}"
    );
    let (provider, _) = fixture(Arc::new(RwLock::new(doc)), 200).await;
    k.cx_for("subscriptions")
        .call(Add::new(
            &SOURCES,
            Source::new(format!("http://127.0.0.1:{provider}/s")),
        ))
        .await
        .unwrap();
    until(&k, "servers", |k| k.snapshot().get(&SERVERS).len() == 2).await;
    let id = |name: &str| {
        k.snapshot()
            .get(&SERVERS)
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .id
    };
    k.cx_for("health-checks")
        .call(Add::new(
            &health::TESTS,
            health::Test {
                id: Id::random(),
                url: format!("http://127.0.0.1:{origin}/"),
                expected: health::Expected::NoContent,
            },
        ))
        .await
        .unwrap();
    k.cx_for("health-checks")
        .call(health::CheckAll)
        .await
        .unwrap();
    // The dead server is excluded and cannot be connected manually.
    assert!(
        k.cx_for("servers")
            .call(Select::manual(Some(id("Dead"))))
            .await
            .is_err()
    );
    k.cx_for("health-checks").call(SetAuto(true)).await.unwrap();
    until(&k, "automatic selection", |k| {
        *k.snapshot().get(&SELECTED) == Some(id("Good"))
    })
    .await;
    assert!(k.snapshot().get(&AUTO).enabled);
    // A manual choice turns Auto off.
    k.cx_for("servers")
        .call(Select::manual(None))
        .await
        .unwrap();
    assert!(!k.snapshot().get(&AUTO).enabled);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_select_fails_over_when_the_connected_server_dies() {
    use plugin_health_checks::auto::{AUTO, SetAuto};
    let dir = tempfile::tempdir().unwrap();
    let (host, inbound) = start(dir.path()).await;
    let k = host.kernel();
    let (a, _, a_task) = stoppable_fixture(Arc::new(RwLock::new("via A".into())), 200).await;
    let (b, _, b_task) = stoppable_fixture(Arc::new(RwLock::new("via B".into())), 200).await;
    let (origin, _) = fixture(Arc::new(RwLock::new(String::new())), 200).await;
    let doc = format!(
        "proxies:\n  - {{name: A, type: http, server: 127.0.0.1, port: {a}}}\n  - {{name: B, type: http, server: 127.0.0.1, port: {b}}}"
    );
    let (provider, _) = fixture(Arc::new(RwLock::new(doc)), 200).await;
    k.cx_for("subscriptions")
        .call(Add::new(
            &SOURCES,
            Source::new(format!("http://127.0.0.1:{provider}/s")),
        ))
        .await
        .unwrap();
    until(&k, "servers", |k| k.snapshot().get(&SERVERS).len() == 2).await;
    k.cx_for("health-checks")
        .call(Add::new(
            &health::TESTS,
            health::Test {
                id: Id::random(),
                url: format!("http://127.0.0.1:{origin}/"),
                expected: health::Expected::Success,
            },
        ))
        .await
        .unwrap();
    k.cx_for("health-checks")
        .call(health::CheckAll)
        .await
        .unwrap();
    k.cx_for("health-checks").call(SetAuto(true)).await.unwrap();
    // Connected, with the other server as the fallback group's backup.
    until(&k, "connection with a backup", |k| {
        k.snapshot().get(&SELECTED).is_some() && !k.snapshot().get(&AUTO).backups.is_empty()
    })
    .await;
    let name = |k: &Kernel, id: Option<Id>| {
        k.snapshot()
            .get(&SERVERS)
            .iter()
            .find(|s| Some(s.id) == id)
            .map(|s| s.name.clone())
    };
    let first = name(&k, *k.snapshot().get(&SELECTED)).unwrap();
    let (dead, other) = if first == "A" {
        (&a_task, "B")
    } else {
        (&b_task, "A")
    };
    let client = proxy_client(inbound.port);
    let url = format!("http://127.0.0.1:{origin}/probe");
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        format!("via {first}")
    );

    dead.abort();
    let stopped = std::time::Instant::now();
    let expected = format!("via {other}");
    loop {
        if let Ok(r) = client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            && r.text().await.unwrap_or_default() == expected
        {
            break;
        }
        assert!(
            stopped.elapsed() < Duration::from_secs(8),
            "traffic did not fail over"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!(
        "traffic recovered {:?} after the upstream stopped",
        stopped.elapsed()
    );
    // The watch adopts mihomo's failover as the selection.
    until(&k, "failover adopted", |k| {
        name(k, *k.snapshot().get(&SELECTED)).as_deref() == Some(other)
    })
    .await;
    println!(
        "selection followed {:?} after the upstream stopped",
        stopped.elapsed()
    );
    assert!(k.snapshot().get(&AUTO).enabled);
    assert_eq!(
        client.get(&url).send().await.unwrap().text().await.unwrap(),
        expected
    );
    host.shutdown().await;
}
