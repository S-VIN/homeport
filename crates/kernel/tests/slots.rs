//! Extension points for plugins that change other plugins: `replace`,
//! `wrap_view` and `hide` edit another plugin's contributions to a zone, and
//! `migrate` runs SQL before state loads.
use rproxy_kernel::{egui, prelude::*, Host};
use std::sync::atomic::{AtomicBool, Ordering};

static ROW: Slot<()> = Slot::new("base.row", "Test zone on the base page.");
static MIGRATED: AtomicBool = AtomicBool::new(false);

static BASE: Plugin = Plugin {
    id: "base",
    name: "Base",
    description: "",
    category: categories::SERVERS,
    requires: &[],
    required: true,
    default_enabled: true,
    platforms: Platform::ALL,
    setup: |p| {
        p.fill(&ROW, "one", 0, |ui, _, _| {
            ui.label("original one");
        });
        p.fill(&ROW, "two", 1, |ui, _, _| {
            ui.label("original two");
        });
        p.fill(&ROW, "three", 2, |ui, _, _| {
            ui.label("original three");
        });
        p.page_fn(PageSpec { id: "base", title: "Base", icon: Icon::Servers, order: 10 }, |ui, cx| {
            cx.slot(ui, &ROW, &());
        });
        Ok(())
    },
};
static EXTRA: Plugin = Plugin {
    id: "extra",
    name: "Extra",
    description: "",
    category: categories::SERVERS,
    requires: &["base"],
    required: false,
    default_enabled: true,
    platforms: Platform::ALL,
    setup: |p| {
        p.replace(&ROW, "base/one", |ui, _, _| {
            ui.label("replaced one");
        });
        p.wrap_view(&ROW, "base/two", |inner, ui, cx, props| {
            ui.label("before two");
            inner(ui, cx, props);
        });
        p.hide(&ROW, "base/three");
        p.migrate(|sql| {
            sql.execute_batch("CREATE TABLE IF NOT EXISTS extra_marker (id INTEGER PRIMARY KEY)")
                .map_err(|e| Error::msg(e.to_string()))?;
            MIGRATED.store(true, Ordering::SeqCst);
            Ok(())
        });
        Ok(())
    },
};

fn texts(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
    match shape {
        egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_owned()),
        egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
        _ => {}
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_edit_other_plugins_contributions_and_migrate() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![&BASE, &EXTRA], dir.path()).await.unwrap();
    assert!(MIGRATED.load(Ordering::SeqCst), "migration ran before state loaded");

    let ctx = egui::Context::default();
    let mut session = host.session(&ctx);
    let mut shown = Vec::new();
    for _ in 0..4 {
        let out = ctx.run_ui(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900., 700.))), ..Default::default() },
            |ui| session.ui(ui),
        );
        shown.clear();
        out.shapes.iter().for_each(|s| texts(&s.shape, &mut shown));
        out.drop_without_applying_deltas();
    }
    let has = |t: &str| shown.iter().any(|s| s.contains(t));
    assert!(has("replaced one") && !has("original one"), "replace: {shown:?}");
    assert!(has("before two") && has("original two"), "wrap_view: {shown:?}");
    assert!(!has("original three"), "hide: {shown:?}");
    drop(session);
    host.shutdown().await;
}
