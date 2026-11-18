#![cfg(target_arch = "wasm32")]
use homeport_remote::*;
use wasm_bindgen_test::*;

#[derive(serde::Deserialize)]
struct Head {
    _file: Option<FileDialog>,
    _ppp: f32,
    _textures: Vec<Texture>,
    _free: Vec<TexId>,
}
#[derive(serde::Deserialize)]
struct WithGalleys {
    _file: Option<FileDialog>,
    _ppp: f32,
    _textures: Vec<Texture>,
    _free: Vec<TexId>,
    _galleys: Vec<(u64, std::sync::Arc<egui::epaint::Galley>)>,
}

#[wasm_bindgen_test]
fn decode_native_frame() {
    let bytes = include_bytes!("frame.bin");
    let raw = miniz_oxide::inflate::decompress_to_vec(bytes).unwrap();
    let head: Result<Head, _> = postcard::from_bytes(&raw);
    assert!(head.is_ok(), "head: {:?}", head.err());
    let g: Result<WithGalleys, _> = postcard::from_bytes(&raw);
    assert!(g.is_ok(), "galleys: {:?}", g.err());
    let f: Result<Frame, _> = postcard::from_bytes(&raw);
    assert!(f.is_ok(), "frame: {:?}", f.err());
}
