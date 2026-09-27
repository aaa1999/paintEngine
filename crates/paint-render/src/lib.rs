//! paint-render：[`Renderer`] 的软件实现。
//!
//! dab 盖章（圆头、硬度、压感）与图层脏区合成均为手写标量循环，
//! M1 不引入第三方光栅化依赖；SIMD 与双线性采样留待后续优化。

mod composite;
mod stamp;

pub use composite::composite;
pub use stamp::stamp_dabs;

use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::history::StrokeRecorder;
use paint_core::layer::Layer;
use paint_core::render::Renderer;
use paint_core::stroke::Dab;

/// 纯 CPU 渲染器。
#[derive(Default)]
pub struct SoftwareRenderer;

impl SoftwareRenderer {
    pub fn new() -> Self {
        Self
    }
}

impl Renderer for SoftwareRenderer {
    fn stamp_dabs(&mut self, layer: &mut Layer, dabs: &[Dab], recorder: &mut StrokeRecorder) {
        stamp::stamp_dabs(layer, dabs, recorder);
    }

    fn composite(&mut self, doc: &Document, target: &mut [u8], width: u32, dirty: Rect) {
        composite::composite(doc, target, width, dirty);
    }
}
