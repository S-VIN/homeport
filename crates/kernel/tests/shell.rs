use rproxy_kernel::{Host, egui};

#[tokio::test(flavor = "multi_thread")]
async fn empty_kernel_draws_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(vec![], dir.path()).await.unwrap();
    let ctx = egui::Context::default();
    let mut session = host.session(&ctx);
    let mut counts = Vec::new();
    for _ in 0..4 {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640., 560.),
                )),
                ..Default::default()
            },
            |ui| session.ui(ui),
        );
        counts.push(out.shapes.len());
        out.drop_without_applying_deltas();
    }
    assert!(counts[3] > 20, "shapes per frame: {counts:?}");
    drop(session);
    host.shutdown().await;
}

mod saved {
    use rproxy_kernel::{Host, egui, prelude::*};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
    #[table(name = "test_saved_notes")]
    struct Note {
        id: Id,
        text: String,
    }
    static NOTES: State<Vec<Note>> = State::new("saved.notes");
    static SUBMITTED: AtomicBool = AtomicBool::new(false);
    static TAKEN: AtomicBool = AtomicBool::new(false);
    static REMOVE: AtomicBool = AtomicBool::new(false);

    static NOTES_PLUGIN: Plugin = Plugin {
        id: "saved",
        name: "Saved",
        category: categories::SERVERS,
        setup: |p| {
            p.list(&NOTES);
            p.page_fn(
                PageSpec {
                    id: "notes",
                    title: "Notes",
                    icon: Icon::Servers,
                    order: 10,
                },
                |ui, cx| {
                    if !SUBMITTED.swap(true, Ordering::SeqCst) {
                        let note = Note {
                            id: Id::random(),
                            text: "hi".into(),
                        };
                        cx.submit("add", Add::new(&NOTES, note));
                    }
                    if REMOVE.swap(false, Ordering::SeqCst) {
                        let id = cx.get(&NOTES)[0].id;
                        cx.submit("add", Remove::new(&NOTES, id));
                    }
                    // Like a draft form: the page takes the outcome at once.
                    if cx.outcome("add").is_some() {
                        TAKEN.store(true, Ordering::SeqCst);
                    }
                    ui.label("notes page");
                },
            );
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

    /// "Saved" stays for a moment after the page took the outcome of a list
    /// edit (an add, then a remove).
    #[tokio::test(flavor = "multi_thread")]
    async fn saved_stays_after_the_page_takes_the_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::start(vec![&NOTES_PLUGIN], dir.path()).await.unwrap();
        let ctx = egui::Context::default();
        let mut session = host.session(&ctx);
        let mut frame = |time: f64| {
            let out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900., 700.),
                    )),
                    time: Some(time),
                    ..Default::default()
                },
                |ui| session.ui(ui),
            );
            let mut shown = Vec::new();
            out.shapes.iter().for_each(|s| texts(&s.shape, &mut shown));
            out.drop_without_applying_deltas();
            shown
        };
        let mut time = 0.;
        for round in 0..2 {
            if round == 1 {
                TAKEN.store(false, Ordering::SeqCst);
                REMOVE.store(true, Ordering::SeqCst);
            }
            let start = time;
            while !TAKEN.load(Ordering::SeqCst) {
                assert!(time - start < 5., "the edit finishes");
                frame(time);
                time += 0.05;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            for _ in 0..3 {
                time += 0.1;
                let shown = frame(time);
                assert!(shown.iter().any(|s| s == "Saved"), "{round}: {shown:?}");
            }
            time += 3.;
            let shown = frame(time);
            assert!(!shown.iter().any(|s| s == "Saved"), "{round}: {shown:?}");
        }
        drop(session);
        host.shutdown().await;
    }
}
