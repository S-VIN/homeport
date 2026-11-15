//! A saved tunnel must not prevent ordinary proxy use when authorization fails.
use rproxy_kernel::{Host, prelude::*};

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Settings {
    off: bool,
    unrelated: u32,
}
static SETTINGS: State<Settings> = State::new("tun-test.settings");
static PLUGIN: Plugin = Plugin {
    id: "tun-test",
    name: "Tunnel test",
    setup: |p| {
        p.editable_setting(&SETTINGS);
        p.core(|s| rproxy_kernel::mihomo::Fragment {
            tun: (!s.get(&SETTINGS).off)
                .then_some(rproxy_kernel::mihomo::Tun { strict_route: true }),
            ..Default::default()
        });
        Ok(())
    },
    ..Plugin::DEFAULT
};

#[tokio::test]
async fn saved_tun_authorization_failure_keeps_the_core_available_and_is_not_retried_on_writes() {
    let dir = tempfile::tempdir().unwrap();
    // Library hosts have no interactive supervisor configured.
    let host = Host::start(vec![&PLUGIN], dir.path()).await.unwrap();
    let k = host.kernel();
    let cx = k.cx_for("tun-test");
    assert!(k.core_status().running());
    assert!(
        k.core_status()
            .tun_error
            .unwrap()
            .contains("authorization is unavailable")
    );
    assert!(
        !k.snapshot().get(&SETTINGS).off,
        "saved preference is preserved"
    );
    cx.call(Update {
        state: &SETTINGS,
        change: Box::new(|s| s.unrelated += 1),
    })
    .await
    .unwrap();
    assert!(k.core_status().running());
    assert!(
        !k.core_api().configs().await.unwrap()["tun"]["enable"]
            .as_bool()
            .unwrap_or(false)
    );
    cx.call(Update {
        state: &SETTINGS,
        change: Box::new(|s| s.off = true),
    })
    .await
    .unwrap();
    assert!(k.core_status().tun_error.is_none());
    // Explicit off/on retries and returns the failure without committing "on".
    assert!(
        cx.call(Update {
            state: &SETTINGS,
            change: Box::new(|s| s.off = false)
        })
        .await
        .is_err()
    );
    assert!(k.snapshot().get(&SETTINGS).off);
    assert!(k.core_status().running());
    host.shutdown().await;
}
