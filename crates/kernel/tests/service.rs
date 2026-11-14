//! `p.service`: a run follows its spec, and the old run is fully stopped (its
//! guards dropped, `on_stop` finished) before the next one starts and before
//! shutdown returns.
use rproxy_kernel::{Host, prelude::*};
use std::{sync::Mutex, time::Duration};

static PORT: State<u16> = State::new("svc.port");
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Logs its stop when dropped, slowly, like restoring a system setting.
struct Guard(u16);
impl Drop for Guard {
    fn drop(&mut self) {
        std::thread::sleep(Duration::from_millis(200));
        LOG.lock().unwrap().push(format!("stop {}", self.0));
    }
}

static SVC: Plugin = Plugin {
    id: "svc",
    name: "Service",
    required: true,
    setup: |p| {
        p.editable_setting(&PORT);
        p.service(
            "run",
            |snap| Some(*snap.get(&PORT)).filter(|p| *p != 0),
            |_cx, port: u16| async move {
                LOG.lock().unwrap().push(format!("start {port}"));
                let _guard = Guard(port);
                std::future::pending::<()>().await;
                Ok(())
            },
        )
        .on_stop(|_cx, port: u16| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            LOG.lock().unwrap().push(format!("clean {port}"));
            Ok(())
        });
        Ok(())
    },
    ..Plugin::DEFAULT
};

async fn until(what: &str, ok: impl Fn(&[String]) -> bool) {
    for _ in 0..100 {
        if ok(&LOG.lock().unwrap()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}: {:?}", LOG.lock().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_restarts_only_after_the_old_run_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&SVC], dir.path()).await.unwrap();
    let cx = host.kernel().cx_for("svc");
    cx.call(Set {
        state: &PORT,
        value: 1,
    })
    .await
    .unwrap();
    until("first run", |l| l.contains(&"start 1".into())).await;
    cx.call(Set {
        state: &PORT,
        value: 2,
    })
    .await
    .unwrap();
    until("second run", |l| l.contains(&"start 2".into())).await;
    host.shutdown().await;
    assert_eq!(
        *LOG.lock().unwrap(),
        [
            "start 1", "stop 1", "clean 1", "start 2", "stop 2", "clean 2"
        ],
        "stops run before the next start and before shutdown returns"
    );
}
