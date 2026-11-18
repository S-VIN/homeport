use egui::epaint::Primitive;
use homeport_remote::{Frame, Scene, host::Encoder, unpack};

#[tokio::test(flavor = "multi_thread")]
async fn kernel_shell_survives_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let host = homeport_kernel::Host::start(vec![], dir.path())
        .await
        .unwrap();
    let ctx = egui::Context::default();
    let client = egui::Context::default();
    let mut session = host.session(&ctx);
    let mut enc = Encoder::default();
    let mut scene = Scene::default();
    for i in 0..4 {
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
        let n = out.shapes.len();
        let bytes = enc.encode(out);
        if i == 1 {
            std::fs::write(
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/frame.bin"),
                &bytes,
            )
            .unwrap();
        }
        let frame: Frame = unpack(&bytes).unwrap_or_else(|e| panic!("frame {i}: {e}"));
        let _ = scene.apply(&client, frame);
        let verts: usize = scene
            .primitives
            .iter()
            .map(|p| match &p.primitive {
                Primitive::Mesh(m) => m.vertices.len(),
                _ => 0,
            })
            .sum();
        eprintln!("frame {i}: shapes={n} bytes={} verts={verts}", bytes.len());
    }
    drop(session);
    host.shutdown().await;
}
