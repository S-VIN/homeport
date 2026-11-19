//! Extension points for plugins that change other plugins: `replace`,
//! `wrap_view` and `hide` edit another plugin's contributions to a zone.
use homeport_kernel::{Host, egui, prelude::*};

static ROW: Slot<()> = Slot::new("base.row");

static BASE: Plugin = Plugin {
    id: "base",
    name: "Base",
    category: Category::new("servers", "Servers"),
    required: true,
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
        p.page_fn(
            PageSpec {
                title: "Base",
                icon: Icon::HardDrives,
                order: 10,
            },
            |ui, cx| {
                cx.slot(ui, &ROW, &());
            },
        );
        Ok(())
    },
    ..Plugin::DEFAULT
};
static EXTRA: Plugin = Plugin {
    id: "extra",
    name: "Extra",
    category: Category::new("servers", "Servers"),
    requires: &[&BASE],
    setup: |p| {
        p.replace(&ROW, "base/one", |ui, _, _| {
            ui.label("replaced one");
        });
        p.wrap_view(&ROW, "base/two", |inner, ui, cx, props| {
            ui.label("before two");
            inner(ui, cx, props);
        });
        p.hide(&ROW, "base/three");
        Ok(())
    },
    ..Plugin::DEFAULT
};

fn texts(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
    match shape {
        egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_owned()),
        egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
        _ => {}
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_edit_other_plugins_contributions() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start_enabled(vec![&BASE, &EXTRA], dir.path())
        .await
        .unwrap();

    let ctx = egui::Context::default();
    let mut session = host.session(&ctx);
    let mut shown = Vec::new();
    for _ in 0..4 {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900., 700.),
                )),
                ..Default::default()
            },
            |ui| session.ui(ui),
        );
        shown.clear();
        out.shapes.iter().for_each(|s| texts(&s.shape, &mut shown));
        out.drop_without_applying_deltas();
    }
    let has = |t: &str| shown.iter().any(|s| s.contains(t));
    assert!(
        has("replaced one") && !has("original one"),
        "replace: {shown:?}"
    );
    assert!(
        has("before two") && has("original two"),
        "wrap_view: {shown:?}"
    );
    assert!(!has("original three"), "hide: {shown:?}");
    drop(session);
    host.shutdown().await;
}

/// Fills a zone of a plugin it does not require.
static STRAY: Plugin = Plugin {
    id: "stray",
    name: "Stray",
    category: Category::new("servers", "Servers"),
    setup: |p| {
        p.fill(&ROW, "x", 0, |_, _, _| {});
        Ok(())
    },
    ..Plugin::DEFAULT
};

#[tokio::test(flavor = "multi_thread")]
#[should_panic(expected = "is not in its `requires`")]
async fn a_zone_is_filled_only_by_plugins_that_require_its_owner() {
    let dir = tempfile::tempdir().unwrap();
    let _ = Host::start_enabled(vec![&BASE, &STRAY], dir.path()).await;
}
