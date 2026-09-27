use crate::color::Color;
use crate::document::Document;
use crate::geometry::Rect;
use crate::history::StrokeRecorder;
use crate::layer::Layer;
use crate::stroke::{Dab, RoundBrush};
use crate::tile::TileGrid;

/// 渲染后端。软件实现见 paint-render；GPU 合成（P2）与 GPU 盖章
/// （P4）各自提供本 trait 的实现，引擎与后端解耦。
///
/// 注意：trait 定义在 paint-core（消费方）而非 paint-render，
/// 保证引擎不依赖任何具体渲染实现。
pub trait Renderer: Send {
    /// 笔刷盖章热路径（目标为任意瓦片网格：像素层或图层蒙版）。
    /// 写入瓦片前必须经 `recorder.capture` 记录旧快照（撤销依赖）。
    fn stamp_dabs(&mut self, grid: &mut TileGrid, dabs: &[Dab], recorder: &mut StrokeRecorder);

    /// 合成可见图层到目标缓冲（RGBA8 预乘，行主序，尺寸 `width` ×
    /// `target.len()/width/4`），只处理 `dirty` 区域。
    /// `background: None` 输出保持透明（PNG 导出等场合）。
    fn composite(
        &mut self,
        doc: &Document,
        target: &mut [u8],
        width: u32,
        dirty: Rect,
        background: Option<Color>,
    );

    /// 把 `src`（按其 opacity/blend_mode/visible）合入 `dst`。
    /// merge_down/flatten/导入用。`recorder` 记录 dst 被改写的瓦片
    /// （撤销依赖），不需要时传 `&mut StrokeRecorder` 空对象亦可。
    fn merge_layers(&mut self, dst: &mut Layer, src: &Layer, recorder: &mut StrokeRecorder);
}

/// 呈现目标。平台壳实现：桌面 softbuffer、Web canvas、
/// 移动端原生层。M1 只有 CPU 路径；GPU 纹理路径在 M2 扩展。
pub trait Surface {
    /// `rgba` 为完整帧缓冲（可能与上次呈现有差异的仅 `dirty` 区域）。
    fn present_cpu(&mut self, rgba: &[u8], size: (u32, u32), dirty: Option<Rect>);
}

/// 引擎初始配置。
pub struct EngineConfig {
    pub background: Color,
    /// 撤销历史内存限额（字节）。
    pub undo_memory_limit: usize,
    pub brush: RoundBrush,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            background: Color::WHITE,
            undo_memory_limit: 256 * 1024 * 1024,
            brush: RoundBrush::default(),
        }
    }
}
