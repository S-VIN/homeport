//! Live update of the mihomo core from GitHub (network): `cargo test -p rproxy-kernel --test core_update -- --ignored`.
use rproxy_kernel::{Host, Kernel};
use std::time::Duration;

async fn finished(k: &Kernel) {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let board = k.board();
            if let Some(r) = board.runs.iter().find(|r| r.key == "kernel/core-update") {
                if !r.phase.active() {
                    assert!(r.error.is_none(), "{:?}", r.error);
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("update job finished");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads the latest mihomo release"]
async fn installs_the_latest_core_and_goes_back() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![], dir.path()).await.unwrap();
    let k = host.kernel();
    let before = k.core_status().version.unwrap();
    k.check_core_update().unwrap();
    finished(&k).await;
    let latest = k.core_updates().latest.expect("a release");
    k.install_core_update().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    finished(&k).await;
    assert!(k.installed_core().is_some());
    let after = k.core_status().version.unwrap();
    assert!(after.contains(&latest.version), "{before} → {after}");
    k.remove_core_update().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    finished(&k).await;
    assert!(k.installed_core().is_none());
    assert_eq!(k.core_status().version.unwrap(), before);
    host.shutdown().await;
}
