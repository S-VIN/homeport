use eframe::egui;
use ksni::{TrayMethods, menu::StandardItem};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};

/// Linux window backends do not report GNOME's appearance preference. The
/// Settings portal works on X11, Wayland and inside Flatpak without shelling out.
pub struct SystemTheme {
    value: Arc<std::sync::atomic::AtomicU8>,
    task: tokio::task::JoinHandle<()>,
}
impl SystemTheme {
    pub fn start(ctx: egui::Context) -> Self {
        use futures_util::StreamExt;
        let value = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let current = value.clone();
        let task = tokio::spawn(async move {
            let Ok(bus) = zbus::Connection::session().await else {
                return;
            };
            let Ok(settings) = zbus::Proxy::new(
                &bus,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings",
            )
            .await
            else {
                return;
            };
            // Subscribe first so a change during the initial read is not lost.
            let Ok(mut changes) = settings.receive_signal("SettingChanged").await else {
                return;
            };
            let publish = |v: &zbus::zvariant::Value<'_>| {
                if let Some(theme) = portal_theme(v) {
                    current.store(
                        if theme == egui::Theme::Dark { 1 } else { 2 },
                        Ordering::Release,
                    );
                    ctx.request_repaint();
                }
            };
            let key = ("org.freedesktop.appearance", "color-scheme");
            let initial: zbus::Result<zbus::zvariant::OwnedValue> =
                settings.call("ReadOne", &key).await;
            let initial = match initial {
                Ok(v) => Ok(v),
                Err(_) => settings.call("Read", &key).await,
            };
            if let Ok(v) = initial {
                publish(&v);
            }
            while let Some(signal) = changes.next().await {
                if let Ok((namespace, name, v)) =
                    signal
                        .body()
                        .deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
                    && namespace == key.0
                    && name == key.1
                {
                    publish(&v);
                }
            }
        });
        Self { value, task }
    }
    pub fn get(&self) -> Option<egui::Theme> {
        match self.value.load(Ordering::Acquire) {
            1 => Some(egui::Theme::Dark),
            2 => Some(egui::Theme::Light),
            _ => None,
        }
    }
}
impl Drop for SystemTheme {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn portal_theme(value: &zbus::zvariant::Value<'_>) -> Option<egui::Theme> {
    use zbus::zvariant::Value;
    match value {
        Value::U32(1) => Some(egui::Theme::Dark),
        Value::U32(0 | 2) => Some(egui::Theme::Light),
        // Legacy Settings.Read wraps the result in an extra variant.
        Value::Value(inner) => portal_theme(inner),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum WindowEvent {
    Open,
    Quit,
}
#[derive(Clone)]
pub struct WindowControl {
    pub wayland: Arc<AtomicBool>,
    pub open_sequence: Arc<AtomicU64>,
    pub handled_open: Arc<AtomicU64>,
    pub online: Arc<AtomicBool>,
    pub quitting: Arc<AtomicBool>,
    pub context: Arc<Mutex<Option<egui::Context>>>,
    tx: mpsc::Sender<Wake>,
}
pub enum Wake {
    Open(u64),
    Quit,
}
impl WindowControl {
    pub fn new() -> (Self, mpsc::Receiver<Wake>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                wayland: Default::default(),
                open_sequence: Default::default(),
                handled_open: Default::default(),
                online: Default::default(),
                quitting: Default::default(),
                context: Default::default(),
                tx,
            },
            rx,
        )
    }
    pub fn send(&self, event: WindowEvent) {
        // Publish the wake under the same mutex as window creation. It survives
        // the interval between native close and clearing the old context.
        let context = self.context.lock().unwrap();
        let wake = match event {
            WindowEvent::Quit => {
                self.quitting.store(true, Ordering::Release);
                Wake::Quit
            }
            WindowEvent::Open => Wake::Open(self.open_sequence.fetch_add(1, Ordering::AcqRel) + 1),
        };
        let _ = self.tx.send(wake);
        if let Some(ctx) = context.as_ref() {
            if event == WindowEvent::Quit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }
    }
    pub fn handle_open(&self, ctx: &egui::Context) {
        let sequence = self.open_sequence.load(Ordering::Acquire);
        if sequence <= self.handled_open.load(Ordering::Acquire)
            || ctx.input(|i| i.viewport().close_requested())
        {
            return;
        }
        if self.wayland.load(Ordering::Acquire) {
            // winit cannot unminimize/focus a Wayland window. Recreate its native
            // surface on explicit Open; the queued request remains unacknowledged.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.handled_open.store(sequence, Ordering::Release);
        }
    }
}
pub use crate::icon::{Link, icon};
pub struct Tray {
    pub link: Link,
    control: WindowControl,
}
impl Tray {
    pub async fn start(control: WindowControl) -> Option<ksni::Handle<Self>> {
        control.online.store(true, Ordering::Release);
        // Flatpak may not own other bus names; the tray works with the unique name.
        let tray = Self {
            control: control.clone(),
            link: Link::Idle,
        };
        let handle = match tray
            .disable_dbus_name(rproxy_kernel::Platform::is_flatpak())
            .spawn()
            .await
        {
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
        vec![ksni::Icon {
            width: i.width as i32,
            height: i.height as i32,
            data: i.rgba,
        }]
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
            StandardItem {
                label: "Open rproxy".into(),
                activate: Box::new(|t: &mut Self| t.control.send(WindowEvent::Open)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.control.send(WindowEvent::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::portal_theme;
    use egui::Theme;
    use zbus::zvariant::Value;

    #[test]
    fn portal_appearance_supports_gnome_default_and_legacy_variants() {
        assert_eq!(portal_theme(&Value::U32(0)), Some(Theme::Light));
        assert_eq!(portal_theme(&Value::U32(1)), Some(Theme::Dark));
        assert_eq!(portal_theme(&Value::U32(2)), Some(Theme::Light));
        assert_eq!(
            portal_theme(&Value::Value(Box::new(Value::U32(1)))),
            Some(Theme::Dark)
        );
        assert_eq!(portal_theme(&Value::U32(99)), None);
        assert_eq!(portal_theme(&Value::from("dark")), None);
    }
}
