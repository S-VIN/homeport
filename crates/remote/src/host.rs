//! Host side: one egui context per browser tab, frames streamed over a WebSocket.
use crate::{image, pack, unpack, wire, Frame, Item, ShapeOp, TexId, Texture, ToHost};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::header,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use egui::epaint::Galley;
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};

/// The application: creates a UI for every new browser tab.
pub trait Frontend: Send + Sync + 'static {
    fn open(&self, ctx: &egui::Context) -> Box<dyn FrontendSession>;
}
pub trait FrontendSession: Send {
    fn ui(&mut self, ui: &mut egui::Ui);
    fn file_dialog(&mut self) -> Option<crate::FileDialog> { None }
    fn file_reply(&mut self, _: crate::FileReply) {}
}

pub struct Asset {
    pub path: &'static str,
    pub mime: &'static str,
    pub bytes: &'static [u8],
}

/// `GET /ws` plus static assets.
pub fn router(frontend: Arc<dyn Frontend>, assets: &'static [Asset]) -> Router {
    let mut app = Router::new().route("/ws", get(upgrade)).with_state(frontend);
    for a in assets {
        let (mime, bytes) = (a.mime, a.bytes);
        app = app.route(a.path, get(move || async move { ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "no-cache")], bytes) }));
    }
    app
}

async fn upgrade(State(frontend): State<Arc<dyn Frontend>>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(40 << 20).max_frame_size(40 << 20).on_upgrade(move |socket| session(socket, frontend)).into_response()
}

/// Remote frames never run faster than this.
const MIN_FRAME: Duration = Duration::from_millis(33);

async fn session(mut socket: WebSocket, frontend: Arc<dyn Frontend>) {
    let ctx = egui::Context::default();
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    let wake = Arc::new(Notify::new());
    let due: Arc<Mutex<Option<Instant>>> = Arc::default();
    {
        let (wake, due) = (wake.clone(), due.clone());
        ctx.set_request_repaint_callback(move |info| {
            let at = Instant::now() + info.delay;
            let mut d = due.lock().unwrap();
            if d.is_none_or(|old| at < old) {
                *d = Some(at);
            }
            wake.notify_one();
        });
    }
    let mut ui = frontend.open(&ctx);
    let mut encoder = Encoder::default();
    let start = Instant::now();
    let mut input = egui::RawInput::default();
    let mut ready = false;
    let mut last = Instant::now() - MIN_FRAME;
    loop {
        let deadline = due.lock().unwrap().map(|d| d.max(last + MIN_FRAME));
        let sleep = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending().await,
            }
        };
        let frame;
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(bytes))) => {
                    match unpack::<ToHost>(&bytes) {
                        Ok(ToHost::Input(raw)) => merge(&mut input, raw),
                        Ok(ToHost::File(reply)) => ui.file_reply(reply),
                        Err(_) => continue,
                    }
                    ready = true;
                    frame = true;
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Text(_))) => continue,
                _ => break,
            },
            _ = wake.notified() => continue,
            _ = sleep => frame = true,
        }
        if !frame || !ready {
            continue;
        }
        *due.lock().unwrap() = None;
        let mut raw = input.take();
        raw.time = Some(start.elapsed().as_secs_f64());
        // `take` moves persistent fields too; the browser only resends them on change.
        raw.screen_rect = input.screen_rect.or(raw.screen_rect);
        input.screen_rect = raw.screen_rect;
        input.viewports = raw.viewports.clone();
        let out = ctx.run_ui(raw, |root| ui.ui(root));
        last = Instant::now();
        if let Some(v) = out.viewport_output.get(&egui::ViewportId::ROOT) {
            if v.repaint_delay < Duration::from_secs(3600) {
                let at = Instant::now() + v.repaint_delay;
                let mut d = due.lock().unwrap();
                if d.is_none_or(|old| at < old) {
                    *d = Some(at);
                }
            }
        }
        let bytes = encoder.encode_with_file(out, ui.file_dialog());
        if socket.send(Message::Binary(bytes.into())).await.is_err() {
            break;
        }
    }
}

/// Keep the persistent parts of the input; append events.
fn merge(into: &mut egui::RawInput, raw: egui::RawInput) {
    into.events.extend(raw.events);
    into.screen_rect = raw.screen_rect.or(into.screen_rect);
    if !raw.viewports.is_empty() {
        into.viewports = raw.viewports;
    }
    into.focused = raw.focused;
    into.system_theme = raw.system_theme;
    into.max_texture_side = raw.max_texture_side.or(into.max_texture_side);
    into.safe_area_insets = raw.safe_area_insets.or(into.safe_area_insets);
    into.hovered_files = raw.hovered_files;
}

/// Browsers run 32-bit wasm: `usize::MAX` defaults (e.g. unlimited rows) must fit.
fn portable(g: &Arc<Galley>) -> Arc<Galley> {
    if g.job.wrap.max_rows <= u32::MAX as usize {
        return g.clone();
    }
    let mut copy = (**g).clone();
    Arc::make_mut(&mut copy.job).wrap.max_rows = u32::MAX as usize;
    Arc::new(copy)
}

/// Turns egui output into compact frames.
#[derive(Default)]
pub struct Encoder {
    prev: Vec<u64>,
    galleys: HashMap<u64, u64>,
    by_ptr: HashMap<usize, (Weak<Galley>, u64)>,
    frame: u64,
}
fn hash_of<T: serde::Serialize>(v: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    postcard::to_allocvec(v).unwrap_or_default().hash(&mut h);
    h.finish()
}
impl Encoder {
    pub fn encode(&mut self, out: egui::FullOutput) -> Vec<u8> {
        self.encode_with_file(out, None)
    }
    pub fn encode_with_file(&mut self, mut out: egui::FullOutput, file_dialog: Option<crate::FileDialog>) -> Vec<u8> {
        self.frame += 1;
        let frame_no = self.frame;
        let mut fresh: Vec<(u64, Arc<Galley>)> = Vec::new();
        let (galleys, by_ptr) = (&mut self.galleys, &mut self.by_ptr);
        let mut galley_id = |g: &Arc<Galley>| {
            let ptr = Arc::as_ptr(g) as usize;
            let id = match by_ptr.get(&ptr).filter(|(w, _)| w.strong_count() > 0) {
                Some((_, id)) => *id,
                None => {
                    let id = hash_of(&**g);
                    by_ptr.insert(ptr, (Arc::downgrade(g), id));
                    id
                }
            };
            if galleys.insert(id, frame_no).is_none() {
                fresh.push((id, portable(g)));
            }
            id
        };
        let mut items: Vec<Item> = Vec::with_capacity(out.shapes.len());
        for c in out.shapes {
            wire(c.shape, c.clip_rect, c.transform_after_tessellation, &mut galley_id, &mut items);
        }
        self.by_ptr.retain(|_, (w, _)| w.strong_count() > 0);
        let forget: Vec<u64> = self.galleys.iter().filter(|(_, used)| frame_no - **used > 600).map(|(id, _)| *id).collect();
        for id in &forget {
            self.galleys.remove(id);
        }
        let hashes: Vec<u64> = items.iter().map(hash_of).collect();
        let mut index: HashMap<u64, u32> = HashMap::new();
        for (i, h) in self.prev.iter().enumerate().rev() {
            index.insert(*h, i as u32);
        }
        let mut ops = Vec::new();
        let mut i = 0;
        let mut pending: Vec<Item> = Vec::new();
        let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
        while i < hashes.len() {
            if let Some(&from) = index.get(&hashes[i]) {
                let mut len = 0;
                while i + len < hashes.len() && (from as usize + len) < self.prev.len() && self.prev[from as usize + len] == hashes[i + len] {
                    len += 1;
                }
                if !pending.is_empty() {
                    ops.push(ShapeOp::New(std::mem::take(&mut pending)));
                }
                ops.push(ShapeOp::Keep { from, len: len as u32 });
                i += len;
            } else {
                pending.push(items[i].take().unwrap());
                i += 1;
            }
        }
        if !pending.is_empty() {
            ops.push(ShapeOp::New(pending));
        }
        self.prev = hashes;
        // The browser applies the deltas: mark them handled here.
        let textures = out
            .textures_delta
            .set
            .drain()
            .flat_map(|(id, deltas)| deltas.into_iter().map(move |d| Texture { id: TexId::from(id), image: image(&d.image), options: d.options, pos: d.pos }))
            .collect();
        let free = out.textures_delta.free.drain().map(TexId::from).collect();
        let mut platform = out.platform_output;
        platform.events.clear();
        pack(&Frame { file_dialog, ppp: out.pixels_per_point, textures, free, galleys: fresh, forget, shapes: ops, platform })
    }
}
