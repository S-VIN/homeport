//! Rebuild the host's shapes from frames and tessellate them locally.
use crate::{Frame, Item, ShapeOp, TexId, image_data, unwire};
use egui::{
    TextureId,
    epaint::{ClippedPrimitive, Galley, Primitive, Tessellator},
};
use std::{collections::HashMap, sync::Arc};

#[derive(Default)]
pub struct Scene {
    items: Vec<Item>,
    galleys: HashMap<u64, Arc<Galley>>,
    textures: HashMap<TexId, TextureId>,
    font_size: [usize; 2],
    pub primitives: Vec<ClippedPrimitive>,
}
impl Scene {
    /// Forget everything (new connection) and free the textures it allocated.
    pub fn clear(&mut self, ctx: &egui::Context) {
        let tex = ctx.tex_manager();
        let mut tex = tex.write();
        for (_, id) in self.textures.drain() {
            tex.free(id);
        }
        drop(tex);
        *self = Self::default();
    }
    pub fn apply(&mut self, ctx: &egui::Context, frame: Frame) -> egui::PlatformOutput {
        let tex = ctx.tex_manager();
        let mut tex = tex.write();
        for t in frame.textures {
            let data = image_data(t.image);
            if t.id == TexId::Managed(0) && t.pos.is_none() {
                self.font_size = data.size();
            }
            match (self.textures.get(&t.id), t.pos) {
                (Some(id), _) => tex.set(
                    *id,
                    egui::epaint::ImageDelta {
                        image: data,
                        options: t.options,
                        pos: t.pos,
                    },
                ),
                (None, None) => {
                    let id = tex.alloc(format!("remote-{:?}", t.id), data, t.options);
                    self.textures.insert(t.id, id);
                }
                (None, Some(_)) => {}
            }
        }
        for g in frame.forget {
            self.galleys.remove(&g);
        }
        self.galleys.extend(frame.galleys);
        let prev = std::mem::take(&mut self.items);
        for op in frame.shapes {
            match op {
                ShapeOp::Keep { from, len } => {
                    let (from, len) = (from as usize, len as usize);
                    if from + len <= prev.len() {
                        self.items.extend_from_slice(&prev[from..from + len]);
                    }
                }
                ShapeOp::New(items) => self.items.extend(items),
            }
        }
        let shapes: Vec<_> = self
            .items
            .iter()
            .filter_map(|i| unwire(i, |id| self.galleys.get(&id).cloned()))
            .collect();
        let size = if self.font_size[0] == 0 {
            [1, 1]
        } else {
            self.font_size
        };
        let mut prims = Tessellator::new(frame.ppp, Default::default(), size, Vec::new())
            .tessellate_shapes(shapes);
        for p in &mut prims {
            if let Primitive::Mesh(m) = &mut p.primitive
                && let Some(id) = self.textures.get(&TexId::from(m.texture_id))
            {
                m.texture_id = *id;
            }
        }
        self.primitives = prims;
        for t in frame.free {
            if let Some(id) = self.textures.remove(&t) {
                tex.free(id);
            }
        }
        drop(tex);
        frame.platform
    }
}
