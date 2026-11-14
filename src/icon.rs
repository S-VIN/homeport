use eframe::egui;
#[derive(Clone, Copy, PartialEq)]
pub enum Link {
    Idle,
    Selected,
    Failed,
}
pub fn status(host: &rproxy_kernel::Host) -> Link {
    use rproxy_kernel::shell::TrayLink;
    let Some(k) = host.try_kernel() else {
        return Link::Idle;
    };
    let answers = k.snapshot().hooks(&rproxy_kernel::shell::TRAY, &());
    if !k.core_status().running() || answers.contains(&Some(TrayLink::Failed)) {
        Link::Failed
    } else if answers.contains(&Some(TrayLink::Connected)) {
        Link::Selected
    } else {
        Link::Idle
    }
}
pub fn icon(link: Link) -> egui::IconData {
    let bytes = match link {
        Link::Idle => include_bytes!("../assets/logos/blue.png").as_slice(),
        Link::Selected => include_bytes!("../assets/logos/green.png"),
        Link::Failed => include_bytes!("../assets/logos/red.png"),
    };
    let img = image::load_from_memory(bytes)
        .expect("bundled rproxy icon")
        .into_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}
