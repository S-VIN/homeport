use eframe::egui;
use ksni::{menu::StandardItem, TrayMethods};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};

#[derive(Clone, Copy, PartialEq)]
pub enum WindowEvent {
    Open,
    Quit,
}
#[derive(Clone)]
pub struct WindowControl {
    pub online: Arc<AtomicBool>,
    pub quitting: Arc<AtomicBool>,
    pub context: Arc<Mutex<Option<egui::Context>>>,
    tx: mpsc::Sender<WindowEvent>,
}
impl WindowControl {
    pub fn new() -> (Self, mpsc::Receiver<WindowEvent>) {
        let (tx, rx) = mpsc::channel();
        (Self { online: Default::default(), quitting: Default::default(), context: Default::default(), tx }, rx)
    }
    pub fn send(&self, event: WindowEvent) {
        if event == WindowEvent::Quit {
            self.quitting.store(true, Ordering::Release);
        }
        let context = self.context.lock().unwrap();
        if let Some(ctx) = context.as_ref() {
            ctx.send_viewport_cmd(match event {
                WindowEvent::Quit => egui::ViewportCommand::Close,
                WindowEvent::Open => egui::ViewportCommand::Focus,
            });
            ctx.request_repaint();
        }
        if context.is_none() || event == WindowEvent::Quit {
            let _ = self.tx.send(event);
        }
    }
}
pub use crate::icon::{icon, Link};
pub struct Tray {
    pub link: Link,
    control: WindowControl,
}
impl Tray {
    pub async fn start(control: WindowControl) -> Option<ksni::Handle<Self>> {
        control.online.store(true, Ordering::Release);
        let handle = match (Self { control: control.clone(), link: Link::Idle }).spawn().await {
            Ok(h) => h,
            Err(_) => {
                control.online.store(false, Ordering::Release);
                return None;
            }
        };
        Some(handle)
    }
}
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "rproxy".into()
    }
    fn title(&self) -> String {
        "rproxy".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let mut i = icon(self.link);
        for p in i.rgba.as_chunks_mut::<4>().0 {
            p.rotate_right(1);
        }
        vec![ksni::Icon { width: i.width as i32, height: i.height as i32, data: i.rgba }]
    }
    fn activate(&mut self, _: i32, _: i32) {
        self.control.send(WindowEvent::Open)
    }
    fn watcher_online(&self) {
        self.control.online.store(true, Ordering::Release);
    }
    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.control.online.store(false, Ordering::Release);
        self.control.send(WindowEvent::Open);
        true
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem { label: "Open rproxy".into(), activate: Box::new(|t: &mut Self| t.control.send(WindowEvent::Open)), ..Default::default() }
                .into(),
            StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| t.control.send(WindowEvent::Quit)), ..Default::default() }.into(),
        ]
    }
}
