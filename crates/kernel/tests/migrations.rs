//! Saved data across versions of a plugin: a new required column with a
//! default, migrations that run once, and a setting that no longer reads.
use rproxy_kernel::{Host, prelude::*};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_notes")]
struct Note {
    id: Id,
    text: String,
}
/// The same table one version later: `stars` is new and required.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_notes")]
struct NoteV2 {
    id: Id,
    text: String,
    #[column(default = "3")]
    stars: u32,
}
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Conf {
    limit: u32,
}

/// The same rows one more version later, in a renamed table.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_cards")]
struct Card {
    id: Id,
    text: String,
    stars: u32,
}
static NOTES: State<Vec<Note>> = State::new("m.notes");
static CARDS: State<Vec<Card>> = State::new("m.notes");
static NOTES_V2: State<Vec<NoteV2>> = State::new("m.notes");
static CONF: State<Conf> = State::new("m.conf");
static RUNS: AtomicUsize = AtomicUsize::new(0);

fn plugin(setup: fn(&mut Setup) -> Result<()>) -> Plugin {
    Plugin {
        id: "m",
        name: "Migrations",
        description: "",
        category: categories::SERVERS,
        requires: &[],
        required: true,
        default_enabled: true,
        platforms: Platform::ALL,
        setup,
    }
}
static V1: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(|p| {
        p.list(&NOTES);
        p.setting(&CONF);
        Ok(())
    })
});
static V2: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(|p| {
        p.list(&NOTES_V2);
        p.setting(&CONF);
        p.migrate(1, |sql| {
            RUNS.fetch_add(1, Ordering::SeqCst);
            sql.execute("UPDATE test_notes SET text = text || '!'", [])?;
            Ok(())
        });
        // The new column already exists (with its default): fill it from old data.
        p.migrate(2, |sql| {
            sql.execute("UPDATE test_notes SET stars = length(text) + 1", [])?;
            Ok(())
        });
        Ok(())
    })
});

static V3: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(|p| {
        p.list(&CARDS);
        p.setting(&CONF);
        p.migrate(3, |sql| {
            sql.execute("ALTER TABLE test_notes RENAME TO test_cards", [])?;
            Ok(())
        });
        Ok(())
    })
});

#[tokio::test(flavor = "multi_thread")]
async fn saved_data_survives_plugin_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&*V1], dir.path()).await.unwrap();
    let id = host
        .kernel()
        .cx_for("m")
        .call(Add::new(
            &NOTES,
            Note {
                id: Id::random(),
                text: "hi".into(),
            },
        ))
        .await
        .unwrap();
    host.shutdown().await;
    drop(host);

    // A value the new version can't read: the app still starts, with the default.
    let db =
        rproxy_kernel::store::rusqlite::Connection::open(dir.path().join("rproxy-plugins.sqlite3"))
            .unwrap();
    db.execute(
        "INSERT OR REPLACE INTO kernel_settings (id, json) VALUES ('m.conf', 'not json')",
        [],
    )
    .unwrap();
    drop(db);

    for _ in 0..2 {
        let host = Host::start(vec![&*V2], dir.path()).await.unwrap();
        let k = host.kernel();
        let notes = k.snapshot().get(&NOTES_V2);
        assert_eq!(
            *notes,
            [NoteV2 {
                id,
                text: "hi!".into(),
                stars: 4
            }],
            "migrations run in order and fill the new column"
        );
        assert_eq!(*k.snapshot().get(&CONF), Conf::default());
        host.shutdown().await;
    }
    assert_eq!(RUNS.load(Ordering::SeqCst), 1, "a migration runs once");

    // A renamed table keeps its rows.
    let host = Host::start(vec![&*V3], dir.path()).await.unwrap();
    let cards = host.kernel().snapshot().get(&CARDS);
    assert_eq!(cards.len(), 1, "the rename keeps the rows");
    assert_eq!(cards[0].text, "hi!");
    host.shutdown().await;
    drop(host);

    // A fresh install is already on the newest schema: nothing to migrate.
    let fresh = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&*V2], fresh.path()).await.unwrap();
    host.shutdown().await;
    assert_eq!(
        RUNS.load(Ordering::SeqCst),
        1,
        "a fresh install skips migrations"
    );
}
