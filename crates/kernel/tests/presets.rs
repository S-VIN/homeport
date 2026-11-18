//! Presets: a SQL script that starts a new installation.
use homeport_kernel::{Host, prelude::*};

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Settings {
    port: u16,
    domain: String,
    unset: String,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "preset_test_items")]
struct Item {
    id: Id,
    name: String,
}
static SETTINGS: State<Settings> = State::new("preset-test.settings");
static ITEMS: State<Vec<Item>> = State::new("preset-test.items");
static PLUGIN: Plugin = Plugin {
    id: "preset-test",
    name: "Preset test",
    migrations: &[(
        "001-items.sql",
        "CREATE TABLE preset_test_items (id TEXT NOT NULL PRIMARY KEY, name TEXT NOT NULL) STRICT;",
    )],
    setup: |p| {
        p.setting(&SETTINGS);
        p.list(&ITEMS);
        Ok(())
    },
    ..Plugin::DEFAULT
};

fn preset(files: &tempfile::TempDir, name: &str, sql: &str) -> std::path::PathBuf {
    let path = files.path().join(name);
    std::fs::write(&path, sql).unwrap();
    path
}

#[tokio::test]
async fn without_a_preset_optional_plugins_are_off() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&PLUGIN], dir.path()).await.unwrap();
    assert!(host.kernel().plugins().is_empty());
    host.shutdown().await;
}

#[tokio::test]
async fn a_preset_starts_only_a_new_installation() {
    let files = tempfile::tempdir().unwrap();
    let first = preset(
        &files,
        "first.sql",
        "INSERT INTO kernel_plugins (id, enabled) VALUES ('preset-test', 1);
         -- Only what differs from the defaults; env() reads the environment.
         INSERT INTO kernel_settings (id, json) VALUES ('preset-test.settings',
           json_object('port', 8443, 'domain', env('PATH'), 'unset', env('HOMEPORT_PRESET_TEST_UNSET')));
         INSERT INTO preset_test_items (id, name) VALUES (new_id(), 'second'), (new_id(), 'first');",
    );
    let second = preset(
        &files,
        "second.sql",
        "INSERT INTO kernel_settings (id, json) VALUES ('preset-test.settings', '{\"port\": 1}');",
    );
    let dir = tempfile::tempdir().unwrap();
    let expected = Settings {
        port: 8443,
        domain: std::env::var("PATH").unwrap(),
        unset: String::new(),
    };
    let host = Host::start_from(vec![&PLUGIN], dir.path(), Some(&first))
        .await
        .unwrap();
    let snap = host.kernel().snapshot();
    assert_eq!(*snap.get(&SETTINGS), expected);
    let names: Vec<_> = snap.get(&ITEMS).iter().map(|i| i.name.clone()).collect();
    assert_eq!(names, ["second", "first"], "in the order of the script");
    host.shutdown().await;
    drop(host);
    let host = Host::start_from(vec![&PLUGIN], dir.path(), Some(&second))
        .await
        .unwrap();
    assert_eq!(
        *host.kernel().snapshot().get(&SETTINGS),
        expected,
        "an existing installation ignores the preset"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn a_failing_preset_leaves_no_installation() {
    let files = tempfile::tempdir().unwrap();
    let broken = preset(
        &files,
        "broken.sql",
        "INSERT INTO no_such_table VALUES (1);",
    );
    let dir = tempfile::tempdir().unwrap();
    let error = Host::start_from(vec![&PLUGIN], dir.path(), Some(&broken))
        .await
        .err()
        .expect("the preset fails");
    assert!(error.message.contains("no_such_table"), "{}", error.message);
    assert!(!dir.path().join("homeport-plugins.sqlite3").exists());
}
