//! Background work of a plugin: the newest request wins (`JobSpec::latest`,
//! `.queued()` observers), `idle()` waits for follow-ups, and a crashing
//! service restarts with `restart_on_error`.
use rproxy_kernel::{Host, prelude::*};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

static LOADED: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static SEEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static SERVICE_RUNS: AtomicUsize = AtomicUsize::new(0);

/// A slow load; a newer one replaces it.
struct Load(u32);
impl Op for Load {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Load").latest())
    }
}
#[derive(Clone)]
struct Tick(u32);
/// Runs until cancelled; a second one waits in the queue.
struct Hold;
impl Op for Hold {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Hold").queued())
    }
}

static WORK: Plugin = Plugin {
    id: "work",
    name: "Work",
    setup: |p| {
        p.handle(|cx: Cx, Load(n): Load| async move {
            cx.or_cancel(tokio::time::sleep(Duration::from_millis(100)))
                .await?;
            LOADED.lock().unwrap().push(n);
            Ok(())
        });
        p.handle(|cx: Cx, _: Hold| async move { cx.or_cancel(std::future::pending::<()>()).await });
        p.on(|_, Tick(n): Tick| async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            SEEN.lock().unwrap().push(n);
            Ok(())
        })
        .queued();
        p.service(
            "flaky",
            |_| Some(()),
            |_, ()| async move {
                if SERVICE_RUNS.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("first run crashes");
                }
                std::future::pending().await
            },
        )
        .restart_on_error();
        Ok(())
    },
    ..Plugin::DEFAULT
};

#[tokio::test(flavor = "multi_thread")]
async fn newest_request_wins_and_idle_waits_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&WORK], dir.path()).await.unwrap();
    let k = host.kernel();
    let cx = k.cx_for("work");

    for n in 1..=3 {
        cx.spawn(Load(n)).unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cx.emit(Tick(1));
    tokio::time::sleep(Duration::from_millis(10)).await;
    cx.emit(Tick(2));
    cx.emit(Tick(3));
    k.idle().await;
    assert_eq!(
        *LOADED.lock().unwrap(),
        [3],
        "1 is cancelled, 2 replaced by 3"
    );
    assert_eq!(*SEEN.lock().unwrap(), [1, 3], "while 1 runs, 3 replaces 2");

    // The crash is reported and the service runs again.
    tokio::time::timeout(Duration::from_secs(5), async {
        while SERVICE_RUNS.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the service restarts after a crash");
    // A queued follow-up never holds up shutdown.
    cx.spawn(Hold).unwrap();
    cx.spawn(Hold).unwrap();
    tokio::time::timeout(Duration::from_secs(5), host.shutdown())
        .await
        .expect("shutdown with a queued job");
}
