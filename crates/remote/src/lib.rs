//! Remote egui: the UI runs on the host, the browser only paints.
//!
//! Host → browser: [`Frame`] with texture changes, new text layouts, a delta of
//! the shape list and platform output (cursor, clipboard, IME). Browser → host:
//! [`ToHost::Input`] with the raw egui input of a frame. The protocol never
//! depends on the application: new screens and plugins need no client update.
use egui::{
    epaint::{
        textures::TextureOptions, BandShape, CircleShape, ClippedShape, CubicBezierShape, EllipseShape,
        Galley, Mesh, PathShape, QuadraticBezierShape, RectShape, Shape, TextShape,
    },
    emath::TSTransform,
    Color32, ImageData, Pos2, Rect, Stroke, TextureId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(target_arch = "wasm32")]
mod client;
mod scene;
pub use scene::Scene;
#[cfg(not(target_arch = "wasm32"))]
pub mod host;

#[derive(Serialize, Deserialize)]
pub enum ToHost {
    Input(egui::RawInput),
    File(FileReply),
}

pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub enum FileDialog {
    Save { name: String, bytes: Vec<u8> },
    Open,
}

#[derive(Serialize, Deserialize)]
pub enum FileReply {
    Finished,
    Opened { name: String, bytes: Vec<u8> },
    Failed(String),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TexId {
    Managed(u64),
    User(u64),
}
impl From<TextureId> for TexId {
    fn from(t: TextureId) -> Self {
        match t {
            TextureId::Managed(n) => Self::Managed(n),
            TextureId::User(n) => Self::User(n),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum Image {
    /// sRGBA, premultiplied.
    Color { size: [usize; 2], rgba: Vec<u8> },
    /// Premultiplied white with alpha (the font atlas): one byte per texel, lossless.
    Font { size: [usize; 2], coverage: Vec<u8> },
}

#[derive(Serialize, Deserialize)]
pub struct Texture {
    pub id: TexId,
    pub image: Image,
    pub options: TextureOptions,
    pub pos: Option<[usize; 2]>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub enum WireShape {
    Circle(CircleShape),
    Ellipse(EllipseShape),
    Line { points: [Pos2; 2], stroke: Stroke },
    Path(PathShape),
    Rect(RectShape),
    Band(BandShape),
    Text {
        pos: Pos2,
        galley: u64,
        underline: Stroke,
        fallback_color: Color32,
        override_text_color: Option<Color32>,
        opacity_factor: f32,
        angle: f32,
    },
    Mesh(Mesh),
    Quadratic(QuadraticBezierShape),
    Cubic(CubicBezierShape),
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Item {
    pub clip: Rect,
    pub shape: WireShape,
    pub transform: TSTransform,
}

#[derive(Serialize, Deserialize)]
pub enum ShapeOp {
    /// Reuse `len` items of the previous frame starting at `from`.
    Keep { from: u32, len: u32 },
    New(Vec<Item>),
}

#[derive(Serialize, Deserialize)]
pub struct Frame {
    pub file_dialog: Option<FileDialog>,
    pub ppp: f32,
    pub textures: Vec<Texture>,
    pub free: Vec<TexId>,
    pub galleys: Vec<(u64, Arc<Galley>)>,
    pub forget: Vec<u64>,
    pub shapes: Vec<ShapeOp>,
    pub platform: egui::PlatformOutput,
}

pub fn pack<T: Serialize>(value: &T) -> Vec<u8> {
    let bytes = postcard::to_allocvec(value).expect("serialize");
    miniz_oxide::deflate::compress_to_vec(&bytes, 1)
}
pub fn unpack<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, String> {
    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(bytes, 256 << 20).map_err(|e| format!("{e:?}"))?;
    postcard::from_bytes(&raw).map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub(crate) fn image(data: &ImageData) -> Image {
    match data {
        ImageData::Color(c) if c.pixels.iter().all(|p| p.r() == p.a() && p.g() == p.a() && p.b() == p.a()) => {
            Image::Font { size: c.size, coverage: c.pixels.iter().map(|p| p.a()).collect() }
        }
        ImageData::Color(c) => Image::Color { size: c.size, rgba: c.pixels.iter().flat_map(|p| p.to_array()).collect() },
    }
}
#[allow(dead_code)]
pub(crate) fn image_data(i: Image) -> ImageData {
    match i {
        Image::Color { size, rgba } => ImageData::Color(Arc::new(egui::ColorImage::new(
            size,
            rgba.as_chunks::<4>().0.iter().map(|[r, g, b, a]| Color32::from_rgba_premultiplied(*r, *g, *b, *a)).collect(),
        ))),
        Image::Font { size, coverage } => ImageData::Color(Arc::new(egui::ColorImage::new(
            size,
            coverage.into_iter().map(|v| Color32::from_rgba_premultiplied(v, v, v, v)).collect(),
        ))),
    }
}

/// Convert an epaint shape; galleys are replaced by ids from `galley`.
#[allow(dead_code)]
pub(crate) fn wire(shape: Shape, clip: Rect, transform: TSTransform, galley: &mut impl FnMut(&Arc<Galley>) -> u64, out: &mut Vec<Item>) {
    let s = match shape {
        Shape::Noop | Shape::Callback(_) => return,
        Shape::Vec(list) => {
            for s in list {
                wire(s, clip, transform, galley, out);
            }
            return;
        }
        Shape::Circle(c) => WireShape::Circle(c),
        Shape::Ellipse(e) => WireShape::Ellipse(e),
        Shape::LineSegment { points, stroke } => WireShape::Line { points, stroke },
        Shape::Path(p) => WireShape::Path(p),
        Shape::Rect(r) => WireShape::Rect(r),
        Shape::Band(b) => WireShape::Band(b),
        Shape::Text(t) => WireShape::Text {
            pos: t.pos,
            galley: galley(&t.galley),
            underline: t.underline,
            fallback_color: t.fallback_color,
            override_text_color: t.override_text_color,
            opacity_factor: t.opacity_factor,
            angle: t.angle,
        },
        Shape::Mesh(m) => WireShape::Mesh((*m).clone()),
        Shape::QuadraticBezier(q) => WireShape::Quadratic(q),
        Shape::CubicBezier(c) => WireShape::Cubic(c),
    };
    out.push(Item { clip, shape: s, transform });
}

/// Back to an epaint shape (text needs the cached galley).
#[allow(dead_code)]
pub(crate) fn unwire(item: &Item, galley: impl Fn(u64) -> Option<Arc<Galley>>) -> Option<ClippedShape> {
    let shape = match &item.shape {
        WireShape::Circle(c) => Shape::Circle(*c),
        WireShape::Ellipse(e) => Shape::Ellipse(*e),
        WireShape::Line { points, stroke } => Shape::LineSegment { points: *points, stroke: *stroke },
        WireShape::Path(p) => Shape::Path(p.clone()),
        WireShape::Rect(r) => Shape::Rect(r.clone()),
        WireShape::Band(b) => Shape::Band(b.clone()),
        WireShape::Text { pos, galley: id, underline, fallback_color, override_text_color, opacity_factor, angle } => {
            let mut t = TextShape::new(*pos, galley(*id)?, *fallback_color);
            t.underline = *underline;
            t.override_text_color = *override_text_color;
            t.opacity_factor = *opacity_factor;
            t.angle = *angle;
            Shape::Text(t)
        }
        WireShape::Mesh(m) => Shape::Mesh(Arc::new(m.clone())),
        WireShape::Quadratic(q) => Shape::QuadraticBezier(q.clone()),
        WireShape::Cubic(c) => Shape::CubicBezier(c.clone()),
    };
    Some(ClippedShape { clip_rect: item.clip, shape, transform_after_tessellation: item.transform })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use egui::epaint::Primitive;
    #[test]
    fn frames_round_trip_with_shape_reuse() {
        let host = egui::Context::default();
        let client = egui::Context::default();
        let mut enc = host::Encoder::default();
        let mut scene = Scene::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400., 300.))),
            ..Default::default()
        };
        for i in 0..4 {
            let out = host.run_ui(input(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.label("hello");
                    let _ = ui.button(format!("button {}", i / 2));
                });
            });
            let shapes = out.shapes.len();
            let bytes = enc.encode(out);
            let frame: Frame = unpack(&bytes).unwrap();
            let _ = scene.apply(&client, frame);
            let verts: usize = scene.primitives.iter().map(|p| match &p.primitive { Primitive::Mesh(m) => m.vertices.len(), _ => 0 }).sum();
            assert!(shapes == 0 || verts > 0, "frame {i}: {shapes} shapes but nothing to paint");
        }
    }
}
