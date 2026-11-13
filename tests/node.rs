//! A VLESS node and a client on one machine, both with the real bundled mihomo:
//! VLESS + REALITY + XHTTP end to end, the camouflage site for probes without a
//! key, and the exit-server chain (client → entry → node → internet).
use plugin_exit_server as exit;
use plugin_inbounds::{Inbound, INBOUNDS};
use plugin_routing::{Route, Rule};
use plugin_servers::{Select, SERVERS};
use plugin_subscriptions::Source;
use plugin_vless_server as vless;
use rproxy_kernel::{list::Add, list::Save, Host, Id, Kernel, Update};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn until(k: &Kernel, what: &str, ok: impl Fn(&Kernel) -> bool) {
    let mut changes = k.changes();
    tokio::time::timeout(Duration::from_secs(20), async {
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

/// HTTP origin that answers every request with `body`.
async fn origin(body: &'static str) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0; 4096];
                if s.read(&mut buf).await.unwrap_or(0) > 0 {
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

/// A real HTTP CONNECT proxy (the entry server); counts tunnels.
async fn connect_proxy() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tunnels = Arc::new(AtomicUsize::new(0));
    let count = tunnels.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let count = count.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let Some(target) = head
                    .strip_prefix("CONNECT ")
                    .and_then(|r| r.split_whitespace().next())
                else {
                    return;
                };
                let Ok(mut upstream) = TcpStream::connect(target).await else {
                    return;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let _ = s
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await;
                let _ = tokio::io::copy_bidirectional(&mut s, &mut upstream).await;
            });
        }
    });
    (port, tunnels)
}

/// Host with the default listener moved to a free port (tests run in parallel).
async fn start(dir: &std::path::Path, enable: &[&str]) -> (Arc<Host>, Inbound) {
    let host = Host::start(rproxy::catalog(), dir).await.unwrap();
    if !enable.is_empty() {
        for id in enable {
            host.kernel().set_plugin(id, true).unwrap();
        }
        host.restart().await;
    }
    let k = host.kernel();
    until(&k, "seeded listener", |k| {
        !k.snapshot().get(&INBOUNDS).is_empty()
    })
    .await;
    let mut inbound = k.snapshot().get(&INBOUNDS)[0].clone();
    inbound.port = free_port().await;
    inbound.enabled = true;
    k.cx_for("inbounds")
        .call(Save(inbound.clone()))
        .await
        .unwrap();
    (host, inbound)
}

/// An exit node: VLESS server on a free port, client traffic leaves directly.
async fn node(dir: &std::path::Path) -> (Arc<Host>, String, u16) {
    let (host, _) = start(dir, &["vless-server"]).await;
    let k = host.kernel();
    let cx = k.cx_for("vless-server");
    let port = free_port().await;
    cx.call(Update::<vless::Settings>(Box::new(move |s| {
        s.domain = "node.test".into();
        s.port = port;
        s.redirect_http = false;
    })))
    .await
    .unwrap();
    cx.call(vless::SetEnabled(true)).await.unwrap();
    k.cx_for("routing")
        .call(Add::new(Rule {
            id: Id::random(),
            pattern: "*".into(),
            action: Route::Direct,
        }))
        .await
        .unwrap();
    until(&k, "camouflage site", |k| {
        matches!(
            *k.snapshot().live(&vless::CERTIFICATE),
            vless::site::Certificate::SelfSigned(_)
        )
    })
    .await;
    assert!(
        k.core_status().listener_errors.is_empty(),
        "{:?}",
        k.core_status().listener_errors
    );
    let snap = k.snapshot();
    // The domain has no DNS record here: dial the node by address, keep the SNI.
    let link =
        vless::link(&snap, &snap.get(&vless::CLIENTS)[0]).replace("@node.test:", "@127.0.0.1:");
    (host, link, port)
}

fn via(listener: u16) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{listener}")).unwrap())
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn vless_node_serves_clients_and_disguises_itself() {
    let (node_dir, client_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (node, link, node_port) = node(node_dir.path()).await;
    assert!(
        link.starts_with("vless://")
            && link.contains("security=reality")
            && link.contains("type=xhttp")
    );

    // Without a client key the node is an ordinary HTTPS website.
    let probe = reqwest::Client::builder()
        .no_proxy()
        .tls_danger_accept_invalid_certs(true)
        .resolve("node.test", SocketAddr::from(([127, 0, 0, 1], node_port)))
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let page = probe
        .get(format!("https://node.test:{node_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status().as_u16(), 200);
    assert!(page.text().await.unwrap().contains("Notes on photography"));

    // A client imports the link and reaches the internet through the node.
    let (client, inbound) = start(client_dir.path(), &[]).await;
    let k = client.kernel();
    k.cx_for("subscriptions")
        .call(Add::new(Source::new(link)))
        .await
        .unwrap();
    until(&k, "server from link", |k| {
        !k.snapshot().get(&SERVERS).is_empty()
    })
    .await;
    let server = k.snapshot().get(&SERVERS)[0].id;
    k.cx_for("servers")
        .call(Select {
            id: Some(server),
            auto: false,
        })
        .await
        .unwrap();
    let target = origin("from origin").await;
    let body = via(inbound.port)
        .get(format!("http://127.0.0.1:{target}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "from origin");

    client.shutdown().await;
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_server_chains_after_the_selected_server_and_fails_closed() {
    let (node_dir, client_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (node, link, _) = node(node_dir.path()).await;
    let (entry, tunnels) = connect_proxy().await;

    let (client, inbound) = start(client_dir.path(), &[]).await;
    let k = client.kernel();
    // The entry: an ordinary server from a subscription.
    let body =
        format!("proxies:\n  - {{name: Entry, type: http, server: 127.0.0.1, port: {entry}}}");
    let provider = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0; 4096];
                    if s.read(&mut buf).await.unwrap_or(0) > 0 {
                        let msg = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        let _ = s.write_all(msg.as_bytes()).await;
                    }
                });
            }
        });
        port
    };
    k.cx_for("subscriptions")
        .call(Add::new(Source::new(format!(
            "http://127.0.0.1:{provider}/sub"
        ))))
        .await
        .unwrap();
    until(&k, "entry server", |k| {
        !k.snapshot().get(&SERVERS).is_empty()
    })
    .await;
    let server = k.snapshot().get(&SERVERS)[0].id;
    k.cx_for("servers")
        .call(Select {
            id: Some(server),
            auto: false,
        })
        .await
        .unwrap();

    // The exit: the node's link, added on the Exit server page.
    k.cx_for("exit-server")
        .call(Add::new(exit::Exit {
            id: Id::random(),
            name: String::new(),
            link,
        }))
        .await
        .unwrap();
    until(&k, "exit in use", |k| exit::active(&k.snapshot()).is_some()).await;
    let target = origin("from origin").await;
    let before = tunnels.load(Ordering::SeqCst);
    let body = via(inbound.port)
        .get(format!("http://127.0.0.1:{target}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "from origin");
    assert!(
        tunnels.load(Ordering::SeqCst) > before,
        "the exit was dialed through the entry"
    );
    let ms = k.cx_for("exit-server").call(exit::CheckExit).await.unwrap();
    assert!(ms > 0);

    // No entry: traffic stops instead of going to the exit directly.
    k.cx_for("servers")
        .call(Select {
            id: None,
            auto: false,
        })
        .await
        .unwrap();
    assert!(via(inbound.port)
        .get(format!("http://127.0.0.1:{target}/"))
        .send()
        .await
        .is_err_or_not_ok());

    // Exit off: one hop again.
    k.cx_for("servers")
        .call(Select {
            id: Some(server),
            auto: false,
        })
        .await
        .unwrap();
    k.cx_for("exit-server").call(exit::Use(None)).await.unwrap();
    let before = tunnels.load(Ordering::SeqCst);
    let r = via(inbound.port)
        .get(format!("http://127.0.0.1:{target}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.text().await.unwrap(), "from origin");
    assert!(tunnels.load(Ordering::SeqCst) > before);

    client.shutdown().await;
    node.shutdown().await;
}

trait Failed {
    fn is_err_or_not_ok(&self) -> bool;
}
impl Failed for reqwest::Result<reqwest::Response> {
    fn is_err_or_not_ok(&self) -> bool {
        !self.as_ref().is_ok_and(|r| r.status().is_success())
    }
}
