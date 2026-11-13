//! Geo rules, connections, configuration exchange and the tunnel with the real bundled mihomo.
use plugin_inbounds::INBOUNDS;
use plugin_routing::{Route, Rule, GEO};
use rproxy_kernel::{list::Add, list::Save, Host, Id, Kernel};
use std::{sync::Arc, time::Duration};

async fn until(k: &Kernel, what: &str, secs: u64, ok: impl Fn(&Kernel) -> bool) {
    let mut changes = k.changes();
    tokio::time::timeout(Duration::from_secs(secs), async {
        while !ok(k) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn start(dir: &std::path::Path) -> Arc<Host> {
    let host = Host::start(rproxy::catalog(), dir).await.unwrap();
    let k = host.kernel();
    until(&k, "seeded listener", 15, |k| {
        !k.snapshot().get(&INBOUNDS).is_empty()
    })
    .await;
    let mut inbound = k.snapshot().get(&INBOUNDS)[0].clone();
    inbound.port = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // The seeded listener starts off when its default port is taken (another rproxy).
    inbound.enabled = true;
    k.cx_for("inbounds").call(Save(inbound)).await.unwrap();
    host
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads geo rule sets"]
async fn geo_rules_download_and_unknown_lists_do_not_break_the_core() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    let routing = k.cx_for("routing");
    for pattern in [
        "geosite:category-ru",
        "geoip:ru",
        "geosite:no-such-list-rproxy",
    ] {
        routing
            .call(Add::new(Rule {
                id: Id::random(),
                pattern: pattern.into(),
                action: Route::Direct,
            }))
            .await
            .unwrap();
    }
    assert!(k.core_status().running());
    until(&k, "geo lists", 60, |k| {
        let g = k.snapshot().live(&GEO);
        g.get("geosite-category-ru").is_some_and(|s| s.rules > 100)
            && g.get("geoip-ru").is_some_and(|s| s.rules > 100)
    })
    .await;
    assert!(!k
        .snapshot()
        .live(&GEO)
        .get("geosite-no-such-list-rproxy")
        .is_some_and(|s| s.rules > 0));
    k.cx_for("routing")
        .call(plugin_routing::UpdateGeo)
        .await
        .unwrap_err();
    assert!(
        dir.path().join("core/rules/geoip-ru.mrs").is_file(),
        "cached in the core home"
    );
    host.shutdown().await;
}

fn via(port: u16) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "registers a Cloudflare WARP device"]
async fn warp_is_set_up_and_carries_traffic_as_the_exit() {
    use plugin_exit_server as exit;
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    // The entry: this machine's own upstream proxy (RPROXY_TEST_UPSTREAM=host:port, SOCKS5 with UDP),
    // or none when the network reaches Cloudflare directly.
    if let Ok(upstream) = std::env::var("RPROXY_TEST_UPSTREAM") {
        let (host_, port) = upstream.rsplit_once(':').unwrap();
        let doc = format!("proxies:\n  - {{name: Upstream, type: socks5, server: {host_}, port: {port}, udp: true}}");
        let id = Id::random();
        let proxies = k
            .cx_for("subscriptions")
            .call(plugin_subscriptions::Parse {
                source: id,
                body: doc,
            })
            .await
            .unwrap();
        k.cx_for("servers")
            .call(plugin_servers::ReplaceFromSource {
                source: id,
                proxies,
                still_current: None,
            })
            .await
            .unwrap();
        let server = k.snapshot().get(&plugin_servers::SERVERS)[0].id;
        k.cx_for("servers")
            .call(plugin_servers::Select::manual(Some(server)))
            .await
            .unwrap();
    } else {
        k.cx_for("exit-server")
            .call(rproxy_kernel::Update {
                state: &exit::SETTINGS,
                change: Box::new(|s| s.entry = exit::Entry::Direct),
            })
            .await
            .unwrap();
    }
    let cx = k.cx_for("exit-server");
    cx.call(exit::SetUpWarp).await.unwrap();
    assert_eq!(exit::plan(&k.snapshot()), Some(exit::Plan::Warp));
    let port = k.snapshot().get(&INBOUNDS)[0].port;
    let mut trace = String::new();
    for _ in 0..5 {
        if let Ok(r) = via(port)
            .get("https://www.cloudflare.com/cdn-cgi/trace")
            .send()
            .await
        {
            trace = r.text().await.unwrap_or_default();
            if trace.contains("warp=") {
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        trace.contains("warp=on") || trace.contains("warp=plus"),
        "{trace}"
    );
    host.shutdown().await;
}

/// An origin that answers slowly, so the connection stays open for a while.
async fn slow_origin(body: &'static str, hold: Duration) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0; 4096];
                if s.read(&mut buf).await.unwrap_or(0) > 0 {
                    tokio::time::sleep(hold).await;
                    let msg = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(msg.as_bytes()).await;
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn connections_show_live_traffic_and_can_be_closed() {
    use plugin_connections::{Close, TRAFFIC};
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    // An open window: connections are sampled every second.
    let _window = host.session(&rproxy_kernel::egui::Context::default());
    let k = host.kernel();
    k.cx_for("routing")
        .call(Add::new(Rule {
            id: Id::random(),
            pattern: "*".into(),
            action: Route::Direct,
        }))
        .await
        .unwrap();
    let port = k.snapshot().get(&INBOUNDS)[0].port;
    let origin = slow_origin("held", Duration::from_secs(6)).await;
    let send = move || {
        tokio::spawn(async move {
            via(port)
                .get(format!("http://127.0.0.1:{origin}/"))
                .send()
                .await
        })
    };
    let open = |k: &Kernel| {
        k.snapshot()
            .live(&TRAFFIC)
            .conns
            .iter()
            .any(|c| c.target.ends_with(&format!(":{origin}")) && c.rule.starts_with("Match"))
    };
    // The listener may still be settling after the port change: resend a
    // request that ended before it was seen.
    let mut request = send();
    for _ in 0..100 {
        if open(&k) {
            break;
        }
        if request.is_finished() {
            request = send();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        open(&k),
        "the held connection is listed: {:?}",
        k.snapshot().live(&TRAFFIC).conns
    );
    k.cx_for("connections").call(Close(None)).await.unwrap();
    let r = request.await.unwrap();
    assert!(
        !r.is_ok_and(|r| r.status().is_success()),
        "closing ends the request"
    );
    assert!(k.snapshot().live(&TRAFFIC).up_total > 0);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_round_trip_preserves_settings_without_automatic_backups() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    let rule = Rule {
        id: Id::random(),
        pattern: "*.backup.test".into(),
        action: Route::Block,
    };
    k.cx_for("routing")
        .call(Add::new(rule.clone()))
        .await
        .unwrap();
    let bytes = host.export_configuration().unwrap();
    let config = host.prepare_import("saved.sqlite3".into(), &bytes).unwrap();
    k.cx_for("routing")
        .call(rproxy_kernel::list::Remove::<Rule>::new(rule.id))
        .await
        .unwrap();
    assert!(k.snapshot().get(&plugin_routing::RULES).is_empty());
    host.import_configuration(config).await.unwrap();
    let k = host.kernel();
    assert_eq!(
        k.snapshot()
            .get(&plugin_routing::RULES)
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![rule.id]
    );
    assert!(!dir.path().join("backups").exists(), "export/import must not create hidden copies");
    assert!(k.core_status().running());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tunnel_reports_when_it_cannot_start() {
    use plugin_inbounds::TUN_UP;
    if std::env::var("USER").as_deref() == Ok("root") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    k.cx_for("inbounds")
        .call(rproxy_kernel::Update { state: &plugin_inbounds::DEVICE, change: Box::new(|d| d.tun = true) })
        .await
        .unwrap();
    assert!(k.core_status().running(), "the rest keeps working");
    until(&k, "tunnel status", 15, |k| {
        k.snapshot().live(&TUN_UP).is_some()
    })
    .await;
    assert_eq!(*k.snapshot().live(&TUN_UP), Some(false), "no rights here");
    host.shutdown().await;
}
