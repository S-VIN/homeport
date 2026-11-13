use eframe::egui;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // Connection colors are used by the Linux tray.
#[derive(Clone, Copy, PartialEq)]
pub enum Link {
    Idle,
    Selected,
    Failed,
}
pub fn icon(link: Link) -> egui::IconData {
    let bytes = match link {
        Link::Idle => include_bytes!("../assets/logos/blue.png").as_slice(),
        Link::Selected => include_bytes!("../assets/logos/green.png"),
        Link::Failed => include_bytes!("../assets/logos/red.png"),
    };
    let img = image::load_from_memory(bytes).expect("bundled rproxy icon").into_rgba8();
    egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
}
