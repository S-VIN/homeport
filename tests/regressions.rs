//! Regressions for configuration exchange and subscription identity/concurrency.
use plugin_subscriptions::{SOURCES, Source};
use rproxy_kernel::{
    Host,
    list::{Add, Save},
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn catalog() -> Vec<&'static rproxy_kernel::Plugin> {
    vec![
        &plugin_servers::PLUGIN,
        &plugin_subscriptions::PLUGIN,
        &plugin_format_uri::PLUGIN,
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_imports_leave_the_running_profile_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(rproxy::catalog(), dir.path()).await.unwrap();
    let original = host.export_configuration().unwrap();
    let running = host.kernel();
    let plugins = running.plugins();
    let identity =
        plugin_subscriptions::device_id(&dir.path().join("plugins/subscriptions")).unwrap();
    assert!(
        host.prepare_import("invalid".into(), b"not a database")
            .is_err()
    );
    for damage in [
        "ALTER TABLE kernel_plugins ADD COLUMN unexpected TEXT",
        "INSERT INTO kernel_settings (id,json) VALUES ('kernel.theme','not-json') ON CONFLICT(id) DO UPDATE SET json='not-json'",
        "INSERT INTO kernel_plugins (id,enabled) VALUES ('auto-select',0) ON CONFLICT(id) DO UPDATE SET enabled=0; INSERT INTO kernel_settings (id,json) VALUES ('auto-select.auto','not-json') ON CONFLICT(id) DO UPDATE SET json='not-json'",
        "CREATE TRIGGER unwanted AFTER INSERT ON kernel_plugins BEGIN DELETE FROM kernel_plugins; END",
    ] {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &original).unwrap();
        let db = rproxy_kernel::store::rusqlite::Connection::open(file.path()).unwrap();
        db.execute_batch(damage).unwrap();
        drop(db);
        let bytes = std::fs::read(file.path()).unwrap();
        assert!(
            host.prepare_import("damaged.sqlite3".into(), &bytes)
                .is_err(),
            "must reject: {damage}"
        );
        assert!(host.kernel().core_status().running());
        assert!(
            Arc::ptr_eq(&host.kernel(), &running),
            "validation must not replace the running kernel"
        );
        assert_eq!(
            host.kernel()
                .plugins()
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            plugins.iter().map(|p| p.id).collect::<Vec<_>>()
        );
    }
    let old = host.kernel();
    host.restart().await;
    assert!(old.set_plugin("subscriptions", false).is_err());
    assert!(old.restart_core().await.is_err());
    assert_eq!(
        plugin_subscriptions::device_id(&dir.path().join("plugins/subscriptions")).unwrap(),
        identity
    );
    assert!(!dir.path().join("backups").exists());
    host.shutdown().await;
}

#[tokio::test]
async fn requests_send_hwid_and_report_provider_device_limits() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(catalog(), dir.path()).await.unwrap();
    let id = plugin_subscriptions::device_id(&dir.path().join("plugins/subscriptions")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for limit in [false, true] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 8192];
            let n = socket.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_lowercase();
            assert!(request.contains(&format!("x-hwid: {id}")));
            assert!(request.contains("x-device-model: rproxy"));
            let extra = if limit {
                "x-hwid-max-devices-reached: true\r\n"
            } else {
                ""
            };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\n{extra}Content-Length: 2\r\nConnection: close\r\n\r\nok"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let client = rproxy_kernel::mihomo::http().no_proxy().build().unwrap();
    let url = format!("http://{address}");
    assert_eq!(
        plugin_subscriptions::fetch_http(&host.kernel().cx_for("subscriptions"), &client, &url)
            .await
            .unwrap(),
        "ok"
    );
    let error =
        plugin_subscriptions::fetch_http(&host.kernel().cx_for("subscriptions"), &client, &url)
            .await
            .unwrap_err();
    assert_eq!(error.code, "device-limit");
    task.await.unwrap();
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_a_loading_subscription_never_commits_the_old_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requested = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (ready, wait) = (requested.clone(), release.clone());
    let task = tokio::spawn(async move {
        for label in ["Old", "Current"] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 8192];
            let _ = socket.read(&mut bytes).await.unwrap();
            if label == "Old" {
                ready.notify_one();
                wait.notified().await;
            }
            let body = format!(
                "vless://00000000-0000-0000-0000-000000000001@example.org:443?security=tls#{label}"
            );
            // The old load is cancelled when the link changes: its connection may be gone.
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(catalog(), dir.path()).await.unwrap();
    let k = host.kernel();
    let cx = k.cx_for("subscriptions");
    let source = Source::new(format!("http://{address}/old"));
    cx.call(Add::new(&SOURCES, source.clone())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), requested.notified())
        .await
        .unwrap();
    let mut next = source.clone();
    next.url = format!("http://{address}/current");
    cx.call(Save::new(&SOURCES, next)).await.unwrap();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snap = k.snapshot();
            let servers = snap.get(&plugin_servers::SERVERS);
            assert!(!servers.iter().any(|s| s.name == "Old"));
            if servers.iter().any(|s| s.name == "Current")
                && snap.get(&SOURCES)[0].updated.is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    task.await.unwrap();
    host.shutdown().await;
}

/// Private provider credentials are supplied only locally, never committed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires private subscription URLs and explicit provider access"]
async fn real_hwid_subscriptions_return_servers() {
    let urls = std::env::var("RPROXY_TEST_SUBSCRIPTION_URLS").expect("URLs supplied locally");
    let identity_dir = std::env::var("RPROXY_TEST_IDENTITY_DIR")
        .expect("persistent installation identity directory");
    std::fs::create_dir_all(&identity_dir).unwrap();
    let id = plugin_subscriptions::device_id(std::path::Path::new(&identity_dir)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(catalog(), dir.path()).await.unwrap();
    let client = rproxy_kernel::mihomo::http().no_proxy().build().unwrap();
    for (index, url) in urls.lines().enumerate() {
        let body = plugin_subscriptions::http_get(&client, url, &id)
            .await
            .unwrap_or_else(|e| panic!("subscription {}: {}", index + 1, e.message));
        let proxies = host
            .kernel()
            .cx_for("subscriptions")
            .call(plugin_subscriptions::Parse {
                source: rproxy_kernel::Id::random(),
                body,
            })
            .await
            .unwrap();
        assert!(
            proxies.len() > 1,
            "subscription {} still returned a placeholder",
            index + 1
        );
        let count = proxies.len();
        host.kernel()
            .cx_for("servers")
            .call(plugin_servers::ReplaceFromSource {
                source: rproxy_kernel::Id::random(),
                proxies,
                still_current: None,
            })
            .await
            .unwrap();
        assert!(host.kernel().core_status().running());
        eprintln!(
            "subscription {}: {count} parsed servers, core accepted configuration",
            index + 1
        );
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn import_waits_for_old_writers_and_rejects_late_operations() {
    use rproxy_kernel::{Platform, Plugin, State, categories};
    static VALUE: State<u32> = State::new("writer-test.value");
    static PLUGIN: Plugin = Plugin {
        id: "writer-test",
        name: "Writer test",
        description: "",
        category: categories::NETWORK,
        requires: &[],
        provides: &[],
        required: true,
        default_enabled: true,
        platforms: Platform::ALL,
        setup: |p| {
            p.editable_setting(&VALUE);
            Ok(())
        },
    };
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&PLUGIN], dir.path()).await.unwrap();
    let candidate = host
        .prepare_import(
            "original.sqlite3".into(),
            &host.export_configuration().unwrap(),
        )
        .unwrap();
    let old = host.kernel();
    let cx = old.cx_for("writer-test");
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let write = tokio::spawn(async move {
        cx.write(move |w| {
            w.set(&VALUE, 42);
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            Ok(())
        })
        .await
    });
    entered.await.unwrap();
    let importing = host.clone();
    let import = tokio::spawn(async move { importing.import_configuration(candidate).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !old.closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !import.is_finished(),
        "import must wait for the old transaction before replacing the database"
    );
    release.send(()).unwrap();
    write.await.unwrap().unwrap();
    import.await.unwrap().unwrap();
    assert_eq!(*host.kernel().snapshot().get(&VALUE), 0);
    assert!(
        old.cx_for("writer-test")
            .write(|w| {
                w.set(&VALUE, 99);
                Ok(())
            })
            .await
            .is_err()
    );
    host.restart().await;
    assert_eq!(
        *host.kernel().snapshot().get(&VALUE),
        0,
        "old writer must not leak into the restored database"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_export_protects_working_files_and_replaces_only_the_selected_file() {
    let data = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir().unwrap();
    let host = Host::start(catalog(), data.path()).await.unwrap();
    let live = data.path().join("rproxy-plugins.sqlite3");
    assert!(host.export_configuration_to(&live).is_err());
    let alias = destination.path().join("alias.sqlite3");
    std::fs::hard_link(&live, &alias).unwrap();
    assert!(host.export_configuration_to(&alias).is_err());
    #[cfg(unix)]
    {
        let link = destination.path().join("link.sqlite3");
        std::os::unix::fs::symlink(&live, &link).unwrap();
        assert!(host.export_configuration_to(&link).is_err());
    }
    let export = destination.path().join("config.sqlite3");
    std::fs::write(&export, b"previous export").unwrap();
    host.export_configuration_to(&export).unwrap();
    let bytes = std::fs::read(&export).unwrap();
    assert!(host.prepare_import("saved.sqlite3".into(), &bytes).is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&export).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(host.export_configuration_to(destination.path()).is_err());
    assert!(std::fs::read(&export).unwrap() == bytes);
    assert!(host.kernel().core_status().running());
    host.shutdown().await;
}

#[test]
fn hysteria_ipv6_and_old_subscription_settings_remain_compatible() {
    let links =
        plugin_format_uri::parse_links("hy2://secret@[2001:db8::1]:8443?sni=example.org#IPv6")
            .unwrap();
    assert_eq!(links[0].server, "2001:db8::1");
    assert_eq!(links[0].port, 8443);
    assert_eq!(links[0].name, "IPv6");
    assert_eq!(links[0].options["password"], "secret");
    let settings: plugin_subscriptions::Settings =
        serde_json::from_str(r#"{"refresh_secs":3600,"send_hwid":false}"#).unwrap();
    assert_eq!(settings.refresh_secs, 3600);
    assert!(
        serde_json::to_value(settings)
            .unwrap()
            .get("send_hwid")
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_wins_over_an_inflight_restart_and_prevents_future_launches() {
    use rproxy_kernel::{Platform, Plugin, State, categories};
    static VALUE: State<u32> = State::new("shutdown-test.value");
    static PLUGIN: Plugin = Plugin {
        id: "shutdown-test",
        name: "Shutdown test",
        description: "",
        category: categories::NETWORK,
        requires: &[],
        provides: &[],
        required: true,
        default_enabled: true,
        platforms: Platform::ALL,
        setup: |p| {
            p.editable_setting(&VALUE);
            Ok(())
        },
    };
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&PLUGIN], dir.path()).await.unwrap();
    let candidate = host
        .prepare_import(
            "before.sqlite3".into(),
            &host.export_configuration().unwrap(),
        )
        .unwrap();
    let old = host.kernel();
    let cx = old.cx_for("shutdown-test");
    let (started, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let writer = tokio::spawn(async move {
        cx.write(move |w| {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            w.set(&VALUE, 1);
            Ok(())
        })
        .await
    });
    entered.await.unwrap();
    let restarting = host.clone();
    let restart = tokio::spawn(async move { restarting.restart().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !old.closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Restart has taken current and is blocked draining the old writer.
    let stopping = host.clone();
    let shutdown = tokio::spawn(async move { stopping.shutdown().await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    release.send(()).unwrap();
    let _ = writer.await.unwrap();
    restart.await.unwrap();
    shutdown.await.unwrap();
    assert!(host.try_kernel().is_none());
    host.restart().await;
    assert!(host.import_configuration(candidate).await.is_err());
    assert!(host.try_kernel().is_none(), "shutdown must be final");
}
