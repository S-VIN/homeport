//! Browser side: paint host frames, forward input. No application code here.
use crate::{pack, unpack, Frame, Scene, ToHost};
use egui::epaint::Primitive;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use wasm_bindgen::{prelude::*, JsCast};
use web_sys::{BinaryType, MessageEvent, WebSocket};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = window)]
    fn save_configuration(bytes: &js_sys::Uint8Array, name: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(js_namespace = window)]
    fn open_configuration(limit: u32) -> js_sys::Promise;
}

fn file_dialog(ws: &WebSocket, dialog: crate::FileDialog) {
    let ws = ws.clone();
    wasm_bindgen_futures::spawn_local(async move {
        use crate::{FileDialog, FileReply};
        let reply = match dialog {
            FileDialog::Save { name, bytes } => match save_configuration(&js_sys::Uint8Array::from(bytes.as_slice()), &name) {
                Ok(()) => FileReply::Finished,
                Err(_) => FileReply::Failed("The browser could not download the configuration. Try exporting again.".into()),
            },
            FileDialog::Open => match wasm_bindgen_futures::JsFuture::from(open_configuration(crate::MAX_FILE_BYTES as u32)).await {
                Ok(value) if value.is_null() => FileReply::Finished,
                Ok(value) => {
                    let name = js_sys::Reflect::get(&value, &"name".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
                    let bytes = js_sys::Reflect::get(&value, &"bytes".into()).ok().map(|v| js_sys::Uint8Array::new(&v).to_vec()).unwrap_or_default();
                    FileReply::Opened { name, bytes }
                }
                Err(_) => FileReply::Failed("Could not open the configuration. Choose an exported .sqlite3 file under 32 MiB.".into()),
            },
        };
        let _ = ws.send_with_u8_array(&pack(&ToHost::File(reply)));
    });
}

#[wasm_bindgen]
pub async fn start(canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(|info| {
        let text = format!("rproxy client crashed: {info}");
        web_sys::console::error_1(&text.clone().into());
        status(Some(&text));
    }));
    eframe::WebRunner::new()
        .start(canvas.clone(), eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(Client::new(&cc.egui_ctx, &canvas)))))
        .await
}

#[derive(Default)]
struct Inbox {
    frames: VecDeque<Vec<u8>>,
    open: bool,
    closed: bool,
}

struct Socket {
    ws: WebSocket,
    inbox: Rc<RefCell<Inbox>>,
    _handlers: Vec<Closure<dyn FnMut(JsValue)>>,
}
impl Socket {
    fn connect(ctx: &egui::Context) -> Option<Self> {
        let loc = web_sys::window()?.location();
        let scheme = if loc.protocol().ok()? == "https:" { "wss" } else { "ws" };
        let ws = WebSocket::new(&format!("{scheme}://{}/ws", loc.host().ok()?)).ok()?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let inbox: Rc<RefCell<Inbox>> = Rc::default();
        let mut handlers = Vec::new();
        let (i, c) = (inbox.clone(), ctx.clone());
        let on_message = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            let e: MessageEvent = e.unchecked_into();
            if let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                i.borrow_mut().frames.push_back(js_sys::Uint8Array::new(&buf).to_vec());
                c.request_repaint();
            }
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        handlers.push(on_message);
        let (i, c) = (inbox.clone(), ctx.clone());
        let on_open = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            i.borrow_mut().open = true;
            c.request_repaint();
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        handlers.push(on_open);
        let (i, c) = (inbox.clone(), ctx.clone());
        let on_close = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            i.borrow_mut().closed = true;
            c.request_repaint();
        });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        handlers.push(on_close);
        Some(Self { ws, inbox, _handlers: handlers })
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.ws.set_onmessage(None);
        self.ws.set_onopen(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

fn apply_platform(ctx: &egui::Context, p: egui::PlatformOutput) {

        ctx.set_cursor_icon(p.cursor_icon);
        for c in p.commands {
            match c {
                egui::OutputCommand::CopyText(t) => ctx.copy_text(t),
                egui::OutputCommand::OpenUrl(u) => ctx.open_url(u),
                egui::OutputCommand::CopyImage(_) | egui::OutputCommand::TextSelectionSettled(_) => {}
            }
        }
        ctx.output_mut(|o| {
            o.ime = p.ime;
            o.mutable_text_under_cursor = p.mutable_text_under_cursor;
        });
}

struct Client {
    socket: Option<Socket>,
    scene: Scene,
    sent_once: bool,
    retry_at: f64,
    shown: bool,
    last_screen: Option<egui::Rect>,
    last_theme: Option<egui::Theme>,
    /// Where the last wheel event happened (CSS points), and the last pointer
    /// position the host knows: a wheel without a preceding move still scrolls
    /// the area under it.
    wheel_at: Rc<Cell<Option<egui::Pos2>>>,
    pointer: Option<egui::Pos2>,
    _wheel: Option<Closure<dyn FnMut(web_sys::WheelEvent)>>,
}

fn status(text: Option<&str>) {
    let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("status")) else { return };
    match text {
        Some(t) => {
            el.set_text_content(Some(t));
            let _ = el.set_attribute("style", "");
        }
        None => {
            let _ = el.set_attribute("style", "display:none");
        }
    }
}

impl Client {
    fn new(ctx: &egui::Context, canvas: &web_sys::HtmlCanvasElement) -> Self {
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        status(Some("Connecting to rproxy…"));
        let wheel_at: Rc<Cell<Option<egui::Pos2>>> = Rc::default();
        let (at, c, target) = (wheel_at.clone(), ctx.clone(), canvas.clone());
        let wheel = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(move |e: web_sys::WheelEvent| {
            let rect = target.get_bounding_client_rect();
            let zoom = c.zoom_factor();
            at.set(Some(egui::pos2((e.client_x() as f32 - rect.left() as f32) / zoom, (e.client_y() as f32 - rect.top() as f32) / zoom)));
        });
        let listening = canvas.add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref()).is_ok();
        Self {
            socket: Socket::connect(ctx),
            scene: Scene::default(),
            sent_once: false,
            retry_at: 0.,
            shown: false,
            last_screen: None,
            last_theme: None,
            wheel_at,
            pointer: None,
            _wheel: listening.then_some(wheel),
        }
    }
}

impl eframe::App for Client {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        let closed = self.socket.as_ref().is_none_or(|s| s.inbox.borrow().closed);
        if closed {
            if self.shown {
                status(Some("Connection to rproxy lost. Reconnecting…"));
                self.shown = false;
            }
            if now >= self.retry_at {
                self.socket = Socket::connect(ctx);
                self.scene.clear(ctx);
                self.sent_once = false;
                self.retry_at = now + 1.5;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if let Some(s) = &self.socket {
            let frames: Vec<Vec<u8>> = s.inbox.borrow_mut().frames.drain(..).collect();
            for f in frames {
                let decoded = unpack::<Frame>(&f);
                if let Err(e) = &decoded {
                    // Deltas are now out of sync: start over with a fresh connection.
                    web_sys::console::error_1(&format!("frame decode failed ({} bytes): {e}", f.len()).into());
                    s.inbox.borrow_mut().closed = true;
                    ctx.request_repaint();
                    break;
                }
                if let Ok(mut frame) = decoded {
                    if let Some(dialog) = frame.file_dialog.take() { file_dialog(&s.ws, dialog); }
                    let platform = self.scene.apply(ctx, frame);
                    apply_platform(ctx, platform);
                    if !self.shown {
                        status(None);
                        self.shown = true;
                    }
                }
            }
            let mut raw = ctx.input(|i| i.raw.clone());
            let open = s.inbox.borrow().open;
            // The system theme changes without an input event: forward it on its own.
            let changed = raw.screen_rect != self.last_screen || raw.system_theme != self.last_theme;
            if let Some(at) = self.wheel_at.take() {
                let wheel = raw.events.iter().position(|e| matches!(e, egui::Event::MouseWheel { .. }));
                if let Some(i) = wheel.filter(|_| self.pointer != Some(at)) {
                    raw.events.insert(i, egui::Event::PointerMoved(at));
                }
            }
            for e in &raw.events {
                match e {
                    egui::Event::PointerMoved(p) => self.pointer = Some(*p),
                    egui::Event::PointerGone => self.pointer = None,
                    _ => {}
                }
            }
            if open && (!self.sent_once || changed || !raw.events.is_empty()) {
                self.last_screen = raw.screen_rect;
                self.last_theme = raw.system_theme;
                raw.time = None;
                let _ = s.ws.send_with_u8_array(&pack(&ToHost::Input(raw)));
                self.sent_once = true;
            }
        }
        let painter = ctx.layer_painter(egui::LayerId::background());
        for p in &self.scene.primitives {
            if let Primitive::Mesh(m) = &p.primitive {
                painter.with_clip_rect(p.clip_rect).add(egui::Shape::mesh(m.clone()));
            }
        }
    }
}
