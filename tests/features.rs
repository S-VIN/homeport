//! Geo rules, connections, configuration exchange and the tunnel with the real bundled mihomo.
use homeport_kernel::{Host, Id, Kernel, list::Add};
use plugin_local_proxy::{PROXY, SaveProxy};
use plugin_routing::{GEO, Route, Rule};
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
    let host = Host::start_enabled(homeport::catalog(), dir).await.unwrap();
    let k = host.kernel();
    let mut inbound = (*k.snapshot().get(&PROXY)).clone();
    inbound.port = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    k.cx_for("local-proxy")
        .call(SaveProxy(inbound))
        .await
        .unwrap();
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
        "geosite:no-such-list-homeport",
    ] {
        routing
            .call(Add::new(
                &plugin_routing::RULES,
                Rule {
                    id: Id::random(),
                    pattern: pattern.into(),
                    action: Route::Direct,
                },
            ))
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
    assert!(
        !k.snapshot()
            .live(&GEO)
            .get("geosite-no-such-list-homeport")
            .is_some_and(|s| s.rules > 0)
    );
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
    homeport_kernel::mihomo::http()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "registers a Cloudflare WARP device"]
async fn warp_is_set_up_and_carries_traffic_as_the_exit() {
    use plugin_chain_exit as exit;
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    // The entry: this machine's own upstream proxy (HOMEPORT_TEST_UPSTREAM=host:port, SOCKS5 with UDP),
    // or none when the network reaches Cloudflare directly.
    if let Ok(upstream) = std::env::var("HOMEPORT_TEST_UPSTREAM") {
        let (host_, port) = upstream.rsplit_once(':').unwrap();
        let doc = format!(
            "proxies:\n  - {{name: Upstream, type: socks5, server: {host_}, port: {port}, udp: true}}"
        );
        let id = Id::random();
        let proxies = k
            .cx_for("subscriptions")
            .call(plugin_subscriptions::Parse {
                source: id,
                body: doc,
            })
            .await
            .unwrap()
            .proxies;
        k.cx_for("servers")
            .call(plugin_servers::ReplaceFromSource {
                source: id,
                proxies,
                guard: None,
            })
            .await
            .unwrap();
        let server = k.snapshot().get(&plugin_servers::SERVERS)[0].id;
        k.cx_for("servers")
            .call(plugin_servers::Select::manual(Some(server)))
            .await
            .unwrap();
    } else {
        k.cx_for("chain-exit")
            .call(homeport_kernel::Update {
                state: &exit::SETTINGS,
                change: Box::new(|s| s.entry = exit::Entry::Direct),
            })
            .await
            .unwrap();
    }
    let cx = k.cx_for("chain-exit");
    cx.call(exit::SetUpWarp).await.unwrap();
    assert_eq!(exit::plan(&k.snapshot()), Some(exit::Plan::Warp));
    let port = k.snapshot().get(&PROXY).port;
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
    use plugin_statistics::{Close, TRAFFIC};
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    // An open window: connections are sampled every second.
    let _window = host.session(&homeport_kernel::egui::Context::default());
    let k = host.kernel();
    k.cx_for("routing")
        .call(Add::new(
            &plugin_routing::RULES,
            Rule {
                id: Id::random(),
                pattern: "*".into(),
                action: Route::Direct,
            },
        ))
        .await
        .unwrap();
    let port = k.snapshot().get(&PROXY).port;
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
    k.cx_for("statistics").call(Close(None)).await.unwrap();
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
        .call(Add::new(&plugin_routing::RULES, rule.clone()))
        .await
        .unwrap();
    let bytes = host.export_configuration().unwrap();
    let config = host.prepare_import("saved.sqlite3".into(), &bytes).unwrap();
    k.cx_for("routing")
        .call(homeport_kernel::list::Remove::new(
            &plugin_routing::RULES,
            rule.id,
        ))
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
    assert!(
        !dir.path().join("backups").exists(),
        "export/import must not create hidden copies"
    );
    assert!(k.core_status().running());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_tunnel_authorization_keeps_the_saved_setting_and_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    // Library hosts do not configure a supervisor, so they cannot open OS dialogs.
    let error = k
        .cx_for("local-proxy")
        .call(homeport_kernel::Update {
            state: &plugin_local_proxy::PROXY,
            change: Box::new(|d| d.tun = true),
        })
        .await
        .unwrap_err();
    assert!(
        error.message.contains("authorization is unavailable"),
        "{error}"
    );
    assert!(k.core_status().running(), "the rest keeps working");
    assert!(!k.snapshot().get(&plugin_local_proxy::PROXY).tun);
    assert!(!k.core_api().tun_enabled().await.unwrap());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_installation_that_cannot_start_explains_why_and_saves_nothing() {
    use plugin_install_to_server::{DEPLOYMENTS, Install, PROGRESS, Status, deploy::Options};
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    let cx = k.cx_for("install-to-server");
    let error = cx.call(Install(Options::default())).await.unwrap_err();
    assert_eq!(error.message, "Enter the server address.");
    // A port nothing listens on: SSH is unreachable.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let options = Options {
        host: "127.0.0.1".into(),
        ssh_port: closed.local_addr().unwrap().port(),
        password: "secret".into(),
        ..Options::default()
    };
    drop(closed);
    let error = cx.call(Install(options)).await.unwrap_err();
    assert!(
        error.message.contains("does not answer over SSH"),
        "{error}"
    );
    assert_eq!(*k.snapshot().live(&PROGRESS), Status::Failed(error.message));
    assert!(k.snapshot().get(&DEPLOYMENTS).is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn uninstalling_from_an_unreachable_server_keeps_its_record() {
    use plugin_install_to_server::{DEPLOYMENTS, Deployment, REMOVAL, Status, Uninstall};
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path()).await;
    let k = host.kernel();
    let cx = k.cx_for("install-to-server");
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let record = Deployment {
        id: Id::random(),
        host: "127.0.0.1".into(),
        ssh_port: closed.local_addr().unwrap().port(),
        ssh_login: "root".into(),
        directory: "/root/homeport".into(),
        panel: "https://panel.example.com:8443".into(),
        login: "admin".into(),
        password: "panel".into(),
        ready: true,
    };
    drop(closed);
    cx.call(Add::new(&DEPLOYMENTS, record.clone()))
        .await
        .unwrap();
    let uninstall = |password: &str| Uninstall {
        id: record.id,
        login: "root".into(),
        password: password.into(),
    };
    let error = cx.call(uninstall("")).await.unwrap_err();
    assert_eq!(error.message, "Enter the SSH login and password.");
    let error = cx.call(uninstall("secret")).await.unwrap_err();
    assert!(
        error.message.contains("does not answer over SSH"),
        "{error}"
    );
    assert_eq!(*k.snapshot().live(&REMOVAL), Status::Failed(error.message));
    assert_eq!(*k.snapshot().get(&DEPLOYMENTS), vec![record]);
    host.shutdown().await;
}
