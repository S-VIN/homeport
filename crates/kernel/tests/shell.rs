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
