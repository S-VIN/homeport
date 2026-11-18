//! The shipped presets (presets/*.sql) start a new installation as described there.
use homeport_kernel::Host;
use plugin_routing::{RULES, Route};
use std::path::Path;

async fn start(preset: &str, dir: &Path) -> std::sync::Arc<Host> {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("presets")
        .join(preset);
    Host::start_from(homeport::catalog(), dir, Some(&file))
        .await
        .unwrap()
}

fn optional(host: &Host) -> Vec<&'static str> {
    let mut ids: Vec<_> = host
        .kernel()
        .plugins()
        .iter()
        .filter(|p| !p.required)
        .map(|p| p.id)
        .collect();
    ids.sort();
    ids
}

fn rules(host: &Host) -> Vec<(String, Route)> {
    host.kernel()
        .snapshot()
        .get(&RULES)
        .iter()
        .map(|r| (r.pattern.clone(), r.action))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_preset_is_a_vless_exit() {
    let dir = tempfile::tempdir().unwrap();
    let host = start("server.sql", dir.path()).await;
    assert_eq!(
        optional(&host),
        ["connections", "logs", "statistics", "vless-inbound"]
    );
    let vless = host.kernel().snapshot().get(&plugin_vless_inbound::CONFIG);
    assert!(vless.enabled && !vless.redirect_http && vless.port == 443);
    assert_eq!(rules(&host).last().unwrap(), &("*".into(), Route::Direct));
    assert!(rules(&host).contains(&("geoip:private".into(), Route::Block)));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_preset_proxies_with_tests_and_local_routes() {
    let dir = tempfile::tempdir().unwrap();
    let host = start("client.sql", dir.path()).await;
    assert_eq!(
        optional(&host),
        [
            "dns",
            "health-checks",
            "local-proxy",
            "name-filters",
            "statistics"
        ]
    );
    let snap = host.kernel().snapshot();
    assert_eq!(snap.get(&plugin_health_checks::TESTS).len(), 4);
    assert_eq!(snap.get(&plugin_servers::TESTING).check_secs, 900);
    assert!(snap.get(&plugin_servers::auto::AUTO).enabled);
    assert_eq!(snap.get(&plugin_name_filters::FILTERS).len(), 7);
    assert!(rules(&host).contains(&("geoip:ru".into(), Route::Direct)));
    host.shutdown().await;
}
