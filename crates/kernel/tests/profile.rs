//! A profile built for another installation loads there like its own data.
use rproxy_kernel::{Host, configuration::Profile, prelude::*};

#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct Settings {
    port: u16,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "profile_test_items")]
struct Item {
    id: Id,
    name: String,
}
static SETTINGS: State<Settings> = State::new("profile-test.settings");
static ITEMS: State<Vec<Item>> = State::new("profile-test.items");
static PLUGIN: Plugin = Plugin {
    id: "profile-test",
    name: "Profile test",
    setup: |p| {
        p.setting(&SETTINGS);
        p.list(&ITEMS);
        Ok(())
    },
    ..Plugin::DEFAULT
};

#[tokio::test]
async fn a_built_profile_starts_another_installation_with_its_settings_and_lists() {
    let items: Vec<Item> = ["second", "first"]
        .map(|name| Item {
            id: Id::random(),
            name: name.into(),
        })
        .into();
    let bytes = Profile::new()
        .unwrap()
        .enable(PLUGIN.id)
        .unwrap()
        .setting(&SETTINGS, &Settings { port: 8443 })
        .unwrap()
        .list(&ITEMS, &items)
        .unwrap()
        .finish()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rproxy-plugins.sqlite3"), bytes).unwrap();
    let host = Host::start(vec![&PLUGIN], dir.path()).await.unwrap();
    let snap = host.kernel().snapshot();
    assert_eq!(snap.get(&SETTINGS).port, 8443);
    assert_eq!(*snap.get(&ITEMS), items, "in the given order");
    host.shutdown().await;
}
