//! Kernel mechanics with small test plugins: state, list ops, op chains,
//! claims, cascades, ownership, events and queued jobs.
use homeport_kernel::{Host, prelude::*};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_parents")]
struct Parent {
    id: Id,
    name: String,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_children")]
struct Child {
    id: Id,
    parent: Id,
}
static PARENTS: State<Vec<Parent>> = State::new("a.parents");
/// A second list of the same record type, in its own table.
static ARCHIVE: State<Vec<Parent>> = State::new("a.archive");
static CHILDREN: State<Vec<Child>> = State::new("b.children");
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
static RUNS: AtomicUsize = AtomicUsize::new(0);

/// Answered by `a` by default; `b` wraps it, and claims only inputs starting with "b:".
#[derive(Clone)]
struct Echo(String);
impl Op for Echo {
    type Reply = String;
}
struct Slow(u64);
impl Op for Slow {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed("slow", "Slow").queued())
    }
}
struct AddChild(Id);
impl Op for AddChild {
    type Reply = ();
}
struct Trespass;
impl Op for Trespass {
    type Reply = ();
}

static A: Plugin = Plugin {
    id: "a",
    name: "A",
    category: categories::SERVERS,
    required: true,
    migrations: &[(
        "001-tables.sql",
        "CREATE TABLE test_parents (id TEXT NOT NULL PRIMARY KEY, name TEXT NOT NULL) STRICT;
         CREATE TABLE test_parents_archive (id TEXT NOT NULL PRIMARY KEY, name TEXT NOT NULL) STRICT;",
    )],
    setup: |p| {
        p.list(&PARENTS).validate(|_, item, _, others| {
            item.name = item.name.trim().to_owned();
            if item.name.is_empty() || others.iter().any(|o| o.name == item.name) {
                bail!("bad name");
            }
            Ok(())
        });
        p.list(&ARCHIVE).table("test_parents_archive");
        p.handle(|_, Echo(s): Echo| async move { Ok(format!("a({s})")) });
        p.handle(|_, Slow(ms): Slow| async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        p.on(|cx: Cx, e: Added<Parent>| async move {
            // Observers run after commit and see the new state.
            let visible = cx.get(&PARENTS).iter().any(|x| x.id == e.item.id);
            SEEN.lock()
                .unwrap()
                .push(format!("added {} visible={visible}", e.item.name));
            Ok(())
        });
        Ok(())
    },
    ..Plugin::DEFAULT
};
static B: Plugin = Plugin {
    id: "b",
    name: "B",
    category: categories::SERVERS,
    requires: &[&A],
    migrations: &[(
        "001-children.sql",
        "CREATE TABLE test_children (id TEXT NOT NULL PRIMARY KEY, parent TEXT NOT NULL) STRICT;",
    )],
    setup: |p| {
        p.collection(&CHILDREN);
        p.cascade(&PARENTS, &CHILDREN, |c| c.parent);
        p.wrap(|cx: Cx, op: Echo, next: Next<Echo>| async move {
            Ok(format!("b[{}]", next.run(&cx, op).await?))
        })
        .claim(|_, op| op.0.starts_with("b:").then_some(Claim::content(1)));
        p.handle(|cx: Cx, AddChild(parent): AddChild| async move {
            cx.write(|w| {
                w.update(&CHILDREN, |c| {
                    c.push(Child {
                        id: Id::random(),
                        parent,
                    })
                });
                Ok(())
            })
            .await
        });
        p.handle(|cx: Cx, _: Trespass| async move {
            cx.write(|w| {
                w.update(&PARENTS, |p| p.clear());
                Ok(())
            })
            .await
        });
        Ok(())
    },
    ..Plugin::DEFAULT
};

#[tokio::test(flavor = "multi_thread")]
async fn kernel_mechanics() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start_enabled(vec![&A, &B], dir.path()).await.unwrap();
    let k = host.kernel();
    let a = k.cx_for("a");

    // List ops with validation and normalization.
    let first = a
        .call(Add::new(
            &PARENTS,
            Parent {
                id: Id::random(),
                name: "  one ".into(),
            },
        ))
        .await
        .unwrap();
    assert!(
        a.call(Add::new(
            &PARENTS,
            Parent {
                id: Id::random(),
                name: "one".into()
            }
        ))
        .await
        .is_err()
    );
    let second = a
        .call(Add::new(
            &PARENTS,
            Parent {
                id: Id::random(),
                name: "two".into(),
            },
        ))
        .await
        .unwrap();
    a.call(Move::new(&PARENTS, second, true)).await.unwrap();
    let names: Vec<_> = k
        .snapshot()
        .get(&PARENTS)
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, ["two", "one"]);
    k.idle().await;
    assert!(
        SEEN.lock()
            .unwrap()
            .contains(&"added one visible=true".to_string())
    );

    // Two lists of one record type stay apart: ops name their list.
    a.call(Add::new(
        &ARCHIVE,
        Parent {
            id: Id::random(),
            name: "old".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(k.snapshot().get(&ARCHIVE).len(), 1);
    assert_eq!(k.snapshot().get(&PARENTS).len(), 2);

    // Op chains: the wrapper only runs for inputs it claims.
    assert_eq!(a.call(Echo("x".into())).await.unwrap(), "a(x)");
    assert_eq!(a.call(Echo("b:y".into())).await.unwrap(), "b[a(b:y)]");

    // Ownership: another plugin's write is refused with an error, not a crash.
    let err = k.cx_for("b").call(Trespass).await.unwrap_err();
    assert_eq!(err.code, "bug");
    assert_eq!(k.snapshot().get(&PARENTS).len(), 2);

    // Cascades remove children of a removed parent in the same transaction.
    let b = k.cx_for("b");
    b.call(AddChild(first)).await.unwrap();
    b.call(AddChild(second)).await.unwrap();
    a.call(Remove::new(&PARENTS, first)).await.unwrap();
    let children = k.snapshot().get(&CHILDREN);
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].parent, second);

    // Queued jobs: requests during a run lead to exactly one follow-up run.
    RUNS.store(0, Ordering::SeqCst);
    a.spawn(Slow(150)).unwrap();
    a.spawn(Slow(10)).unwrap();
    a.spawn(Slow(10)).unwrap();
    k.idle().await;
    assert_eq!(RUNS.load(Ordering::SeqCst), 2);

    // Persistence: order and cascades survive a restart.
    host.restart().await;
    let k = host.kernel();
    let names: Vec<_> = k
        .snapshot()
        .get(&PARENTS)
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, ["two"]);
    assert_eq!(k.snapshot().get(&ARCHIVE)[0].name, "old");
    assert_eq!(k.snapshot().get(&CHILDREN).len(), 1);
    host.shutdown().await;
}

/// A failed SQLite commit leaves the state as it was: the write fails as a
/// whole, nothing half-saved shows up.
#[tokio::test(flavor = "multi_thread")]
async fn failed_commit_keeps_the_state() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start_enabled(vec![&A, &B], dir.path()).await.unwrap();
    let a = host.kernel().cx_for("a");
    let db = homeport_kernel::store::rusqlite::Connection::open(
        dir.path().join("homeport-plugins.sqlite3"),
    )
    .unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse BEFORE INSERT ON test_parents_archive
         BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
    )
    .unwrap();
    let item = Parent {
        id: Id::random(),
        name: "kept".into(),
    };
    let error = a.call(Add::new(&ARCHIVE, item.clone())).await.unwrap_err();
    assert!(error.message.contains("disk full"), "{}", error.message);
    assert!(host.kernel().snapshot().get(&ARCHIVE).is_empty());

    db.execute_batch("DROP TRIGGER refuse").unwrap();
    a.call(Add::new(&ARCHIVE, item)).await.unwrap();
    assert_eq!(host.kernel().snapshot().get(&ARCHIVE).len(), 1);
    host.shutdown().await;
}
