//! Saved data across versions of a plugin: SQL migration scripts run once
//! each in name order, the tables must match their types, and a setting that
//! no longer reads falls back to its default.
use homeport_kernel::{Host, prelude::*};

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
    stars: u32,
}
/// The same rows one more version later, in a renamed table.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_cards")]
struct Card {
    id: Id,
    text: String,
    stars: u32,
}
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Conf {
    limit: u32,
}
static NOTES: State<Vec<Note>> = State::new("m.notes");
static NOTES_V2: State<Vec<NoteV2>> = State::new("m.notes");
static CARDS: State<Vec<Card>> = State::new("m.notes");
static CONF: State<Conf> = State::new("m.conf");

const CREATE: (&str, &str) = (
    "001-notes.sql",
    "CREATE TABLE test_notes (id TEXT NOT NULL PRIMARY KEY, text TEXT NOT NULL) STRICT;",
);
const EXCLAIM: (&str, &str) = (
    "002-exclaim.sql",
    "UPDATE test_notes SET text = text || '!';",
);
const STARS: (&str, &str) = (
    "003-stars.sql",
    "ALTER TABLE test_notes ADD COLUMN stars INT NOT NULL DEFAULT 0;
     UPDATE test_notes SET stars = length(text) + 1;",
);
const RENAME: (&str, &str) = (
    "004-cards.sql",
    "ALTER TABLE test_notes RENAME TO test_cards;",
);

fn plugin(
    migrations: &'static [(&'static str, &'static str)],
    setup: fn(&mut Setup) -> Result<()>,
) -> Plugin {
    Plugin {
        id: "m",
        name: "Migrations",
        category: categories::SERVERS,
        required: true,
        migrations,
        setup,
        ..Plugin::DEFAULT
    }
}
static V1: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(&[CREATE], |p| {
        p.list(&NOTES);
        p.setting(&CONF);
        Ok(())
    })
});
static V2: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(&[CREATE, EXCLAIM, STARS], |p| {
        p.list(&NOTES_V2);
        p.setting(&CONF);
        Ok(())
    })
});
static V3: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(&[CREATE, EXCLAIM, STARS, RENAME], |p| {
        p.list(&CARDS);
        p.setting(&CONF);
        Ok(())
    })
});
/// A new field without a script for it.
static FORGOTTEN: std::sync::LazyLock<Plugin> = std::sync::LazyLock::new(|| {
    plugin(&[CREATE], |p| {
        p.list(&NOTES_V2);
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
    let db = homeport_kernel::store::rusqlite::Connection::open(
        dir.path().join("homeport-plugins.sqlite3"),
    )
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
        assert_eq!(
            *k.snapshot().get(&NOTES_V2),
            [NoteV2 {
                id,
                text: "hi!".into(),
                stars: 4
            }],
            "new scripts run once, in name order"
        );
        assert_eq!(*k.snapshot().get(&CONF), Conf::default());
        host.shutdown().await;
    }

    // A renamed table keeps its rows.
    let host = Host::start(vec![&*V3], dir.path()).await.unwrap();
    let cards = host.kernel().snapshot().get(&CARDS);
    assert_eq!(cards.len(), 1, "the rename keeps the rows");
    assert_eq!(cards[0].text, "hi!");
    host.shutdown().await;
    drop(host);

    // A new installation runs every script from the first.
    let fresh = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&*V3], fresh.path()).await.unwrap();
    assert!(host.kernel().snapshot().get(&CARDS).is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_that_differs_from_its_type_stops_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let error = Host::start(vec![&*FORGOTTEN], dir.path())
        .await
        .err()
        .expect("the table lacks a column");
    assert!(
        error.message.contains("test_notes lacks column stars"),
        "{}",
        error.message
    );
}
