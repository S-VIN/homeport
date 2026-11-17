//! Exercise the real egui shell and optionally export its tessellated frames for visual review.
use base64::Engine;
use egui::{
    Event, Pos2, Rect,
    epaint::{Primitive, Shape},
};
use image::ImageEncoder;
use rproxy_kernel::{Host, Session};
use serde_json::json;
use std::{path::Path, sync::Arc, time::Instant};

struct Ui {
    host: Arc<Host>,
    ctx: egui::Context,
    session: Session,
    size: egui::Vec2,
    dark: bool,
    offline: bool,
    empty: bool,
    tick: f64,
    textures: Vec<serde_json::Value>,
    shapes: Vec<egui::epaint::ClippedShape>,
}
impl Ui {
    fn new(host: Arc<Host>, size: [f32; 2], dark: bool) -> Self {
        let ctx = egui::Context::default();
        let session = host.session(&ctx);
        Self {
            host,
            ctx,
            session,
            size: size.into(),
            dark,
            offline: false,
            empty: false,
            tick: 0.,
            textures: vec![],
            shapes: vec![],
        }
    }
    fn frame(&mut self, events: Vec<Event>) {
        let servers = self.host.kernel().snapshot().get(&plugin_servers::SERVERS);
        let server = servers
            .first()
            .map(|s| s.id.to_string())
            .unwrap_or_else(|| "DIRECT".into());
        self.host
            .kernel()
            .cx_for("connections")
            .update_live(&plugin_connections::TRAFFIC, |t| {
                t.online = !self.offline;
                t.down_rate = if self.offline { 0 } else { 1_240_000 };
                t.up_rate = if self.offline { 0 } else { 42_000 };
                t.down_total = 42_000_000;
                t.up_total = 1_700_000;
                t.conns = [
                    "alpha.example",
                    "chat.example",
                    "updates.example",
                    "media.example",
                    "long-host-name-for-clipping.example",
                ]
                .iter()
                .enumerate()
                .map(|(i, host)| plugin_connections::Conn {
                    id: i.to_string(),
                    target: format!("{host}:443"),
                    network: "TCP".into(),
                    process: String::new(),
                    rule: "DomainSuffix example".into(),
                    chain: vec![server.clone()],
                    up: 3400 + i as u64,
                    down: 120_000 * (i as u64 + 1),
                    started: Instant::now(),
                })
                .collect();
                if self.empty {
                    t.conns.clear();
                }
            });
        self.tick += 0.1;
        let mut out = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                time: Some(self.tick),
                events,
                system_theme: Some(if self.dark {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                }),
                ..Default::default()
            },
            |ui| self.session.ui(ui),
        );
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                let egui::ImageData::Color(image) = &delta.image;
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                let mut png = Vec::new();
                image::codecs::png::PngEncoder::new(&mut png)
                    .write_image(
                        &rgba,
                        image.width() as u32,
                        image.height() as u32,
                        image::ExtendedColorType::Rgba8,
                    )
                    .unwrap();
                self.textures.push(json!({"id":format!("{id:?}"),"size":image.size,"pos":delta.pos,"png":base64::engine::general_purpose::STANDARD.encode(png) }));
            }
        }
        self.shapes = std::mem::take(&mut out.shapes);
        out.drop_without_applying_deltas();
    }
    fn text(&self, label: &str) -> Option<Rect> {
        fn find(s: &Shape, label: &str) -> Option<Rect> {
            match s {
                Shape::Text(t) if t.galley.text() == label => {
                    Some(Rect::from_min_size(t.pos, t.galley.size()))
                }
                Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                _ => None,
            }
        }
        self.shapes.iter().find_map(|s| {
            find(&s.shape, label).filter(|r| {
                s.clip_rect.contains_rect(*r) && r.min.y >= 0. && r.max.y <= self.size.y
            })
        })
    }
    fn text_containing(&self, part: &str) -> Option<Rect> {
        fn find(s: &Shape, part: &str) -> Option<Rect> {
            match s {
                Shape::Text(t) if t.galley.text().contains(part) => {
                    Some(Rect::from_min_size(t.pos, t.galley.size()))
                }
                Shape::Vec(v) => v.iter().find_map(|s| find(s, part)),
                _ => None,
            }
        }
        self.shapes.iter().find_map(|s| {
            find(&s.shape, part).filter(|r| {
                s.clip_rect.contains_rect(*r) && r.min.y >= 0. && r.max.y <= self.size.y
            })
        })
    }
    fn click(&mut self, label: &str) {
        let pos = self
            .text(label)
            .unwrap_or_else(|| panic!("missing {label} at {:?}", self.size))
            .center();
        for pressed in [true, false] {
            self.frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ]);
        }
        self.frame(vec![Event::PointerGone]);
    }
    /// A tab: in the bottom navigation, the sidebar or the More menu (page
    /// titles and settings sections may say the same words).
    fn tab(&self, label: &str) -> Option<Rect> {
        fn all(s: &Shape, label: &str, out: &mut Vec<Rect>) {
            match s {
                Shape::Text(t) if t.galley.text() == label => {
                    out.push(Rect::from_min_size(t.pos, t.galley.size()))
                }
                Shape::Vec(v) => v.iter().for_each(|s| all(s, label, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for s in &self.shapes {
            let mut here = Vec::new();
            all(&s.shape, label, &mut here);
            found.extend(here.into_iter().filter(|r| {
                s.clip_rect.contains_rect(*r) && r.min.y >= 0. && r.max.y <= self.size.y
            }));
        }
        let sidebar = self.size.x >= 960.;
        found
            .iter()
            .copied()
            .find(|r| {
                if sidebar {
                    r.max.x <= 184.
                } else {
                    r.min.y >= self.size.y - 48.
                }
            })
            .or_else(|| found.last().copied())
    }
    fn navigate(&mut self, label: &str) {
        let tab = |ui: &Self| {
            ui.tab(label)
                .filter(|r| ui.size.x >= 960. && r.max.x <= 184. || r.min.y >= ui.size.y - 48.)
        };
        if tab(self).is_none() {
            self.click("More");
        }
        let pos = self
            .tab(label)
            .unwrap_or_else(|| panic!("missing tab {label} at {:?}", self.size))
            .center();
        for pressed in [true, false] {
            self.frame(vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ]);
        }
        for _ in 0..2 {
            self.frame(vec![]);
        }
    }
    fn capture(&self, directory: &Path, page: &str) {
        let primitives = self.ctx.tessellate(self.shapes.clone(), 1.);
        let meshes:Vec<_> = primitives.into_iter().filter_map(|p| match p.primitive {
            Primitive::Mesh(m) => Some(json!({"clip":[p.clip_rect.min.x,p.clip_rect.min.y,p.clip_rect.max.x,p.clip_rect.max.y],"id":format!("{:?}",m.texture_id),"indices":m.indices,"vertices":m.vertices.iter().map(|v|json!([v.pos.x,v.pos.y,v.uv.x,v.uv.y,v.color.to_array()])).collect::<Vec<_>>()})),
            _ => None,
        }).collect();
        let name = format!(
            "{page}-{}-{}x{}.json",
            if self.dark { "dark" } else { "light" },
            self.size.x,
            self.size.y
        );
        std::fs::create_dir_all(directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            directory.join(name),
            serde_json::to_vec(
                &json!({"size":[self.size.x,self.size.y],"textures":self.textures,"meshes":meshes}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_navigation_connections_and_configuration_fit_supported_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(rproxy::catalog(), dir.path()).await.unwrap();
    let source = rproxy_kernel::Id::random();
    let proxies = plugin_subscriptions::formats::uri::parse_links("vless://00000000-0000-0000-0000-000000000001@example.org:443?security=tls#%F0%9F%87%AB%F0%9F%87%AE%20Helsinki%20%E2%9A%A1\nvless://00000000-0000-0000-0000-000000000002@example.net:443?security=tls#%F0%9F%87%A9%F0%9F%87%AA%20Frankfurt\nvless://00000000-0000-0000-0000-000000000003@example.com:443?security=tls#%F0%9F%9A%80%20A%20very%20long%20server%20name%20that%20must%20truncate").unwrap();
    host.kernel()
        .cx_for("servers")
        .call(plugin_servers::ReplaceFromSource {
            source,
            proxies,
            still_current: None,
        })
        .await
        .unwrap();
    let directory = std::env::var_os("RPROXY_CAPTURE_DIR");
    for size in [[480., 540.], [640., 560.], [1040., 600.], [375., 540.]] {
        for dark in [false, true] {
            let mut ui = Ui::new(host.clone(), size, dark);
            for _ in 0..4 {
                ui.frame(vec![]);
            }
            for page in [
                "Servers",
                "Connections",
                "Plugins",
                "Settings",
                "Subscriptions",
            ] {
                ui.navigate(page);
                if page == "Connections" {
                    let a = ui
                        .text("alpha.example:443")
                        .expect("first connection visible");
                    let b = ui
                        .text("chat.example:443")
                        .expect("second connection visible");
                    assert!(
                        (40.0..80.0).contains(&(b.top() - a.top())),
                        "connection rows must be compact: {a:?}, {b:?}"
                    );
                    assert!(a.right() < size[0] && b.right() < size[0]);
                    assert!(ui.shapes.iter().any(|s| matches!(&s.shape,Shape::Text(t) if t.galley.rows.iter().any(|r| r.glyphs.iter().any(|g| g.is_color)))),"emoji should be drawn in color");
                }
                if page == "Settings" {
                    assert!(ui.text("Backups").is_none());
                    assert!(
                        ui.text("Export…").is_some(),
                        "export is discoverable without scrolling"
                    );
                    assert!(
                        ui.text("Import…").is_some(),
                        "import is discoverable without scrolling"
                    );
                }
                if let Some(dir) = &directory {
                    ui.capture(Path::new(dir), page);
                }
            }
            ui.navigate("Connections");
            ui.offline = true;
            ui.frame(vec![]);
            if let Some(dir) = &directory {
                ui.capture(Path::new(dir), "Connections-offline");
            }
            ui.empty = true;
            ui.frame(vec![]);
            assert!(ui.text("Core offline").is_some());
            if let Some(dir) = &directory {
                ui.capture(Path::new(dir), "Connections-offline-empty");
            }
            ui.offline = false;
            ui.frame(vec![]);
            assert!(ui.text("No open connections").is_some());
            if let Some(dir) = &directory {
                ui.capture(Path::new(dir), "Connections-empty");
            }
            ui.empty = false;
            ui.ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::unique("connections-filter"),
                    "no-match".to_owned(),
                )
            });
            ui.frame(vec![]);
            assert!(ui.text("Nothing matches").is_some());
            if let Some(dir) = &directory {
                ui.capture(Path::new(dir), "Connections-filtered");
            }
            ui.click("Clear filter");
            assert!(ui.text("alpha.example:443").is_some());
            host.kernel().set_plugin("logs", false).unwrap();
            ui.navigate("Settings");
            assert!(
                ui.text("Restart").is_some(),
                "pending plugin restart must stay visible on other pages"
            );
            if let Some(dir) = &directory {
                ui.capture(Path::new(dir), "Settings-restart-pending");
            }
            ui.click("Restart");
            assert!(ui.text("Restart now").is_some());
            host.kernel().set_plugin("logs", true).unwrap();
            ui.navigate("Settings");
            ui.session.file_finished(Ok(Some((
                "my-config.sqlite3".into(),
                host.export_configuration().unwrap(),
            ))));
            // The file is checked in the background.
            for _ in 0..100 {
                ui.frame(vec![]);
                if ui.text("Replace settings").is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(ui.text("Replace settings").is_some());
            if let Some(dir) = &directory {
                ui.capture(Path::new(dir), "Settings-import");
            }
            ui.click("Cancel");
            assert!(ui.text("Replace settings").is_none());
        }
    }
    host.shutdown().await;
}

#[derive(Clone)]
struct PopupReviewJob(usize);
impl rproxy_kernel::Op for PopupReviewJob {
    type Reply = ();
    fn job(&self) -> Option<rproxy_kernel::JobSpec> {
        Some(rproxy_kernel::JobSpec::keyed(
            self.0.to_string(),
            format!("Refresh subscription {} · Helsinki ⚡", self.0),
        ))
    }
}
static POPUP_REVIEW_PLUGIN: rproxy_kernel::Plugin = rproxy_kernel::Plugin {
    id: "popup-review",
    name: "Popup review fixture",
    description: "",
    category: rproxy_kernel::categories::NETWORK,
    requires: &[],
    required: true,
    default_enabled: true,
    platforms: rproxy_kernel::Platform::ALL,
    setup: |p| {
        p.handle(|_: rproxy_kernel::Cx, job: PopupReviewJob| async move {
            Err(rproxy_kernel::Error::msg(format!("Subscription {} could not refresh: the remote server returned an invalid response. Your previous servers are kept. Check the subscription URL and refresh again. Provider detail: {}", job.0, "invalid-response-".repeat(5))))
        });
        Ok(())
    },
};

impl Ui {
    fn escape(&mut self) {
        self.frame(vec![Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]);
        self.frame(vec![Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Default::default(),
        }]);
    }
    fn hover(&mut self, text: &str) {
        let pos = self
            .text(text)
            .unwrap_or_else(|| panic!("missing hover target {text}"))
            .center();
        self.frame(vec![Event::PointerMoved(pos)]);
        for _ in 0..12 {
            self.frame(vec![]);
        }
    }
    fn reveal(&mut self, label: &str) {
        for _ in 0..10 {
            if self.text(label).is_some() {
                return;
            }
            self.frame(vec![
                Event::PointerMoved(Pos2::new(self.size.x * 0.5, self.size.y * 0.6)),
                Event::MouseWheel {
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0., -100.),
                    modifiers: Default::default(),
                },
            ]);
            self.settle();
        }
        panic!("cannot reveal {label} at {:?}", self.size);
    }
    fn settle(&mut self) {
        for _ in 0..4 {
            self.frame(vec![]);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn popup_review_scenarios() {
    use rproxy_kernel::list::Add;
    let dir = tempfile::tempdir().unwrap();
    if let Ok(identity) = std::env::var("RPROXY_TEST_IDENTITY_DIR") {
        std::fs::copy(
            Path::new(&identity).join("device-id"),
            dir.path().join("device-id"),
        )
        .unwrap();
    }
    let mut catalog = rproxy::catalog();
    catalog.push(&POPUP_REVIEW_PLUGIN);
    let host = Host::start(catalog, dir.path()).await.unwrap();
    let urls=std::env::var("RPROXY_TEST_SUBSCRIPTION_URLS").unwrap_or_else(|_| "vless://00000000-0000-0000-0000-000000000001@example.org:443?security=tls#Helsinki%20%E2%9A%A1\nvless://00000000-0000-0000-0000-000000000002@example.net:443?security=tls#Frankfurt%20%F0%9F%9A%80".into());
    let cx = host.kernel().cx_for("subscriptions");
    for url in urls.lines() {
        cx.call(Add::new(
            &plugin_subscriptions::SOURCES,
            plugin_subscriptions::Source::new(url),
        ))
        .await
        .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let snapshot = host.kernel().snapshot();
            let sources = snapshot.get(&plugin_subscriptions::SOURCES);
            assert!(
                sources.iter().all(|s| s.error.is_none()),
                "subscription fixture failed to load"
            );
            if sources.iter().all(|s| s.updated.is_some()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let selected = host.kernel().snapshot().get(&plugin_servers::SERVERS)[0].id;
    host.kernel()
        .cx_for("servers")
        .call(plugin_servers::Select::manual(Some(selected)))
        .await
        .unwrap();
    eprintln!(
        "Populated UI fixture: {} subscriptions, {} servers",
        host.kernel()
            .snapshot()
            .get(&plugin_subscriptions::SOURCES)
            .len(),
        host.kernel().snapshot().get(&plugin_servers::SERVERS).len()
    );
    for i in 0..9 {
        host.kernel()
            .cx_for("popup-review")
            .spawn(PopupReviewJob(i))
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while host
            .kernel()
            .board()
            .runs
            .iter()
            .filter(|r| r.key.starts_with("popup-review/") && !r.phase.active())
            .count()
            < 9
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let directory = std::env::var_os("RPROXY_CAPTURE_DIR");
    for size in [[375., 320.], [375., 540.], [640., 360.], [1040., 600.]] {
        for dark in [false, true] {
            for scenario in [
                "activity-long",
                "connection",
                "listeners",
                "more",
                "schedule",
                "tooltip",
                "notification",
                "subscriptions",
                "subscription-tooltip",
                "connection-tooltip",
                "servers",
                "server-tooltip",
                "server-details",
                "plugins",
                "settings",
                "connections",
                "local-proxy",
                "routing",
                "checks",
                "deploy",
                "system-proxy-help",
            ] {
                if std::env::var("RPROXY_CAPTURE_SCENARIOS")
                    .is_ok_and(|only| !only.split(',').any(|s| s == scenario))
                {
                    continue;
                }
                let mut ui = Ui::new(host.clone(), size, dark);
                ui.settle();
                match scenario {
                    "activity-long" if ui.text("Tasks").is_some()=>ui.click("Tasks"),
                    "activity-long"=>{
                        // On a phone idle Tasks is only its icon, at the right end of the bar.
                        for pressed in [true,false] { let pos=Pos2::new(size[0]-22.,16.);ui.frame(vec![Event::PointerMoved(pos),Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()}]); }
                    },
                    "connection" if size[0]<960.=>{
                        // The selected server's chip leads the status bar.
                        for pressed in [true,false] { let pos=Pos2::new(70.,16.);ui.frame(vec![Event::PointerMoved(pos),Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()}]); }
                    },
                    "listeners" if size[0]<960.=>{ if ui.text("127.0.0.1:20808").is_some(){ui.click("127.0.0.1:20808");}else{continue;} },
                    "more" if ui.text("More").is_some()=>ui.click("More"),
                    "schedule"=>{ui.navigate("Settings");ui.reveal("Every day");ui.click("Every day");},
                    "tooltip"=>{ui.navigate("Settings");ui.hover("Export…");},
                    "notification"=>host.kernel().notify_error_at("Subscription could not refresh", "Your previous servers are kept. Check the subscription URL and try again. Provider details: an unexpected response was received from the subscription service and it could not be parsed as a supported server list.",Some("subscriptions")),
                    "subscriptions"=>ui.navigate("Subscriptions"),
                    "plugins"=>ui.navigate("Plugins"),
                    "settings"=>ui.navigate("Settings"),
                    "connections"=>ui.navigate("Connections"),
                    "local-proxy"=>ui.navigate("Local proxy"),
                    "routing"=>ui.navigate("Routing"),
                    "checks"=>ui.navigate("Tests"),
                    "deploy"=>ui.navigate("Install to server"),
                    "system-proxy-help"=>{ui.navigate("Local proxy");ui.reveal("System proxy");ui.hover("System proxy");},
                    "subscription-tooltip"=>{ui.navigate("Subscriptions");let label=if std::env::var_os("RPROXY_TEST_SUBSCRIPTION_URLS").is_some(){"allvsub.cc"}else{"Helsinki ⚡"}; if ui.text(label).is_some(){ui.hover(label);}else{continue;}},
                    "connection-tooltip" if size[0]<960.=>{ui.frame(vec![Event::PointerMoved(Pos2::new(70.,16.))]);for _ in 0..12 { ui.frame(vec![]); }},
                    "server-tooltip"|"server-details"=>{
                        // Phones show the row's Connect as an icon: find the row by a server name.
                        let names=host.kernel().snapshot().get(&plugin_servers::SERVERS).iter().filter(|s|s.id!=selected).map(|s|s.name.split_whitespace().next().unwrap_or_default().to_owned()).collect::<Vec<_>>();
                        let row=names.iter().find_map(|n|ui.text_containing(n)).expect("populated server row");
                        let pos=Pos2::new(if size[0]>=960. {270.}else{80.},row.center().y);
                        ui.frame(vec![Event::PointerMoved(pos)]);
                        if scenario=="server-tooltip" { for _ in 0..12 {ui.frame(vec![]);} }
                        else {for pressed in [true,false] {ui.frame(vec![Event::PointerButton{pos,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()}]);}}
                    },
                    "servers"=>{},
                    _=>continue,
                }
                if !scenario.ends_with("tooltip") && !scenario.ends_with("help") {
                    ui.settle();
                }
                if scenario == "activity-long" {
                    let rect =
                        egui::containers::AreaState::load(&ui.ctx, egui::Id::unique("activity"))
                            .unwrap()
                            .rect();
                    assert!(
                        Rect::from_min_size(Pos2::ZERO, ui.size).contains_rect(rect),
                        "Activity must fit the window: {rect:?}"
                    );
                    if let Some(dir) = &directory {
                        ui.capture(Path::new(dir), "Popup-activity-long");
                    }
                    for _ in 0..3 {
                        ui.frame(vec![
                            Event::PointerMoved(rect.center()),
                            Event::MouseWheel {
                                phase: egui::TouchPhase::Move,
                                source: egui::MouseWheelSource::Unknown,
                                unit: egui::MouseWheelUnit::Point,
                                delta: egui::vec2(0., -1000.),
                                modifiers: Default::default(),
                            },
                        ]);
                        ui.settle();
                    }
                    assert!(
                        ui.text("Scheduled").is_some(),
                        "Activity must scroll to the final entries"
                    );
                    if let Some(dir) = &directory {
                        ui.capture(Path::new(dir), "Popup-activity-scrolled");
                    }
                } else if let Some(dir) = &directory {
                    ui.capture(Path::new(dir), &format!("Popup-{scenario}"));
                }
                ui.escape();
                ui.settle();
                if scenario == "activity-long" {
                    assert!(ui.text("Scheduled").is_none(), "Escape dismisses Activity");
                }
            }
        }
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_quit_is_explicit_and_recreated_windows_preserve_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::start(rproxy::catalog(), dir.path()).await.unwrap();
    for size in [[375., 540.], [640., 560.]] {
        for dark in [false, true] {
            let mut ui = Ui::new(host.clone(), size, dark);
            ui.settle();
            ui.navigate("Settings");
            assert!(
                ui.text("Quit rproxy").is_none(),
                "web sessions cannot quit the host"
            );
            ui.session.set_exit_available(true);
            ui.settle();
            ui.reveal("Quit rproxy");
            if let Some(path) = std::env::var_os("RPROXY_CAPTURE_DIR") {
                ui.capture(Path::new(&path), "Native-settings-quit");
            }
            ui.click("Quit rproxy");
            assert!(ui.session.take_exit_request());
            assert!(!ui.session.take_exit_request());
            assert!(
                host.kernel().core_status().running(),
                "only the native owner handles the request"
            );
            ui.navigate("Subscriptions");
            ui.click("Subscription URL or server link");
            ui.frame(vec![Event::Text("https://draft.example/preserved".into())]);
            ui.settle();
            assert!(ui.text("https://draft.example/preserved").is_some());
            let replacement = egui::Context::default();
            ui.session.rebind(&replacement);
            ui.ctx = replacement;
            ui.settle();
            assert!(
                ui.text("https://draft.example/preserved").is_some(),
                "a new native window must retain the unsubmitted draft and current page"
            );
        }
    }
    host.shutdown().await;
}
