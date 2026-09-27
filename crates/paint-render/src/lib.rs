//! paint-render：[`Renderer`] 的软件实现。
//!
//! dab 盖章（圆头、硬度、压感、橡皮）、图层脏区合成（12 种混合
//! 模式）与图层合并均为手写标量循环，M1/M2 不引入第三方光栅化
//! 依赖；SIMD 与双线性采样留待后续优化。

mod composite;
mod merge;
mod stamp;

pub use composite::composite;
pub use merge::merge_layers;
pub use stamp::stamp_dabs;

use paint_core::color::Color;
use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::history::StrokeRecorder;
use paint_core::layer::Layer;
use paint_core::render::Renderer;
use paint_core::stroke::Dab;
use paint_core::tile::TileGrid;

/// 纯 CPU 渲染器。
#[derive(Default)]
pub struct SoftwareRenderer;

impl SoftwareRenderer {
    pub fn new() -> Self {
        Self
    }
}

impl Renderer for SoftwareRenderer {
    fn stamp_dabs(
        &mut self,
        grid: &mut TileGrid,
        dabs: &[Dab],
        clip: Option<&TileGrid>,
        recorder: &mut StrokeRecorder,
    ) {
        stamp::stamp_dabs(grid, dabs, clip, recorder);
    }

    fn composite(
        &mut self,
        doc: &Document,
        target: &mut [u8],
        width: u32,
        dirty: Rect,
        background: Option<Color>,
    ) {
        composite::composite(doc, target, width, dirty, background);
    }

    fn merge_layers(&mut self, dst: &mut Layer, src: &Layer, recorder: &mut StrokeRecorder) {
        merge::merge_layers(dst, src, recorder);
    }
}
