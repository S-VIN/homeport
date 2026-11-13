//! Windows/macOS tray, owned by the main UI thread (required by AppKit).
use crate::icon::{self, Link};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use tray_icon::{menu::{Menu, MenuEvent, MenuItem}, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub struct NativeTray {
    icon: TrayIcon,
    link: Link,
    pub quitting: Arc<AtomicBool>,
}
pub fn show(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}
fn image(link: Link) -> tray_icon::Icon {
    let image = icon::icon(link);
    tray_icon::Icon::from_rgba(image.rgba, image.width, image.height).expect("bundled tray icon")
}
impl NativeTray {
    pub fn new(ctx: &egui::Context) -> Result<Self, Box<dyn std::error::Error>> {
        #[cfg(target_os = "macos")]
        crate::macos::install_reopen(ctx);
        let menu = Menu::new();
        let open = MenuItem::new("Open rproxy", true, None);
        let quit = MenuItem::new("Quit rproxy", true, None);
        menu.append_items(&[&open, &quit])?;
        let icon = TrayIconBuilder::new().with_menu(Box::new(menu)).with_icon(image(Link::Idle))
            .with_tooltip("rproxy · not connected")
            .with_menu_on_left_click(false).build()?;
        let quitting = Arc::new(AtomicBool::new(false));
        let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
        let context = ctx.clone();
        let stop = quitting.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id { show(&context); }
            else if event.id == quit_id {
                stop.store(true, Ordering::Release);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                context.request_repaint();
            }
        }));
        let context = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if matches!(event, TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }) { show(&context); }
        }));
        Ok(Self { icon, link: Link::Idle, quitting })
    }
    pub fn update(&mut self, link: Link) {
        if self.link == link { return; }
        if self.icon.set_icon(Some(image(link))).is_ok() {
            self.link = link;
            let text = match link { Link::Idle => "rproxy · not connected", Link::Selected => "rproxy · connected", Link::Failed => "rproxy · connection error" };
            let _ = self.icon.set_tooltip(Some(text));
        }
    }
}
