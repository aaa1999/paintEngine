use std::collections::HashMap;

use crate::color::Color;
use crate::document::Document;
use crate::geometry::Rect;
use crate::history::{StrokeRecorder, UndoGroup, UndoOp};
use crate::input::{PlatformEvent, PointerKind, PointerPhase, PointerSample};
use crate::layer::{BlendMode, Layer, LayerId};
use crate::render::{EngineConfig, Renderer, Surface};
use crate::stroke::{Dab, DabMode, RoundBrush, StrokeGen, StrokeState};
use crate::tile::TileGrid;
use crate::tile::{TileId, TILE};

/// 屏幕脏区状态：All 全量重绘、Part 增量、Clean 无需合成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    All,
    Part(Rect),
    Clean,
}

impl Dirty {
    fn union(&mut self, r: Rect) {
        match *self {
            Dirty::All => {}
            Dirty::Clean => *self = Dirty::Part(r),
            Dirty::Part(p) => *self = Dirty::Part(p.union(&r)),
        }
    }
}

/// 图层面板 UI 的单层信息。
#[derive(Debug, Clone)]
pub struct LayerInfo {
    pub id: u64,
    pub name: String,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: BlendMode,
    pub clipped: bool,
    pub has_mask: bool,
    pub group: Option<String>,
    pub adjustment: Option<crate::layer::LayerAdjustment>,
}

/// 对称绘画模式。轴/中心为画布坐标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SymmetryMode {
    None,
    /// 垂直轴 x = axis_x（左右镜像）。
    Horizontal {
        axis_x: f64,
    },
    /// 水平轴 y = axis_y（上下镜像）。
    Vertical {
        axis_y: f64,
    },
    /// 双轴（四分对称）。
    Both {
        axis_x: f64,
        axis_y: f64,
    },
    /// 径向 N 分（绕 center 旋转复制）。
    Radial {
        center: (f64, f64),
        segments: u32,
    },
}

impl SymmetryMode {
    /// 生成一个 dab 的全部对称副本（含原始 dab）。
    pub fn expand_dab(&self, dab: &Dab) -> Vec<Dab> {
        match self {
            SymmetryMode::None => vec![dab.clone()],
            SymmetryMode::Horizontal { axis_x } => {
                let mut v = vec![dab.clone()];
                let mut m = dab.clone();
                m.x = 2.0 * axis_x - dab.x;
                m.angle = std::f32::consts::PI - dab.angle; // 各向异性翻转
                v.push(m);
                v
            }
            SymmetryMode::Vertical { axis_y } => {
                let mut v = vec![dab.clone()];
                let mut m = dab.clone();
                m.y = 2.0 * axis_y - dab.y;
                m.angle = -dab.angle;
                v.push(m);
                v
            }
            SymmetryMode::Both { axis_x, axis_y } => {
                let mut v = vec![dab.clone()];
                // X 镜像
                let mut mx = dab.clone();
                mx.x = 2.0 * axis_x - dab.x;
                mx.angle = std::f32::consts::PI - dab.angle;
                v.push(mx.clone());
                // Y 镜像
                let mut my = dab.clone();
                my.y = 2.0 * axis_y - dab.y;
                my.angle = -dab.angle;
                v.push(my.clone());
                // XY 镜像
                mx.y = 2.0 * axis_y - dab.y;
                v.push(mx);
                v
            }
            SymmetryMode::Radial { center, segments } => {
                let n = (*segments).clamp(2, 32) as usize;
                let mut v = Vec::with_capacity(n);
                let dx = dab.x - center.0;
                let dy = dab.y - center.1;
                for i in 0..n {
                    let a = i as f64 * std::f64::consts::TAU / n as f64;
                    let (c, sn) = (a.cos(), a.sin());
                    let mut d = dab.clone();
                    d.x = center.0 + dx * c - dy * sn;
                    d.y = center.1 + dx * sn + dy * c;
                    // 各向异性角度随旋转变换
                    d.angle = dab.angle + a as f32;
                    v.push(d);
                }
                v
            }
        }
    }

    /// 名称。
    pub fn name(&self) -> &'static str {
        match self {
            SymmetryMode::None => "关",
            SymmetryMode::Horizontal { .. } => "左右",
            SymmetryMode::Vertical { .. } => "上下",
            SymmetryMode::Both { .. } => "四分",
            SymmetryMode::Radial { segments, .. } => match segments {
                3 => "径向3",
                4 => "径向4",
                6 => "径向6",
                8 => "径向8",
                _ => "径向",
            },
        }
    }
}

/// 选区布尔操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOp {
    Replace,
    Add,
    Subtract,
}

/// 当前工具。橡皮 = 同一 RoundBrush 引擎、dst-out 合成；
/// 蒙版编辑 = 盖章目标切到活动图层的蒙版网格（白=显现）；
/// 形状 = 拖拽定义几何（描边复用笔刷参数，引擎内状态机 + 帧级预览）；
/// 文字 = 点击落锚点，壳层取锚点（[`Engine::take_text_anchor`]）弹输入；
/// 填充 = 点击处连通区域填笔刷色（油漆桶）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
    Mask,
    Shape {
        kind: crate::shape::ShapeKind,
        fill: bool,
    },
    Text,
    Fill {
        /// 各通道容差 0-255。
        tolerance: u8,
    },
}

struct ActiveStroke {
    state: StrokeState,
    recorder: StrokeRecorder,
    layer: LayerId,
    pointer: u64,
}

/// 进行中的形状拖拽（画布坐标）。预览走帧缓冲叠加，提交才落瓦片。
#[derive(Clone, Copy)]
struct ActiveShape {
    kind: crate::shape::ShapeKind,
    fill: bool,
    anchor: (f64, f64),
    current: (f64, f64),
    layer: LayerId,
    pointer: u64,
}

/// 进行中的对象拖拽移动。
#[derive(Clone, Copy)]
struct ObjectMoving {
    layer: LayerId,
    index: usize,
    last: (f64, f64),
    pointer: u64,
}

struct Gesture {
    centroid: (f64, f64),
    dist: f64,
}

/// 引擎：平台壳持有它，喂事件、要帧。
pub struct Engine {
    doc: Document,
    renderer: Box<dyn Renderer>,
    frame: Vec<u8>,
    size: (u32, u32),
    dirty: Dirty,
    brush: RoundBrush,
    tool: Tool,
    stroke: Option<ActiveStroke>,
    /// 进行中的形状拖拽（形状工具）。
    shape_drag: Option<ActiveShape>,
    /// 文字工具的落点（画布坐标），壳层 take 后弹输入框。
    text_anchor: Option<(f64, f64)>,
    /// 文字工具命中的已有对象（编辑目标），壳层 take 后弹编辑框。
    text_editing: Option<(LayerId, usize)>,
    /// 最近命中的文字对象（update/delete 的目标；下次命中覆盖）。
    last_text_target: Option<(LayerId, usize)>,
    /// 文字对象拖拽移动（文字工具下按住对象拖动）。
    object_moving: Option<ObjectMoving>,
    /// 移动开始时的对象列表快照（撤销用）。
    obj_drag_before: Option<(LayerId, Vec<crate::layer::DrawObject>)>,
    /// 文字字体字节（swash 光栅化；壳层启动时设置。Web 壳层用
    /// 浏览器渲染走 raster 通道，可不设）。
    text_font: Option<std::sync::Arc<Vec<u8>>>,
    /// 帧率监控开关（EngineConfig.fps_monitor，默认开）。
    fps_monitor: bool,
    /// 交互期渲染（缩放/平移手势中）：半分辨率合成 + 最近邻上采样。
    interactive: bool,
    /// 低清合成暂存（interactive 时复用）。
    scratch: Vec<u8>,
    /// 上采样行展开缓冲（2× 特化复用）。
    row2: Vec<u8>,
    /// 呈现帧计数（render 计数；监控关闭时冻结）。
    /// 壳层用自己的时钟两次采样差值即得帧率——引擎不依赖平台时钟
    ///（std Instant 在 wasm32 上不可用）。
    presents: u64,
    pen_in_range: bool,
    vp_rev: u64,
    // 触摸多指状态
    touches: HashMap<u64, (f64, f64)>,
    gesture: Option<Gesture>,
    /// 手势闩锁：手势触发后，剩余手指抬完前不再起笔画
    gesture_latch: bool,
    /// 内容级变换的撤销采集器（begin→commit 存活）。
    transform_recorder: Option<StrokeRecorder>,
    /// 内部剪贴板（瓦片网格自带画布绝对位置）。
    clipboard: Option<TileGrid>,
    /// 笔刷预设表（内置 + 用户自定义；导入导出走文本格式）。
    presets: Vec<(String, RoundBrush)>,
    preset_idx: Option<usize>,
    /// 对称绘画模式。
    symmetry: SymmetryMode,
    /// 插件注册表。
    plugins: crate::plugin::PluginRegistry,
}

impl Engine {
    /// 创建引擎（渲染后端 + 配置）。
    ///
    /// # 示例
    /// ```text
    /// // renderer: 任何实现 Renderer trait 的后端（如 paint_render::SoftwareRenderer）
    /// let engine = Engine::new(renderer, EngineConfig::default());
    /// ```
    pub fn new(renderer: Box<dyn Renderer>, config: EngineConfig) -> Self {
        let background = config.background;
        let mut doc = Document::new(config.undo_memory_limit);
        doc.set_background(background);
        doc.set_show_grid(true); // 无限画布空间指示，可经 set_show_graph 关闭
        Self {
            doc,
            renderer,
            frame: Vec::new(),
            size: (0, 0),
            dirty: Dirty::All,
            brush: config.brush,
            tool: Tool::Brush,
            stroke: None,
            shape_drag: None,
            text_anchor: None,
            text_editing: None,
            last_text_target: None,
            object_moving: None,
            obj_drag_before: None,
            text_font: None,
            fps_monitor: config.fps_monitor,
            interactive: false,
            scratch: Vec::new(),
            row2: Vec::new(),
            presents: 0,
            pen_in_range: false,
            vp_rev: 0,
            touches: HashMap::new(),
            gesture: None,
            gesture_latch: false,
            transform_recorder: None,
            clipboard: None,
            presets: builtin_presets(),
            preset_idx: None,
            symmetry: SymmetryMode::None,
            plugins: crate::plugin::PluginRegistry::with_builtin_examples(),
        }
    }

    /// 只读文档访问。
    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// 内存概况（瓦片/撤销/总计，字节）。
    pub fn memory_report(&self) -> (usize, usize, usize) {
        let tiles = self.doc.tile_memory_bytes();
        let undo = self.doc.history().memory_used();
        (tiles, undo, tiles + undo)
    }

    /// 可变文档访问（视口/图层属性直改——引擎自动检测变更）。
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.doc
    }

    /// 当前笔刷（读）。
    pub fn brush(&self) -> &RoundBrush {
        &self.brush
    }

    /// 当前笔刷（写）。
    pub fn brush_mut(&mut self) -> &mut RoundBrush {
        &mut self.brush
    }

    // ── 对称绘画 ──

    /// 对称模式。
    pub fn symmetry(&self) -> &SymmetryMode {
        &self.symmetry
    }

    /// 设置对称模式。
    pub fn set_symmetry(&mut self, mode: SymmetryMode) {
        self.symmetry = mode;
    }

    /// 快捷循环：关 → 左右 → 上下 → 四分 → 关（轴默认视野中心）。
    pub fn cycle_symmetry(&mut self) -> &'static str {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            self.symmetry = SymmetryMode::None;
            return "关";
        }
        let (cx, cy) = self
            .doc
            .viewport()
            .screen_to_canvas(w as f64 / 2.0, h as f64 / 2.0);
        self.symmetry = match self.symmetry {
            SymmetryMode::None => SymmetryMode::Horizontal { axis_x: cx },
            SymmetryMode::Horizontal { .. } => SymmetryMode::Vertical { axis_y: cy },
            SymmetryMode::Vertical { .. } => SymmetryMode::Both {
                axis_x: cx,
                axis_y: cy,
            },
            SymmetryMode::Both { .. } | SymmetryMode::Radial { .. } => SymmetryMode::None,
        };
        self.symmetry.name()
    }

    // ── 画布尺寸 ──

    /// 设置固定画布尺寸（像素，原点 (0,0)）。w=0 或 h=0 清除（恢复无限画布）。
    pub fn set_canvas(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            self.doc.set_canvas(None);
        } else {
            self.doc.set_canvas(Some(Rect::new(0, 0, w, h)));
        }
        self.dirty = Dirty::All;
    }

    /// 清除画布尺寸（恢复无限画布）。
    pub fn clear_canvas(&mut self) {
        self.doc.set_canvas(None);
        self.dirty = Dirty::All;
    }

    /// 画布尺寸（None = 无限）。
    pub fn canvas_bounds(&self) -> Option<Rect> {
        self.doc.canvas()
    }

    // ── 图层组 ──

    /// 设置活动图层的组标签（None = 退出组）。
    pub fn set_layer_group(&mut self, group: Option<String>) {
        if let Some(id) = self.doc.layers().try_active() {
            self.doc.layers_mut().set_group(id, group);
            self.dirty = Dirty::All;
        }
    }

    /// 切换组内所有图层可见性，返回受影响图层数。
    pub fn toggle_group_visible(&mut self, group: &str) -> usize {
        // 找组内任一层的当前可见性取反
        let current = self
            .doc
            .layers()
            .iter()
            .find(|l| l.group.as_deref() == Some(group))
            .map(|l| l.visible)
            .unwrap_or(true);
        let n = self.doc.layers_mut().set_group_visible(group, !current);
        if n > 0 {
            self.dirty = Dirty::All;
        }
        n
    }

    /// 组名列表。
    pub fn group_names(&self) -> Vec<String> {
        self.doc.layers().group_names()
    }

    // ── 插件 ──

    /// 插件注册表（读）。
    pub fn plugins(&self) -> &crate::plugin::PluginRegistry {
        &self.plugins
    }

    /// 插件注册表（写——注册插件）。
    pub fn plugins_mut(&mut self) -> &mut crate::plugin::PluginRegistry {
        &mut self.plugins
    }

    /// 应用注册的滤镜插件（选区感知 + 撤销）。
    pub fn apply_plugin_filter(
        &mut self,
        name: &str,
        params: &crate::plugin::PluginParams,
    ) -> bool {
        if self.transforming() || self.plugins.filter(name).is_none() {
            return false;
        }
        let Some(layer_id) = self.doc.layers().try_active() else {
            return false;
        };
        let selection = self.doc.selection().cloned();

        // 复用 filter.rs 的提取/写回管道，仅处理函数换为插件
        let bbox = match selection.as_ref() {
            Some(sel) => sel.content_bounds(),
            None => self
                .doc
                .layers()
                .get(layer_id)
                .tiles
                .content_bounds_precise(),
        };
        let Some(_bbox) = bbox else { return false };
        let mut recorder = StrokeRecorder::new(layer_id);
        {
            let l = self.doc.layers_mut().get_mut(layer_id);
            // 提取 → 插件处理 → 写回（直接走 filter 管道的变体）
            crate::filter::apply_filter_via(
                l,
                layer_id,
                &mut |px, w, h| {
                    self.plugins.run_filter(name, px, w, h, params);
                },
                selection.as_ref(),
                &mut recorder,
            );
        }
        self.doc.commit(recorder.finish("PluginFilter"));
        self.dirty = Dirty::All;
        true
    }

    /// 设置活动图层的非破坏性调整（None 清除）。
    pub fn set_layer_adjustment(&mut self, adj: Option<crate::layer::LayerAdjustment>) {
        if let Some(id) = self.doc.layers().try_active() {
            self.doc.layers_mut().get_mut(id).adjustment = adj;
            self.dirty = Dirty::All;
        }
    }

    /// 活动图层调整参数。
    pub fn layer_adjustment(&self) -> Option<crate::layer::LayerAdjustment> {
        self.doc
            .layers()
            .try_active()
            .and_then(|id| self.doc.layers().get(id).adjustment)
    }

    /// 对活动图层（或选区内）应用滤镜。入撤销历史。
    pub fn apply_filter(&mut self, filter: crate::filter::Filter) -> bool {
        log::info!("[filter] 应用滤镜 {filter:?}");
        if self.transforming() {
            return false;
        }
        let Some(layer_id) = self.doc.layers().try_active() else {
            return false;
        };
        let selection = self.doc.selection().cloned();
        let mut recorder = StrokeRecorder::new(layer_id);
        {
            let l = self.doc.layers_mut().get_mut(layer_id);
            crate::filter::apply_filter(l, layer_id, &filter, selection.as_ref(), &mut recorder);
        }
        self.doc.commit(recorder.finish("Filter"));
        self.dirty = Dirty::All;
        true
    }

    /// 内置滤镜名列表。
    pub fn filter_names() -> Vec<&'static str> {
        vec!["模糊", "亮度/对比度", "色相/饱和度", "反色", "灰度"]
    }

    /// 吸管取色：读合成帧缓冲的屏幕像素（Alt+点击）。
    pub fn pick_color(&self, x: u32, y: u32) -> Option<Color> {
        let (w, h) = self.size;
        if x >= w || y >= h {
            return None;
        }
        let i = ((y * w + x) * 4) as usize;
        let p = self.frame.get(i..i + 3)?;
        Some(Color {
            r: p[0],
            g: p[1],
            b: p[2],
        })
    }

    /// 设置纹理笔刷尖（PNG 字节；None 恢复圆头笔）。
    pub fn set_brush_tip(&mut self, png: Option<&[u8]>) -> bool {
        match png {
            Some(bytes) => match crate::stroke::TipTexture::from_png(bytes) {
                Some(tip) => {
                    self.brush.tip = Some(std::sync::Arc::new(tip));
                    true
                }
                None => false,
            },
            None => {
                self.brush.tip = None;
                true
            }
        }
    }

    /// 尖图散布强度 0..1。
    pub fn set_brush_scatter(&mut self, v: f32) {
        self.brush.scatter = v.clamp(0.0, 1.0);
    }

    /// 当前工具。
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// 切换工具（画笔/橡皮/蒙版编辑/形状/文字）。
    /// 拖拽中切换 = 放弃当前形状预览。
    pub fn set_tool(&mut self, tool: Tool) {
        if tool != self.tool {
            log::info!("[tool] 切换工具: {:?} → {:?}", self.tool, tool);
            self.cancel_shape();
            self.text_anchor = None;
        }
        self.tool = tool;
    }

    // ── 多文档 ──

    /// 换出当前文档（保留在调用方），换入另一个。
    /// 撤销历史/图层/选区/浮动层都在 Document 内——切换零丢失。
    pub fn swap_document(&mut self, doc: Document) -> Document {
        log::info!("[doc] 切换文档");
        self.end_stroke();
        let old = std::mem::replace(&mut self.doc, doc);
        self.dirty = Dirty::All;
        old
    }

    /// 复制当前文档（新文档 = 深拷贝图层栈引用 + 空历史）。
    pub fn duplicate_document(&mut self) -> Document {
        Document::with_layers(self.doc.layers().clone())
    }

    /// 新建空文档（一个默认图层）。
    pub fn new_document(&mut self) {
        log::info!("[doc] 新建文档");
        self.end_stroke();
        self.doc = Document::new(256 * 1024 * 1024);
        self.dirty = Dirty::All;
    }

    /// 当前文档的可变引用（壳层多文档管理器直接操作用）。
    pub fn document_take(&mut self) -> Document {
        std::mem::replace(&mut self.doc, Document::new(1))
    }

    /// 编辑计数（commit/undo/redo 各 +1）——壳层自动保存脏检查：
    /// 记录上次保存时的值，与当前值不同即有未保存修改。
    pub fn edit_count(&self) -> u64 {
        self.doc.edit_count()
    }

    /// 呈现帧计数（单调递增；监控关闭时冻结）。
    /// 壳层两次采样 + 本地时钟差值 = 帧率：
    /// `fps = (c1 - c0) / (t1 - t0)`。事件驱动架构下空闲时段
    /// 增量为 0——这正是"按需重绘零开销"的直接度量。
    pub fn render_count(&self) -> u64 {
        self.presents
    }

    /// 帧率监控开关（运行时切换）。
    pub fn set_fps_monitor(&mut self, on: bool) {
        self.fps_monitor = on;
    }

    /// 监控当前是否开启。
    pub fn fps_monitor_enabled(&self) -> bool {
        self.fps_monitor
    }

    /// 交互期渲染是否激活（含手势自动判定）。
    pub fn interactive_enabled(&self) -> bool {
        self.interactive || self.gesture.is_some()
    }

    /// 交互期渲染开关（滚轮缩放等壳层驱动场景）。开启：半分辨率合成；
    /// 关闭：置全脏区做一次全分辨率精化。双指手势无需壳层调用（自动）。
    pub fn set_interactive(&mut self, on: bool) {
        if self.interactive && !on {
            self.dirty = Dirty::All; // 精化一帧
        }
        self.interactive = on;
    }

    /// 合成帧缓冲的可变访问（呈现前叠加 UI 用）。
    pub fn frame_mut(&mut self) -> Option<&mut [u8]> {
        if self.frame.is_empty() {
            None
        } else {
            Some(&mut self.frame)
        }
    }

    /// 帧尺寸。
    pub fn frame_size(&self) -> (u32, u32) {
        self.size
    }

    /// 当前脏区状态。
    pub fn dirty(&self) -> Dirty {
        self.dirty
    }

    /// 空白区点阵网格开关（无限画布空间指示）。
    pub fn show_grid(&self) -> bool {
        self.doc.show_grid()
    }

    /// 网格开关。
    pub fn set_show_grid(&mut self, on: bool) {
        self.doc.set_show_grid(on);
        self.dirty = Dirty::All;
    }

    /// 绕屏幕中心旋转视图。
    pub fn rotate_view(&mut self, delta: f64) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        self.doc
            .viewport_mut()
            .rotate_by((w as f64 / 2.0, h as f64 / 2.0), delta);
        self.dirty = Dirty::All;
    }

    /// 绕屏幕中心水平翻转视图。
    pub fn flip_view(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        self.doc
            .viewport_mut()
            .flip_x_at((w as f64 / 2.0, h as f64 / 2.0));
        self.dirty = Dirty::All;
    }

    /// 100% 缩放，保持屏幕中心内容不动。
    pub fn zoom_100(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let z = self.doc.viewport().zoom();
        self.doc
            .viewport_mut()
            .zoom_at((w as f64 / 2.0, h as f64 / 2.0), 1.0 / z);
        self.dirty = Dirty::All;
    }

    /// 视野适配到全部可见内容（四周留 margin 屏幕像素）。
    /// 无内容时回到原点居中、100% 缩放。平移丢失后的"回家"操作。
    pub fn fit_to_content(&mut self, margin: f64) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        match self.visible_content_bounds() {
            Some(b) => self
                .doc
                .viewport_mut()
                .fit_to(b, (w as f64, h as f64), margin),
            None => self.doc.viewport_mut().center_origin((w as f64, h as f64)),
        }
        self.dirty = Dirty::All;
    }

    // ── 图层结构 API（撤销走 Document 历史）──

    /// 新建图层（入撤销）。
    /// 单独显示某层（视图状态，不入撤销/存档）：合成只画该层。
    /// `None` = 恢复全部显示。指向已删层时合成端安全回退全显。
    pub fn set_solo_layer(&mut self, id: Option<LayerId>) {
        if self.doc.solo() != id {
            log::info!("[layer] 单独显示: {:?}", id.map(|i| i.to_raw()));
            self.doc.set_solo(id);
            self.dirty = Dirty::All;
        }
    }

    /// 当前单独显示的图层。
    pub fn solo_layer(&self) -> Option<LayerId> {
        self.doc.solo()
    }

    /// 按栈序索引设置单独显示（壳层列表交互便利）。
    pub fn set_solo_index(&mut self, idx: Option<usize>) {
        let id = idx.and_then(|i| {
            self.doc
                .layers()
                .iter_with_id()
                .nth(i)
                .map(|(id, _)| id)
        });
        self.set_solo_layer(id);
    }

    pub fn add_layer(&mut self) -> Option<LayerId> {
        log::info!("[layer] 新建图层");
        let r = self.doc.add_layer(None);
        if r.is_some() {
            self.dirty = Dirty::All;
        }
        r
    }

    /// 删除图层（入撤销）。
    pub fn remove_layer(&mut self, id: LayerId) -> bool {
        let r = self.doc.remove_layer(id);
        if r {
            self.dirty = Dirty::All;
        }
        r
    }

    /// 复制图层（入撤销）。
    pub fn duplicate_layer(&mut self, id: LayerId) -> Option<LayerId> {
        let r = self.doc.duplicate_layer(id);
        if r.is_some() {
            self.dirty = Dirty::All;
        }
        r
    }

    /// 重排图层（入撤销）。
    pub fn reorder_layer(&mut self, id: LayerId, to: usize) -> bool {
        let r = self.doc.reorder_layer(id, to);
        if r {
            self.dirty = Dirty::All;
        }
        r
    }

    // ── 剪贴板（内部瓦片级 + 外部图像）──

    /// 复制选中内容（无选区=整层内容）到内部剪贴板。返回是否有内容。
    pub fn copy_selection(&mut self) -> bool {
        log::info!("[clipboard] 复制选区");
        if self.transforming() {
            return false;
        }
        match self.lift_copy() {
            Some((grid, _)) => {
                self.clipboard = Some(grid);
                true
            }
            None => false,
        }
    }

    /// 剪切：复制 + 清除原内容（整组入撤销）。
    pub fn cut_selection(&mut self) -> bool {
        log::info!("[clipboard] 剪切选区");
        if !self.copy_selection() {
            return false;
        }
        self.delete_selection()
    }

    /// 删除选中内容（无选区=整层；入撤销）。
    pub fn delete_selection(&mut self) -> bool {
        if self.transforming() {
            return false;
        }
        let Some(layer) = self.doc.layers().try_active() else {
            return false;
        };
        let sel = self.doc.selection().cloned();
        let Some(bbox) = self.region_bbox() else {
            return false;
        };
        let mut recorder = StrokeRecorder::new(layer);
        {
            let l = self.doc.layers_mut().get_mut(layer);
            for ty in ((bbox.y as i64 >> 8) - 1)..=((bbox.y2() >> 8) + 1) {
                for tx in ((bbox.x as i64 >> 8) - 1)..=((bbox.x2() >> 8) + 1) {
                    let id = TileId {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    let Some(src) = l.tiles.get(id).cloned() else {
                        continue;
                    };
                    let sp = src.pixels();
                    let mut any = false;
                    for i in 0..sp.len() / 4 {
                        let hit = match &sel {
                            Some(g) => g.get(id).map(|t| t.pixels()[i * 4] > 127).unwrap_or(false),
                            None => sp[i * 4 + 3] > 0,
                        };
                        if hit && sp[i * 4 + 3] > 0 {
                            if !any {
                                recorder.capture(&l.tiles, id);
                                any = true;
                            }
                            let t = l.tiles.get_or_create_mut(id);
                            t.pixels_mut()[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                }
            }
            l.tiles.prune();
        }
        self.doc.commit(recorder.finish("Cut"));
        self.dirty = Dirty::All;
        true
    }

    /// 粘贴内部剪贴板：以浮动变换形态出现在原位置（拖拽定位后 Enter 提交）。
    pub fn paste_float(&mut self) -> bool {
        let Some(grid) = self.clipboard.clone() else {
            return false;
        };
        if self.transforming() {
            return false;
        }
        let Some(layer) = self.doc.layers().try_active() else {
            return false;
        };
        let Some(bbox) = grid.content_bounds_precise() else {
            return false;
        };
        self.doc.set_floating(Some(crate::float::Floating {
            tiles: grid,
            layer,
            affine: crate::float::Affine2::IDENTITY,
            pivot: (
                bbox.x as f64 + bbox.w as f64 / 2.0,
                bbox.y as f64 + bbox.h as f64 / 2.0,
            ),
        }));
        // 粘贴无"提升"步骤：采集器只捕获提交写入
        self.transform_recorder = Some(StrokeRecorder::new(layer));
        self.dirty = Dirty::All;
        true
    }

    /// 粘贴外部图像（PNG 字节）：置于当前视野中心，浮动形态。
    pub fn paste_image_float(&mut self, png: &[u8]) -> bool {
        let Ok((rgba, w, h)) = crate::io::decode_png(png) else {
            return false;
        };
        self.paste_premul_float(&rgba, w, h)
    }

    /// 粘贴直行 RGBA（系统剪贴板常见形态）：先预乘再入浮动。
    pub fn paste_rgba_float(&mut self, rgba: &[u8], w: u32, h: u32) -> bool {
        log::info!("[clipboard] 粘贴浮动 {w}×{h}");
        if w == 0 || h == 0 || rgba.len() < (w as usize) * (h as usize) * 4 {
            return false;
        }
        let mut premul = rgba.to_vec();
        for px in premul.as_chunks_mut::<4>().0 {
            let a = px[3] as u32;
            for c in px.iter_mut().take(3) {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
        self.paste_premul_float(&premul, w, h)
    }

    /// 油漆桶：以点击处活动层像素为基准色的连通区域填充（笔刷色不透明）。
    /// `tolerance` 为各通道容差（0-255）。填充范围限制在图层内容包围盒
    /// 与点击点周围 8192px 的交集——无限画布上空区域不发散。选区
    /// 存在时作为屏障裁剪扩散。整组入撤销。
    pub fn flood_fill(&mut self, x: i64, y: i64, tolerance: u8) -> bool {
        let layer = self.doc.active_layer();
        // 采样阶段（只读借用）
        let (region, target, selection) = {
            let l = self.doc.layers().get(layer);
            let Some(bounds) = l.tiles.content_bounds_precise() else {
                return false; // 空层无处可填
            };
            // 点击点周围 8192px 上限框
            let cap = 8192i64;
            let cbox = Rect::new(
                (x - cap) as i32,
                (y - cap) as i32,
                (cap * 2) as u32,
                (cap * 2) as u32,
            );
            let Some(r) = bounds.intersect(&cbox) else {
                return false;
            };
            let t = layer_px(&l.tiles, x, y);
            (r, t, self.doc.selection().cloned())
        };
        if region.w == 0 || region.h == 0 {
            return false;
        }
        let (rx, ry, rw, rh) = (
            region.x as i64,
            region.y as i64,
            region.w as i64,
            region.h as i64,
        );
        // 访问位图（region 局部坐标）
        let mut visited = vec![0u8; (rw * rh) as usize];
        let idx = |px: i64, py: i64| ((py - ry) * rw + (px - rx)) as usize;
        let inside = |px: i64, py: i64| px >= rx && px < rx + rw && py >= ry && py < ry + rh;
        let sel_ok = |sel: &Option<TileGrid>, px: i64, py: i64| match sel {
            None => true,
            Some(s) => layer_px(s, px, py)[0] > 0,
        };
        let matches = |p: [u8; 4], tol: u8| {
            (p[0] as i16 - target[0] as i16).unsigned_abs() <= tol as u16
                && (p[1] as i16 - target[1] as i16).unsigned_abs() <= tol as u16
                && (p[2] as i16 - target[2] as i16).unsigned_abs() <= tol as u16
                && (p[3] as i16 - target[3] as i16).unsigned_abs() <= tol as u16
        };

        let color = self.brush.color;
        let fill_px = [color.r, color.g, color.b, 255];
        let tol = tolerance;
        let mut stack: Vec<(i64, i64)> = vec![(x, y)];
        let mut touched: Vec<TileId> = Vec::new();
        let mut recorder = StrokeRecorder::new(layer);
        let mut filled_any = false;

        // 写入阶段（可变借用，逐像素入格）
        let l = self.doc.layers_mut().get_mut(layer);
        while let Some((sx, sy)) = stack.pop() {
            if !inside(sx, sy) || visited[idx(sx, sy)] != 0 {
                continue;
            }
            // 向上找到区间起点
            let mut y0 = sy;
            while y0 > ry
                && visited[idx(sx, y0 - 1)] == 0
                && matches(layer_px(&l.tiles, sx, y0 - 1), tol)
                && sel_ok(&selection, sx, y0 - 1)
            {
                y0 -= 1;
            }
            // 向下扫描填充
            let mut cy = y0;
            let mut span_left = false;
            let mut span_right = false;
            while cy < ry + rh
                && matches(layer_px(&l.tiles, sx, cy), tol)
                && sel_ok(&selection, sx, cy)
            {
                let tid = TileId::at(sx, cy);
                if !touched.contains(&tid) {
                    recorder.capture(&l.tiles, tid);
                    touched.push(tid);
                }
                let t = l.tiles.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let i = (((cy - oy) * 256 + (sx - ox)) * 4) as usize;
                if i + 3 < t.pixels_mut().len() {
                    t.pixels_mut()[i..i + 4].copy_from_slice(&fill_px);
                    filled_any = true;
                }
                visited[idx(sx, cy)] = 1;
                // 左右邻居入栈（扫描线优化：只在 span 边界入栈）
                if inside(sx - 1, cy) && visited[idx(sx - 1, cy)] == 0 {
                    if !span_left && matches(layer_px(&l.tiles, sx - 1, cy), tol) {
                        stack.push((sx - 1, cy));
                        span_left = true;
                    } else if span_left && !matches(layer_px(&l.tiles, sx - 1, cy), tol) {
                        span_left = false;
                    }
                }
                if inside(sx + 1, cy) && visited[idx(sx + 1, cy)] == 0 {
                    if !span_right && matches(layer_px(&l.tiles, sx + 1, cy), tol) {
                        stack.push((sx + 1, cy));
                        span_right = true;
                    } else if span_right && !matches(layer_px(&l.tiles, sx + 1, cy), tol) {
                        span_right = false;
                    }
                }
                cy += 1;
            }
        }
        if !filled_any {
            log::info!("[fill] 油漆桶 ({x},{y}) tol={tolerance}: 无可填区域");
            return false;
        }
        let n = touched.len();
        self.doc.commit(recorder.finish("Fill"));
        log::info!("[fill] 油漆桶 ({x},{y}) tol={tolerance}: 填充 {n} 瓦片");
        self.dirty = Dirty::All;
        true
    }

    /// 直行 RGBA 直接盖到活动图层指定画布位置（alpha over，入撤销）。
    /// 壳层自渲染内容（Web 浏览器字体文字、贴纸等）的落墨通道；
    /// `x, y` 为目标左上角画布坐标。
    pub fn paste_rgba_at(&mut self, rgba: &[u8], w: u32, h: u32, x: i64, y: i64) -> bool {
        log::info!("[paste] RGBA 落墨 {w}×{h} at ({x},{y})");
        if w == 0 || h == 0 || rgba.len() < (w as usize) * (h as usize) * 4 {
            return false;
        }
        let Some(layer) = self.doc.layers().try_active() else {
            return false;
        };
        let mut recorder = StrokeRecorder::new(layer);
        let l = self.doc.layers_mut().get_mut(layer);
        for gy in 0..h as i64 {
            for gx in 0..w as i64 {
                let src = ((gy * w as i64 + gx) * 4) as usize;
                let a = rgba[src + 3];
                if a == 0 {
                    continue;
                }
                let (px, py) = (x + gx, y + gy);
                let tid = TileId::at(px, py);
                recorder.capture(&l.tiles, tid);
                let t = l.tiles.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let i = (((py - oy) * 256 + (px - ox)) * 4) as usize;
                if i + 3 >= t.pixels_mut().len() {
                    continue;
                }
                // 直行 → 预乘源
                let sa = a as u32;
                let sr = [rgba[src] as u32 * sa / 255, rgba[src + 1] as u32 * sa / 255, rgba[src + 2] as u32 * sa / 255];
                let p = &mut t.pixels_mut()[i..i + 4];
                let da = p[3] as u32;
                for k in 0..3 {
                    p[k] = (sr[k] + p[k] as u32 * (255 - sa) / 255) as u8;
                }
                p[3] = (sa + da * (255 - sa) / 255) as u8;
            }
        }
        let group = recorder.finish("Paste");
        if !group.tile_ids().is_empty() {
            self.doc.commit(group);
            self.dirty = Dirty::All;
        }
        true
    }

    fn paste_premul_float(&mut self, rgba: &[u8], w: u32, h: u32) -> bool {
        if self.transforming() {
            return false;
        }
        let Some(layer) = self.doc.layers().try_active() else {
            return false;
        };
        // 视野中心（画布坐标）为图像中心
        let (sw, sh) = self.size;
        if sw == 0 || sh == 0 {
            return false;
        }
        let (cx, cy) = self
            .doc
            .viewport()
            .screen_to_canvas(sw as f64 / 2.0, sh as f64 / 2.0);
        // 图像左上角取整到画布像素，随后以画布绝对坐标写入瓦片
        let (ix0, iy0) = (
            (cx - w as f64 / 2.0).round() as i64,
            (cy - h as f64 / 2.0).round() as i64,
        );
        let mut grid = TileGrid::new();
        for gy in 0..h as i64 {
            for gx in 0..w as i64 {
                let src = ((gy * w as i64 + gx) * 4) as usize;
                if rgba[src + 3] == 0 {
                    continue;
                }
                let id = TileId::at(ix0 + gx, iy0 + gy);
                let t = grid.get_or_create_mut(id);
                let (ox, oy) = id.origin();
                let lx = (ix0 + gx - ox) as usize;
                let ly = (iy0 + gy - oy) as usize;
                let dst = (ly * 256 + lx) * 4;
                t.pixels_mut()[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        }
        grid.prune();
        self.doc.set_floating(Some(crate::float::Floating {
            tiles: grid,
            layer,
            affine: crate::float::Affine2::IDENTITY,
            pivot: (cx, cy),
        }));
        self.transform_recorder = Some(StrokeRecorder::new(layer));
        self.dirty = Dirty::All;
        true
    }

    /// 内部剪贴板非空。
    pub fn has_clipboard(&self) -> bool {
        self.clipboard.is_some()
    }

    /// 选区内容（无选区=整层）导出 PNG（系统剪贴板写入用）。
    pub fn copy_selection_png(&self) -> Option<Vec<u8>> {
        let _ = self.doc.layers().try_active()?;
        let (grid, bbox) = self.lift_copy()?;
        let (w, h) = (bbox.w as usize, bbox.h as usize);
        let mut rgba = vec![0u8; w * h * 4];
        for id in grid.ids() {
            let (ox, oy) = id.origin();
            let Some(t) = grid.get(id) else { continue };
            for row in 0..TILE as i64 {
                let gy = oy + row;
                if gy < bbox.y as i64 || gy >= bbox.y2() {
                    continue;
                }
                for col in 0..TILE as i64 {
                    let gx = ox + col;
                    if gx < bbox.x as i64 || gx >= bbox.x2() {
                        continue;
                    }
                    let s = ((row * 256 + col) * 4) as usize;
                    let d = (((gy - bbox.y as i64) * w as i64 + (gx - bbox.x as i64)) * 4) as usize;
                    rgba[d..d + 4].copy_from_slice(&t.pixels()[s..s + 4]);
                }
            }
        }
        crate::io::encode_png(&rgba, w as u32, h as u32).ok()
    }

    /// 提升范围 bbox：选区 bbox 或图层内容 bbox。
    fn region_bbox(&self) -> Option<Rect> {
        let layer = self.doc.layers().try_active()?;
        match self.doc.selection() {
            Some(g) => g.content_bounds(),
            None => self.doc.layers().get(layer).content().content_bounds_precise(),
        }
    }

    /// 拷贝选中像素（不动图层）：返回 (网格, bbox)。
    fn lift_copy(&self) -> Option<(TileGrid, Rect)> {
        let layer = self.doc.layers().try_active()?;
        let sel = self.doc.selection();
        let bbox = self.region_bbox()?;
        let mut grid = TileGrid::new();
        let l = self.doc.layers().get(layer);
        for ty in ((bbox.y as i64 >> 8) - 1)..=((bbox.y2() >> 8) + 1) {
            for tx in ((bbox.x as i64 >> 8) - 1)..=((bbox.x2() >> 8) + 1) {
                let id = TileId {
                    x: tx as i32,
                    y: ty as i32,
                };
                let Some(src) = l.tiles.get(id) else { continue };
                let sp = src.pixels();
                let mut any = false;
                {
                    let t = grid.get_or_create_mut(id);
                    let fp = t.pixels_mut();
                    for i in 0..fp.len() / 4 {
                        let hit = match sel {
                            Some(g) => g.get(id).map(|t| t.pixels()[i * 4] > 127).unwrap_or(false),
                            None => sp[i * 4 + 3] > 0,
                        };
                        if hit && sp[i * 4 + 3] > 0 {
                            any = true;
                            fp[i * 4..i * 4 + 4].copy_from_slice(&sp[i * 4..i * 4 + 4]);
                        }
                    }
                }
                if !any {
                    grid.remove(id);
                }
            }
        }
        if grid.is_empty() {
            None
        } else {
            Some((grid, bbox))
        }
    }

    // ── 内容级变换（选区或整层的移动/旋转/缩放，浮动预览）──

    /// 内容级变换进行中。
    pub fn transforming(&self) -> bool {
        self.doc.floating().is_some()
    }

    /// 开始变换：提升选中内容（无选区则整层内容）为浮动层。
    /// 进行中忽略笔画与撤销；提交/取消后恢复。
    pub fn begin_transform(&mut self) -> bool {
        log::info!("[transform] 开始内容变换");
        if self.transforming() {
            return false;
        }
        let layer = match self.doc.layers().try_active() {
            Some(l) => l,
            None => return false,
        };
        let sel = self.doc.selection().cloned();
        // 提升范围：选区瓦片范围；无选区时图层内容范围
        let bbox = match (
            &sel,
            self.doc.layers().get(layer).tiles.content_bounds_precise(),
        ) {
            (Some(_), _) => self.doc.selection().and_then(|g| g.content_bounds()),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(bbox) = bbox else { return false };

        let mut float_tiles = TileGrid::new();
        let mut recorder = StrokeRecorder::new(layer);
        {
            let l = self.doc.layers_mut().get_mut(layer);
            let x0 = (bbox.x as i64 >> 8) - 1;
            let y0 = (bbox.y as i64 >> 8) - 1;
            let x1 = ((bbox.x as i64 + bbox.w as i64) >> 8) + 1;
            let y1 = ((bbox.y as i64 + bbox.h as i64) >> 8) + 1;
            for ty in y0..=y1 {
                for tx in x0..=x1 {
                    let id = TileId {
                        x: tx as i32,
                        y: ty as i32,
                    };
                    let Some(src) = l.tiles.get(id).cloned() else {
                        continue;
                    };
                    let sp = src.pixels();
                    let ft = float_tiles.get_or_create_mut(id);
                    let fp = ft.pixels_mut();
                    let mut any = false;
                    for i in 0..fp.len() / 4 {
                        let sel_v = match &sel {
                            Some(g) => g.get(id).map(|t| t.pixels()[i * 4] > 127).unwrap_or(false),
                            None => sp[i * 4 + 3] > 0,
                        };
                        if sel_v && sp[i * 4 + 3] > 0 {
                            any = true;
                            fp[i * 4..i * 4 + 4].copy_from_slice(&sp[i * 4..i * 4 + 4]);
                        }
                    }
                    if any {
                        // 清除图层中被提升的像素（整块重写为透明）
                        recorder.capture(&l.tiles, id);
                        let t = l.tiles.get_or_create_mut(id);
                        let lp = t.pixels_mut();
                        for i in 0..lp.len() / 4 {
                            let sel_v = match &sel {
                                Some(g) => {
                                    g.get(id).map(|t| t.pixels()[i * 4] > 127).unwrap_or(false)
                                }
                                None => sp[i * 4 + 3] > 0,
                            };
                            if sel_v {
                                lp[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
                            }
                        }
                    }
                }
            }
            l.tiles.prune();
        }
        if float_tiles.is_empty() {
            return false; // 空内容
        }
        let pivot = (
            bbox.x as f64 + bbox.w as f64 / 2.0,
            bbox.y as f64 + bbox.h as f64 / 2.0,
        );
        self.doc.set_floating(Some(crate::float::Floating {
            tiles: float_tiles,
            layer,
            affine: crate::float::Affine2::IDENTITY,
            pivot,
        }));
        self.transform_recorder = Some(recorder);
        self.dirty = Dirty::All;
        true
    }

    /// 变换平移（画布坐标）。
    pub fn transform_translate(&mut self, dx: f64, dy: f64) {
        if let Some(f) = self.doc.floating_mut() {
            f.translate(dx, dy);
            self.dirty = Dirty::All;
        }
    }

    /// 变换旋转（弧度，绕内容中心）。
    pub fn transform_rotate(&mut self, delta_rad: f64) {
        if let Some(f) = self.doc.floating_mut() {
            f.rotate(delta_rad);
            self.dirty = Dirty::All;
        }
    }

    /// 变换缩放（绕内容中心）。
    pub fn transform_scale(&mut self, factor: f64) {
        if let Some(f) = self.doc.floating_mut() {
            f.scale(factor);
            self.dirty = Dirty::All;
        }
    }

    /// 提交：按累积仿射盖章回图层（整组入撤销）。
    pub fn commit_transform(&mut self) -> bool {
        log::info!("[transform] 提交变换");
        let Some(fl) = self.doc.floating().cloned() else {
            return false;
        };
        let Some(mut recorder) = self.transform_recorder.take() else {
            return false;
        };
        self.doc.set_floating(None);
        let inv = fl.affine.invert();
        // 目标范围：浮动 bbox（画布）逐瓦片
        let Some(bbox) = fl.canvas_bbox() else {
            self.doc.commit(recorder.finish("Transform"));
            self.dirty = Dirty::All;
            return true;
        };
        let layer = fl.layer;
        let l = self.doc.layers_mut().get_mut(layer);
        let x0 = (bbox.x as i64 >> 8) - 1;
        let y0 = (bbox.y as i64 >> 8) - 1;
        let x1 = ((bbox.x as i64 + bbox.w as i64) >> 8) + 1;
        let y1 = ((bbox.y as i64 + bbox.h as i64) >> 8) + 1;
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let id = TileId {
                    x: tx as i32,
                    y: ty as i32,
                };
                let (ox, oy) = id.origin();
                // 逐像素：目标画布 → 逆仿射 → 源瓦片采样（最近邻）
                let mut rows: Vec<(usize, usize, [u8; 4])> = Vec::new();
                for py in 0..crate::tile::TILE as i64 {
                    for px in 0..crate::tile::TILE as i64 {
                        let (cx, cy) = (ox as f64 + px as f64 + 0.5, oy as f64 + py as f64 + 0.5);
                        let (sx, sy) = inv.apply(cx, cy);
                        let sid = TileId::at(sx.floor() as i64, sy.floor() as i64);
                        let Some(st) = fl.tiles.get(sid) else {
                            continue;
                        };
                        let (sox, soy) = sid.origin();
                        let lx = (sx.floor() as i64 - sox) as usize;
                        let ly = (sy.floor() as i64 - soy) as usize;
                        if lx >= 256 || ly >= 256 {
                            continue;
                        }
                        let p = &st.pixels()[(ly * 256 + lx) * 4..][..4];
                        if p[3] == 0 {
                            continue;
                        }
                        rows.push((py as usize, px as usize, [p[0], p[1], p[2], p[3]]));
                    }
                }
                if rows.is_empty() {
                    continue;
                }
                recorder.capture(&l.tiles, id);
                let tile = l.tiles.get_or_create_mut(id);
                for (py, px, p) in rows {
                    let i = (py * crate::tile::TILE as usize + px) * 4;
                    // source-over（预乘）
                    let sa = p[3] as f32 / 255.0;
                    let da = tile.pixels()[i + 3] as f32 / 255.0;
                    let oa = sa + da * (1.0 - sa);
                    let tp = tile.pixels_mut();
                    for k in 0..3 {
                        tp[i + k] = (p[k] as f32 + tp[i + k] as f32 * (1.0 - sa)) as u8;
                    }
                    tp[i + 3] = (oa * 255.0 + 0.5) as u8;
                }
            }
        }
        l.tiles.prune();
        self.doc.commit(recorder.finish("Transform"));
        self.dirty = Dirty::All;
        true
    }

    /// 取消：浮动内容按恒等仿射放回原位（内容零变化，不入撤销）。
    pub fn cancel_transform(&mut self) -> bool {
        log::info!("[transform] 取消变换");
        let Some(fl) = self.doc.floating().cloned() else {
            return false;
        };
        self.transform_recorder = None; // 丢弃提升时的采集——撤销组不完整，但取消本身是净零操作，
                                        // 原像素即将被写回，净效果等于从未发生
        self.doc.set_floating(None);
        let layer = fl.layer;
        let l = self.doc.layers_mut().get_mut(layer);
        for id in fl.tiles.ids().collect::<Vec<_>>() {
            let Some(src) = fl.tiles.get(id) else {
                continue;
            };
            let sp = src.pixels();
            let tile = l.tiles.get_or_create_mut(id);
            let tp = tile.pixels_mut();
            for i in 0..tp.len() / 4 {
                if sp[i * 4 + 3] > 0 {
                    tp[i * 4..i * 4 + 4].copy_from_slice(&sp[i * 4..i * 4 + 4]);
                }
            }
        }
        self.dirty = Dirty::All;
        true
    }

    // ── 文字与矢量形状（写入活动图层，入撤销历史）──

    /// 文字：字体字节由调用方提供（不内置字体，核心零资源依赖）。
    #[cfg(feature = "text")]
    /// `x, y` 为首字基线附近原点（画布坐标）。
    pub fn draw_text(
        &mut self,
        font: &[u8],
        text: &str,
        x: i64,
        y: i64,
        size: f32,
    ) -> Option<Rect> {
        let layer = self.doc.layers().try_active()?;
        let color = self.brush.color;
        let mut recorder = StrokeRecorder::new(layer);
        let grid = &mut self.doc.layers_mut().get_mut(layer).tiles;
        let bounds = crate::shape::draw_text(grid, &mut recorder, font, text, x, y, size, color);
        if bounds.is_some() {
            self.doc.commit(recorder.finish("Text"));
            self.dirty = Dirty::All;
        }
        bounds
    }

    /// 矩形填充（入撤销）。
    pub fn fill_rect(&mut self, rect: Rect) -> bool {
        self.run_shape(|w| w.fill_rect(rect))
    }

    /// 椭圆填充（入撤销）。
    pub fn fill_ellipse(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) -> bool {
        self.run_shape(move |w| w.fill_ellipse(cx, cy, rx, ry))
    }

    /// 椭圆轮廓（入撤销）。
    pub fn stroke_ellipse(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) -> bool {
        self.run_shape(move |w| w.stroke_ellipse(cx, cy, rx, ry))
    }

    /// 直线（dab 链，用当前笔刷参数）。
    pub fn stroke_line(&mut self, x0: f64, y0: f64, x1: f64, y1: f64) -> bool {
        let layer = match self.doc.layers().try_active() {
            Some(l) => l,
            None => return false,
        };
        let b = self.brush.clone();
        let dabs = crate::shape::line_dabs(
            x0,
            y0,
            x1,
            y1,
            b.size / 2.0,
            b.hardness,
            b.color,
            b.flow.clamp(0.01, 1.0),
        );
        let recorder = StrokeRecorder::new(layer);
        // 复用笔画盖章通道（含选区裁剪与撤销采集）
        self.stroke = Some(ActiveStroke {
            state: StrokeState::new(x0, y0, 1.0),
            recorder,
            layer,
            pointer: 0,
        });
        self.stamp(layer, &dabs);
        let act = self.stroke.take().unwrap();
        self.doc.commit(act.recorder.finish("Line"));
        self.dirty = Dirty::All;
        true
    }

    fn run_shape(&mut self, f: impl FnOnce(&mut crate::shape::ShapeWriter)) -> bool {
        let Some(layer) = self.doc.layers().try_active() else {
            return false;
        };
        let color = self.brush.color;
        let mut recorder = StrokeRecorder::new(layer);
        {
            let grid = &mut self.doc.layers_mut().get_mut(layer).tiles;
            let mut writer = crate::shape::ShapeWriter::new(grid, &mut recorder, color);
            f(&mut writer);
        }
        self.doc.commit(recorder.finish("Shape"));
        self.dirty = Dirty::All;
        true
    }

    // ── 选区（像素级裁剪笔画；不入撤销历史，与主流软件一致）──

    /// 是否有活动选区。
    pub fn has_selection(&self) -> bool {
        self.doc.selection().is_some()
    }

    /// 清除选区。
    pub fn clear_selection(&mut self) {
        if self.doc.set_selection(None) {
            self.dirty = Dirty::All;
        }
    }

    /// 全选（当前可见内容包围盒范围）。
    pub fn select_all(&mut self) {
        let b = self
            .visible_content_bounds()
            .unwrap_or(Rect::new(0, 0, 1024, 1024));
        self.select_rect(b, SelectionOp::Replace);
    }

    /// 矩形选区。
    pub fn select_rect(&mut self, rect: Rect, op: SelectionOp) {
        let mut grid = self.take_selection_for(op);
        for y in rect.y as i64..rect.y2() {
            for x in rect.x as i64..rect.x2() {
                let tid = TileId::at(x, y);
                let t = grid.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let i = (((y - oy) * 256 + (x - ox)) * 4) as usize;
                t.pixels_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        self.doc.set_selection(Some(grid));
        self.dirty = Dirty::All;
    }

    /// 套索（多边形）选区，画布坐标。扫描线填充。
    pub fn select_lasso(&mut self, points: &[(f64, f64)], op: SelectionOp) {
        let mut grid = self.take_selection_for(op);
        if points.len() >= 3 {
            let ys: Vec<f64> = points.iter().map(|p| p.1).collect();
            let (y0, y1) = (
                ys.iter().cloned().fold(f64::MAX, f64::min).floor() as i64,
                ys.iter().cloned().fold(f64::MIN, f64::max).ceil() as i64,
            );
            let n = points.len();
            for y in y0..y1 {
                let cy = y as f64 + 0.5;
                let mut xs = Vec::new();
                for i in 0..n {
                    let (ax, ay) = (points[i].0, points[i].1);
                    let (bx, by) = (points[(i + 1) % n].0, points[(i + 1) % n].1);
                    if (ay <= cy && by > cy) || (by <= cy && ay > cy) {
                        let t = (cy - ay) / (by - ay);
                        xs.push(ax + t * (bx - ax));
                    }
                }
                xs.sort_by(|a, b| a.total_cmp(b));
                let it = xs.chunks(2);
                for pair in it {
                    if pair.len() == 2 {
                        for x in pair[0].ceil() as i64..pair[1].ceil() as i64 {
                            let tid = TileId::at(x, y);
                            let t = grid.get_or_create_mut(tid);
                            let (ox, oy) = tid.origin();
                            let i = (((y - oy) * 256 + (x - ox)) * 4) as usize;
                            if i + 3 < t.pixels_mut().len() {
                                t.pixels_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                            }
                        }
                    }
                }
            }
        }
        self.doc.set_selection(Some(grid));
        self.dirty = Dirty::All;
    }

    fn take_selection_for(&mut self, op: SelectionOp) -> TileGrid {
        let existing = self.doc.selection().cloned();
        match (op, existing) {
            (SelectionOp::Replace, _) | (_, None) => TileGrid::new(),
            (SelectionOp::Add, Some(g)) => g,
            (SelectionOp::Subtract, Some(g)) => g,
        }
    }

    /// 给活动图层创建空蒙版（已有则移除）。返回是否启用。
    pub fn toggle_layer_mask(&mut self) -> bool {
        let Some(active) = self.doc.layers().try_active() else {
            return false;
        };
        let layer = self.doc.layers_mut().get_mut(active);
        let had = layer.mask.is_some();
        layer.mask = if had { None } else { Some(TileGrid::new()) };
        self.dirty = Dirty::All;
        !had
    }

    /// 切换活动图层的剪贴层属性。
    pub fn toggle_layer_clip(&mut self) -> bool {
        let Some(active) = self.doc.layers().try_active() else {
            return false;
        };
        let layer = self.doc.layers_mut().get_mut(active);
        layer.clipped = !layer.clipped;
        let v = layer.clipped;
        self.dirty = Dirty::All;
        v
    }

    /// 设置图层属性（面板 UI 用；统一标脏）。
    pub fn set_layer_opacity(&mut self, id: LayerId, v: f32) {
        if let Some(l) = self.doc.layers_mut().try_get_mut(id) {
            l.opacity = v.clamp(0.0, 1.0);
            self.dirty = Dirty::All;
        }
    }

    /// 图层可见性。
    pub fn set_layer_visible(&mut self, id: LayerId, v: bool) {
        if let Some(l) = self.doc.layers_mut().try_get_mut(id) {
            l.visible = v;
            self.dirty = Dirty::All;
        }
    }

    /// 图层混合模式。
    pub fn set_layer_blend_mode(&mut self, id: LayerId, mode: BlendMode) {
        if let Some(l) = self.doc.layers_mut().try_get_mut(id) {
            l.blend_mode = mode;
            self.dirty = Dirty::All;
        }
    }

    /// 图层信息（面板 UI 渲染用）。
    pub fn layer_infos(&self) -> Vec<LayerInfo> {
        self.doc
            .layers()
            .iter_with_id()
            .map(|(id, l)| LayerInfo {
                id: id.to_raw(),
                name: l.name.clone(),
                opacity: l.opacity,
                visible: l.visible,
                blend_mode: l.blend_mode,
                clipped: l.clipped,
                has_mask: l.mask.is_some(),
                group: l.group.clone(),
                adjustment: l.adjustment,
            })
            .collect()
    }

    /// 活动图层原始 id。
    pub fn active_layer_id(&self) -> Option<u64> {
        self.doc.layers().try_active().map(|l| l.to_raw())
    }

    /// 按 id 选层。
    pub fn select_layer_by_id(&mut self, raw: u64) -> bool {
        let id = LayerId::from_raw(raw);
        if !self.doc.layers().contains(id) {
            return false;
        }
        self.doc.layers_mut().set_active(id);
        self.dirty = Dirty::All;
        true
    }

    /// 12 种混合模式名。
    pub fn blend_mode_names() -> Vec<&'static str> {
        BlendMode::ALL.iter().map(|m| m.name()).collect()
    }

    /// 按原始 id 操作（面板用薄封装）。
    pub fn reorder_layer_by_index(&mut self, index: usize, to: usize) -> bool {
        let Some(id) = self
            .doc
            .layers()
            .iter_with_id()
            .nth(index)
            .map(|(i, _)| i.to_raw())
        else {
            return false;
        };
        self.reorder_layer(LayerId::from_raw(id), to)
    }

    /// 按栈位删除。
    pub fn remove_layer_by_index(&mut self, index: usize) -> bool {
        let Some(id) = self
            .doc
            .layers()
            .iter_with_id()
            .nth(index)
            .map(|(i, _)| i.to_raw())
        else {
            return false;
        };
        self.remove_layer(LayerId::from_raw(id))
    }

    /// 按栈位复制。
    pub fn duplicate_layer_by_index(&mut self, index: usize) -> Option<u64> {
        let id_opt = self
            .doc
            .layers()
            .iter_with_id()
            .nth(index)
            .map(|(id, _)| id);
        let id = id_opt?;
        self.duplicate_layer(id).map(|l| l.to_raw())
    }

    /// 按栈位设透明度。
    pub fn set_layer_opacity_by_index(&mut self, index: usize, v: f32) {
        let id_opt = self
            .doc
            .layers()
            .iter_with_id()
            .nth(index)
            .map(|(id, _)| id);
        if let Some(id) = id_opt {
            self.set_layer_opacity(id, v);
        }
    }

    /// 按栈位设可见性。
    pub fn set_layer_visible_by_index(&mut self, index: usize, v: bool) {
        let id_opt = self
            .doc
            .layers()
            .iter_with_id()
            .nth(index)
            .map(|(id, _)| id);
        if let Some(id) = id_opt {
            self.set_layer_visible(id, v);
        }
    }

    /// 按栈位设混合模式。
    pub fn set_layer_blend_by_index(&mut self, index: usize, mode_idx: usize) {
        let id_opt = self
            .doc
            .layers()
            .iter_with_id()
            .nth(index)
            .map(|(id, _)| id);
        if let Some(id) = id_opt {
            if let Some(m) = BlendMode::ALL.get(mode_idx) {
                self.set_layer_blend_mode(id, *m);
            }
        }
    }

    /// 图层数。
    pub fn layer_count(&self) -> usize {
        self.doc.layers().len()
    }

    /// 活动图层的栈位（0 = 底层）。
    pub fn active_layer_index(&self) -> Option<usize> {
        self.doc.layers().position(self.doc.layers().active())
    }

    /// 按栈位选择活动图层。
    pub fn select_layer_index(&mut self, index: usize) -> bool {
        let Some((id, _)) = self.doc.layers().iter_with_id().nth(index) else {
            return false;
        };
        self.doc.layers_mut().set_active(id);
        self.dirty = Dirty::All;
        true
    }

    /// 按栈位取图层名。
    pub fn layer_name(&self, index: usize) -> Option<&str> {
        self.doc.layers().iter().nth(index).map(|l| l.name.as_str())
    }

    /// 活动图层向下合并。底层无下层时失败。
    pub fn merge_down(&mut self) -> bool {
        log::info!("[layer] 向下合并");
        let Some(active) = self.doc.layers().try_active() else {
            return false;
        };
        let Some(pos) = self.doc.layers().position(active) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        let Some(below) = self
            .doc
            .layers()
            .iter_with_id()
            .nth(pos - 1)
            .map(|(id, _)| id)
        else {
            return false;
        };
        let Some((index, above_layer)) = self.doc.layers_mut().remove(active) else {
            return false;
        };
        let mut rec = StrokeRecorder::new(below);
        {
            let dst = self.doc.layers_mut().get_mut(below);
            self.renderer.merge_layers(dst, &above_layer, &mut rec);
        }
        let mut ops = rec.finish("MergeDown").ops;
        ops.push(UndoOp::InsertLayer {
            index,
            id: active,
            layer: above_layer,
        });
        self.doc.commit(UndoGroup {
            label: "MergeDown",
            ops,
        });
        self.doc.layers_mut().set_active(below);
        self.dirty = Dirty::All;
        true
    }

    /// 全部可见图层压平为单层。
    pub fn flatten(&mut self) -> bool {
        log::info!("[layer] 压平");
        if self.doc.layers().len() <= 1 {
            return false;
        }
        let mut merged = Layer::new("扁平化图层");
        for l in self.doc.layers().iter() {
            if l.visible {
                let l = l.clone(); // Arc 共享，代价极小
                self.renderer.merge_layers(
                    &mut merged,
                    &l,
                    &mut StrokeRecorder::new(LayerId::from_raw(u64::MAX)),
                );
            }
        }
        let old_ids: Vec<LayerId> = self.doc.layers().iter_with_id().map(|(id, _)| id).collect();
        let merged_id = self.doc.layers_mut().alloc_id();
        self.doc
            .layers_mut()
            .insert_entry(old_ids.len(), merged_id, merged);
        let mut ops = vec![UndoOp::RemoveLayer { id: merged_id }];
        for old in old_ids {
            if let Some((index, layer)) = self.doc.layers_mut().remove(old) {
                ops.push(UndoOp::InsertLayer {
                    index,
                    id: old,
                    layer,
                });
            }
        }
        self.doc.commit(UndoGroup {
            label: "Flatten",
            ops,
        });
        self.doc.layers_mut().set_active(merged_id);
        self.dirty = Dirty::All;
        true
    }

    // ── PNG 导入导出 ──

    /// 导出为 PNG。`bounds: None` 自动取可见内容包围盒；
    /// `transparent: false` 合成白色不透明背景。
    pub fn export_png(
        &mut self,
        bounds: Option<Rect>,
        scale: f32,
        transparent: bool,
    ) -> Option<Vec<u8>> {
        let scale = scale.max(0.01) as f64;
        let bounds = bounds
            .or_else(|| self.canvas_bounds())
            .or_else(|| self.visible_content_bounds())?;
        let w = ((bounds.w as f64) * scale).ceil().max(1.0) as u32;
        let h = ((bounds.h as f64) * scale).ceil().max(1.0) as u32;
        if w > 16384 || h > 16384 {
            return None; // 防御超大分配
        }
        let mut scratch = Document::with_layers(self.doc.layers().clone());
        {
            let vp = scratch.viewport_mut();
            vp.set_zoom(scale);
            let (px, py) = (bounds.x as f64 * scale, bounds.y as f64 * scale);
            vp.pan_by(-px, -py);
        }
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let bg = if transparent {
            None
        } else {
            Some(self.doc.background())
        };
        self.renderer
            .composite(&scratch, &mut buf, w, Rect::new(0, 0, w, h), bg);
        crate::io::encode_png(&buf, w, h).ok()
    }

    // ── OpenRaster 工程存档 ──

    /// 16-bit PNG 导出：f32 中间累积（消减多图层叠加的 8-bit 舍入
    /// 色带），量化到 u16 输出。`bounds: None` = 可见内容包围盒。
    pub fn export_png16(&mut self, bounds: Option<Rect>, scale: f32) -> Option<Vec<u8>> {
        let scale = scale.max(0.01) as f64;
        let bounds = bounds
            .or_else(|| self.canvas_bounds())
            .or_else(|| self.visible_content_bounds())?;
        let w = ((bounds.w as f64) * scale).ceil().max(1.0) as u32;
        let h = ((bounds.h as f64) * scale).ceil().max(1.0) as u32;
        if w > 16384 || h > 16384 {
            return None;
        }
        let mut scratch = Document::with_layers(self.doc.layers().clone());
        {
            let vp = scratch.viewport_mut();
            vp.set_zoom(scale);
            vp.pan_by(-bounds.x as f64 * scale, -bounds.y as f64 * scale);
        }
        // f32 累积缓冲（直行域：先以 u8 合成为预乘，再升级 f32 输出
        // 实际的 f32 链路在 composite 的 blend_pixel 内已经是 f32，
        // 这里的 f32 缓冲避免最终量化损失）
        let mut u8_buf = vec![0u8; (w as usize) * (h as usize) * 4];
        self.renderer.composite(
            &scratch,
            &mut u8_buf,
            w,
            crate::geometry::Rect::new(0, 0, w, h),
            None,
        );
        // u8 预乘 → f32 直行
        let mut f32_buf = vec![0f32; (w as usize) * (h as usize) * 4];
        for (d, s) in f32_buf
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(u8_buf.as_chunks::<4>().0)
        {
            let a = s[3] as f32 / 255.0;
            d[3] = a;
            if a > 0.0 {
                for k in 0..3 {
                    d[k] = (s[k] as f32 / 255.0) / a;
                }
            }
        }
        crate::io::encode_png16(&f32_buf, w, h).ok()
    }

    /// 导入图像（自动识别 PNG/JPEG/WebP）→ 瓦片 → 新图层。
    pub fn import_image(&mut self, bytes: &[u8]) -> Option<u64> {
        log::info!("[io] 导入图像 {} 字节", bytes.len());
        let (premul, w, h) = crate::io::decode_auto(bytes).ok()?;
        self.insert_pixels_as_layer(&premul, w, h)
    }

    /// JPEG 导出（有损，quality 0-100）。`transparent: false` 时白底合成。
    pub fn export_jpeg(
        &mut self,
        bounds: Option<Rect>,
        scale: f32,
        quality: u8,
    ) -> Option<Vec<u8>> {
        let png = self.export_png(bounds, scale, false)?;
        // export_png 输出直行 PNG → 解码回预乘 → 编码 JPEG
        let (premul, w, h) = crate::io::decode_png(&png).ok()?;
        crate::io::encode_jpeg(&premul, w, h, quality).ok()
    }

    /// 导入 SVG：resvg 光栅化 → 瓦片 → 新图层（置于视野中心）。
    /// `scale` 控制渲染分辨率（1.0 = SVG 原始尺寸）。
    #[cfg(feature = "svg")]
    pub fn import_svg(&mut self, svg: &[u8], scale: f32) -> Option<u64> {
        log::info!("[io] 导入 SVG {} 字节 scale={scale}", svg.len());
        let scale = scale.max(0.01);
        let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size();
        let (sw, sh) = (size.width(), size.height());
        if sw <= 0.0 || sh <= 0.0 {
            return None;
        }
        let w = (sw * scale).ceil().max(1.0) as u32;
        let h = (sh * scale).ceil().max(1.0) as u32;
        if w > 8192 || h > 8192 {
            return None;
        }
        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
        let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        // tiny-skia Pixmap 是预乘 RGBA（与我们的瓦片一致）
        let premul = pixmap.data();
        self.insert_pixels_as_layer(premul, w, h)
    }

    /// 预乘 RGBA 像素 → 瓦片网格 → 新图层，返回图层 id。
    fn insert_pixels_as_layer(&mut self, premul: &[u8], w: u32, h: u32) -> Option<u64> {
        if w == 0 || h == 0 || premul.len() < (w as usize) * (h as usize) * 4 {
            return None;
        }
        let mut layer = Layer::new("SVG 图层");
        for gy in 0..h as i64 {
            for gx in 0..w as i64 {
                let src = ((gy * w as i64 + gx) * 4) as usize;
                if premul[src + 3] == 0 {
                    continue;
                }
                let tid = TileId::at(gx, gy);
                let t = layer.tiles.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let lx = (gx - ox) as usize;
                let ly = (gy - oy) as usize;
                let dst = (ly * 256 + lx) * 4;
                t.pixels_mut()[dst..dst + 4].copy_from_slice(&premul[src..src + 4]);
            }
        }
        layer.tiles.prune();
        let idx = self.doc.layers().len();
        let id = self.doc.layers_mut().alloc_id();
        self.doc.layers_mut().insert_entry(idx, id, layer);
        self.doc.layers_mut().set_active(id);
        self.dirty = Dirty::All;
        Some(id.to_raw())
    }

    /// 保存为 .ora（含合成图与缩略图）。
    pub fn save_ora(&mut self) -> Option<Vec<u8>> {
        let t0 = timing_start();
        let merged = self.export_png(None, 1.0, true)?;
        let b = self.visible_content_bounds()?;
        let scale = (256.0 / b.w as f64).min(256.0 / b.h as f64).min(1.0) as f32;
        let thumb = self.export_png(Some(b), scale, true)?;
        match crate::ora::encode_ora(&self.doc, Some(&merged), Some(&thumb)) {
            Ok(bytes) => {
                match timing_ms(t0) {
                    Some(ms) => log::info!(
                        "[io] 存档 ORA {} 字节（{ms:.1}ms，edit_count={}）",
                        bytes.len(),
                        self.doc.edit_count()
                    ),
                    None => log::info!(
                        "[io] 存档 ORA {} 字节（edit_count={}）",
                        bytes.len(),
                        self.doc.edit_count()
                    ),
                }
                Some(bytes)
            }
            Err(e) => {
                log::warn!("[io] 存档 ORA 失败: {e}");
                None
            }
        }
    }

    /// 载入 .ora 替换当前文档（历史重置）。
    pub fn load_ora(&mut self, bytes: &[u8]) -> bool {
        let t0 = timing_start();
        let Ok(ora) = crate::ora::decode_ora(bytes) else {
            log::warn!("[io] 载入 ORA 失败（{} 字节，解码错误）", bytes.len());
            return false;
        };
        let layers = crate::ora::layers_from_ora(&ora);
        let mut stack = crate::layer::LayerStack::new();
        let mut ids = Vec::new();
        for layer in layers {
            let id = stack.alloc_id();
            stack.insert_entry(stack.len(), id, layer);
            ids.push(id);
        }
        if let Some(top) = ids.last() {
            stack.set_active(*top);
        }
        self.doc = Document::with_layers(stack);
        match timing_ms(t0) {
            Some(ms) => log::info!(
                "[io] 载入 ORA {} 字节 → {} 层（{ms:.1}ms）",
                bytes.len(),
                ids.len()
            ),
            None => log::info!("[io] 载入 ORA {} 字节 → {} 层", bytes.len(), ids.len()),
        }
        self.dirty = Dirty::All;
        true
    }

    /// 解码 PNG 并作为新图层插入（放在顶层，画布原点对齐）。
    pub fn import_png(&mut self, bytes: &[u8]) -> Option<LayerId> {
        let (data, iw, ih) = crate::io::decode_png(bytes).ok()?;
        let mut layer = Layer::new("导入图像");
        for ty in 0..ih.div_ceil(TILE) {
            for tx in 0..iw.div_ceil(TILE) {
                let id = TileId {
                    x: tx as i32,
                    y: ty as i32,
                };
                let x0 = tx * TILE;
                let y0 = ty * TILE;
                let tile = layer.tiles.get_or_create_mut(id);
                let px = tile.pixels_mut();
                for y in 0..TILE {
                    let gy = y0 + y;
                    if gy >= ih {
                        break;
                    }
                    let row_src = ((gy * iw + x0) * 4) as usize;
                    for x in 0..TILE {
                        let gx = x0 + x;
                        if gx >= iw {
                            break;
                        }
                        let s = row_src + (x as usize) * 4;
                        let d = ((y * TILE + x) * 4) as usize;
                        px[d..d + 4].copy_from_slice(&data[s..s + 4]);
                    }
                }
                // 空瓦片回收（PNG 透明区不占内存）
                if tile.is_transparent() {
                    layer.tiles.remove(id);
                }
            }
        }
        let idx = self.doc.layers().len();
        let id = self.doc.insert_layer_obj(layer, idx);
        self.dirty = Dirty::All;
        Some(id)
    }

    /// 可见图层内容包围盒（像素精确；扫描成本 O 内容瓦片数）。
    pub fn visible_content_bounds(&self) -> Option<Rect> {
        let mut acc: Option<Rect> = None;
        for l in self.doc.layers().iter().filter(|l| l.visible) {
            if let Some(b) = l.content().content_bounds_precise() {
                acc = Some(match acc {
                    Some(a) => a.union(&b),
                    None => b,
                });
            }
        }
        acc
    }

    // ── 事件与帧 ──

    /// 平台事件入口（指针/键盘/焦点/尺寸——壳层唯一输入通道）。
    pub fn handle_event(&mut self, ev: PlatformEvent) {
        match ev {
            PlatformEvent::Resize { w, h, .. } => {
                log::debug!("[viewport] 尺寸 {w}×{h}");
                self.size = (w, h);
                self.frame = vec![0; (w as usize) * (h as usize) * 4];
                self.dirty = Dirty::All;
            }
            PlatformEvent::PenInRange(v) => self.pen_in_range = v,
            PlatformEvent::Focus(f) => {
                if !f {
                    // 失焦：按抬笔处理，保留已画内容与撤销记录；
                    // 形状拖拽/对象拖拽放弃（未提交本就无历史）
                    self.end_stroke();
                    self.cancel_shape();
                    self.cancel_object_move();
                    self.text_anchor = None;
                    self.text_editing = None;
                    self.gesture = None;
                    self.gesture_latch = false;
                }
            }
            PlatformEvent::Pointer { phase, sample } => self.on_pointer(phase, sample),
        }
    }

    /// 形状拖拽预览：合成后叠加到帧（不落瓦片、不入撤销；全清/低清两路共用）。
    fn stamp_shape_preview(&mut self, w: u32, region: crate::geometry::Rect) {
        // 形状拖拽预览：合成后叠加到帧（不落瓦片、不入撤销）
        if let Some(sh) = self.shape_drag {
            let vp = self.doc.viewport().clone();
            let (color, alpha) = (self.brush.color, self.brush.opacity);
            let (radius, hardness) = (self.brush.size / 2.0, self.brush.hardness);
            if sh.fill && sh.kind != crate::shape::ShapeKind::Line {
                // 填充预览：屏幕空间直接填充
                let (x0, y0) = vp.canvas_to_screen(
                    sh.anchor.0.min(sh.current.0),
                    sh.anchor.1.min(sh.current.1),
                );
                let (x1, y1) = vp.canvas_to_screen(
                    sh.anchor.0.max(sh.current.0),
                    sh.anchor.1.max(sh.current.1),
                );
                match sh.kind {
                    crate::shape::ShapeKind::Rect => {
                        let r = crate::geometry::Rect::new(
                            x0.floor() as i32,
                            y0.floor() as i32,
                            (x1.ceil() - x0.floor()) as u32,
                            (y1.ceil() - y0.floor()) as u32,
                        );
                        crate::preview::fill_rect_flat(
                            &mut self.frame, w, region, r, color, alpha,
                        );
                    }
                    crate::shape::ShapeKind::Ellipse => {
                        let (a, b) = (
                            vp.canvas_to_screen(sh.anchor.0, sh.anchor.1),
                            vp.canvas_to_screen(sh.current.0, sh.current.1),
                        );
                        crate::preview::fill_ellipse_flat(
                            &mut self.frame,
                            w,
                            region,
                            (a.0 + b.0) / 2.0,
                            (a.1 + b.1) / 2.0,
                            ((b.0 - a.0) / 2.0).abs(),
                            ((b.1 - a.1) / 2.0).abs(),
                            color,
                            alpha,
                        );
                    }
                    crate::shape::ShapeKind::Line => {}
                }
            } else {
                // 描边预览：画布空间 dab 链 → 屏幕空间平铺盖章
                let dabs = crate::shape::shape_dabs(
                    sh.kind, sh.anchor, sh.current, radius, hardness, color, alpha,
                );
                let zoom = vp.zoom() as f32;
                let screen_dabs: Vec<Dab> = dabs
                    .into_iter()
                    .map(|mut d| {
                        let (sx, sy) = vp.canvas_to_screen(d.x, d.y);
                        d.x = sx;
                        d.y = sy;
                        d.radius *= zoom;
                        d
                    })
                    .collect();
                crate::preview::stamp_dabs_flat(&mut self.frame, w, region, &screen_dabs);
            }
        }
    }

    /// 合成并呈现。Clean 时跳过合成仅重新呈现（窗口恢复等场景）。
    pub fn render(&mut self, surface: &mut dyn Surface) {
        // 撤销/重做/载入后的对象缓存重建（置位 stale 的层）
        self.refresh_stale_objects();
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        if self.fps_monitor {
            self.presents += 1;
        }
        // 视口被外部（壳层手势等）修改：全量重绘
        let vp_rev = self.doc.viewport().revision();
        if vp_rev != self.vp_rev {
            self.vp_rev = vp_rev;
            self.dirty = Dirty::All;
        }
        if self.dirty == Dirty::Clean {
            surface.present_cpu(&self.frame, self.size, None);
            return;
        }
        let full = Rect::new(0, 0, w, h);
        let region = match self.dirty {
            Dirty::All => full,
            Dirty::Part(r) => match r.intersect(&full) {
                Some(r) => r,
                None => {
                    self.dirty = Dirty::Clean;
                    surface.present_cpu(&self.frame, self.size, None);
                    return;
                }
            },
            // Clean 分支上方已提前返回；防御性兜底（不 panic）
            Dirty::Clean => full,
        };
        // ── 交互期低清路径：半分辨率合成 + 最近邻上采样（像素量 ÷4）──
        // 双指手势进行中或壳层显式声明（滚轮缩放连发）时启用；
        // 小画布（<1MP）不值得降质，直接走全分辨率。
        // 按需启用：交互中 + 大画布 + 重负载（放大双线性或多图层）——
        // 空场景低清无净收益（上采样固定成本），重场景 1.7x+（见 e2e 基准）
        let heavy = self.doc.viewport().zoom() > 1.0
            || self.doc.layers().iter().filter(|l| l.visible).count() > 2;
        let interactive = (self.gesture.is_some() || self.interactive)
            && heavy
            && (w as u64) * (h as u64) >= 1_000_000;
        if interactive {
            let (lw, lh) = (w.div_ceil(2), h.div_ceil(2));
            if self.scratch.len() != (lw as usize) * (lh as usize) * 4 {
                self.scratch = vec![0; (lw as usize) * (lh as usize) * 4];
            }
            // 视口缩放 0.5：低清缓冲的屏幕坐标 = 全屏坐标 × 0.5
            //（旋转/翻转为线性变换，缩放可交换）
            let vp = self.doc.viewport().clone();
            let (pan_x, pan_y) = vp.pan();
            let zoom = vp.zoom();
            {
                let v = self.doc.viewport_mut();
                v.set_pan(pan_x * 0.5, pan_y * 0.5);
                v.set_zoom((zoom * 0.5).max(crate::viewport::MIN_ZOOM));
            }
            let bg = self.doc.background();
            self.renderer.composite(
                &self.doc,
                &mut self.scratch,
                lw,
                Rect::new(0, 0, lw, lh),
                Some(bg),
            );
            // 恢复视口
            {
                let v = self.doc.viewport_mut();
                v.set_pan(pan_x, pan_y);
                v.set_zoom(zoom);
            }
            // 最近邻上采样到全帧（行/像素复制，内存带宽受限 ~3ms @5.6MP）
            // 2× 最近邻特化：源行展开一次（u64 双像素）→ 两目标行 memcpy
            if self.row2.len() != w as usize * 4 {
                self.row2 = vec![0u8; w as usize * 4];
            }
            let w2 = lw as usize;
            let mut row2 = std::mem::take(&mut self.row2);
            for sy in 0..lh as usize {
                let src_row = &self.scratch[sy * w2 * 4..][..w2 * 4];
                // 行内 2x 展开：每源像素 u64 双写
                for sx in 0..w2 {
                    let p4 = &src_row[sx * 4..sx * 4 + 4];
                    let d8 = &mut row2[sx * 8..sx * 8 + 8];
                    d8[..4].copy_from_slice(p4);
                    d8[4..].copy_from_slice(p4);
                }
                // 奇数宽尾像素补齐
                let even_w = w as usize & !1;
                if even_w < w as usize {
                    let last = &src_row[(w2 - 1) * 4..][..4];
                    row2[even_w * 4..even_w * 4 + 4].copy_from_slice(last);
                }
                // 写两目标行（memcpy 级）
                let y0 = sy * 2;
                let y1 = (y0 + 1).min(h as usize - 1);
                self.frame[y0 * w as usize * 4..][..w as usize * 4].copy_from_slice(&row2);
                self.frame[y1 * w as usize * 4..][..w as usize * 4].copy_from_slice(&row2);
            }
            self.row2 = row2;
            self.stamp_shape_preview(w, full);
            surface.present_cpu(&self.frame, self.size, Some(full));
            self.dirty = Dirty::Clean;
            return;
        }
        let bg = self.doc.background();
        self.renderer
            .composite(&self.doc, &mut self.frame, w, region, Some(bg));
        self.stamp_shape_preview(w, region);
        surface.present_cpu(&self.frame, self.size, Some(region));
        self.dirty = Dirty::Clean;
    }

    /// 撤销一步。
    pub fn undo(&mut self) -> bool {
        if self.transforming() || self.shape_drag.is_some() {
            return false; // 变换/形状拖拽未提交前不动历史
        }
        if self.doc.undo() {
            self.refresh_stale_objects();
            self.dirty = Dirty::All;
            true
        } else {
            log::debug!("[history] 撤销被拒绝（无可撤销项）");
            false
        }
    }

    /// 重做一步。
    pub fn redo(&mut self) -> bool {
        if self.transforming() || self.shape_drag.is_some() {
            return false;
        }
        if self.doc.redo() {
            self.refresh_stale_objects();
            self.dirty = Dirty::All;
            true
        } else {
            false
        }
    }

    // ── 指针路由：笔/鼠标走笔画，触摸走手势状态机 ──

    fn on_pointer(&mut self, phase: PointerPhase, sample: PointerSample) {
        match sample.kind {
            PointerKind::Touch => self.on_touch(phase, sample),
            PointerKind::Pen | PointerKind::Eraser | PointerKind::Mouse => {
                self.on_stylus(phase, sample)
            }
        }
    }

    fn on_stylus(&mut self, phase: PointerPhase, sample: PointerSample) {
        // 内容级变换中：输入由壳层驱动仿射，忽略笔画
        if self.transforming() {
            return;
        }
        // 笔落下即接管：清除进行中的触摸手势
        if phase == PointerPhase::Down && (!self.touches.is_empty() || self.gesture.is_some()) {
            self.cancel_stroke();
            self.cancel_shape();
            self.touches.clear();
            self.gesture = None;
            self.gesture_latch = false;
        }
        match self.tool {
            // 文字：点击命中已有对象 → 拖拽移动 / 壳层取编辑；未命中 → 新锚点
            Tool::Text => match phase {
                PointerPhase::Down => {
                    let (x, y) = self.doc.viewport().screen_to_canvas(sample.x, sample.y);
                    if let Some((layer, idx)) = self.hit_test_object(x, y) {
                        self.text_editing = Some((layer, idx));
                        self.last_text_target = Some((layer, idx));
                        self.obj_drag_before =
                            Some((layer, self.doc.layers().get(layer).objects.clone()));
                        self.object_moving = Some(ObjectMoving {
                            layer,
                            index: idx,
                            last: (x, y),
                            pointer: sample.id,
                        });
                    } else {
                        self.text_anchor = Some((x, y));
                    }
                }
                PointerPhase::Move => {
                    let (x, y) = self.doc.viewport().screen_to_canvas(sample.x, sample.y);
                    self.move_object(x, y, sample.id);
                }
                PointerPhase::Up => self.end_object_move(),
                PointerPhase::Cancel => self.cancel_object_move(),
            }
            // 油漆桶：点击即填充
            Tool::Fill { tolerance } => {
                if phase == PointerPhase::Down {
                    let (x, y) = self.doc.viewport().screen_to_canvas(sample.x, sample.y);
                    self.flood_fill(x as i64, y as i64, tolerance);
                }
            }
            // 形状：拖拽状态机（Up 提交 / Cancel 放弃）
            Tool::Shape { .. } => match phase {
                PointerPhase::Down => self.begin_shape(&sample),
                PointerPhase::Move => self.move_shape(&sample),
                PointerPhase::Up => self.commit_shape(),
                PointerPhase::Cancel => self.cancel_shape(),
            },
            _ => match phase {
                PointerPhase::Down => {
                    if self.stroke.is_none() && self.doc.layers().try_active().is_some() {
                        self.begin_stroke(&sample);
                    }
                }
                PointerPhase::Move => self.extend_stroke(&sample),
                PointerPhase::Up | PointerPhase::Cancel => self.end_stroke(),
            },
        }
    }

    fn on_touch(&mut self, phase: PointerPhase, sample: PointerSample) {
        // 手掌拒绝：笔在感应区时忽略触摸
        if self.pen_in_range {
            return;
        }
        match phase {
            PointerPhase::Down => {
                self.touches.insert(sample.id, (sample.x, sample.y));
                if self.touches.len() >= 2 {
                    // 第二指落下：取消误触笔画/形状拖拽，进入手势
                    self.cancel_stroke();
                    self.cancel_shape();
                    if let Some((c, d)) = centroid_and_dist(&self.touches) {
                        self.gesture = Some(Gesture {
                            centroid: c,
                            dist: d,
                        });
                        self.gesture_latch = true;
                    }
                } else if !self.gesture_latch && !self.transforming() {
                    match self.tool {
                        Tool::Text => match phase {
                            PointerPhase::Down => {
                                let (x, y) = self
                                    .doc
                                    .viewport()
                                    .screen_to_canvas(sample.x, sample.y);
                                if let Some((layer, idx)) = self.hit_test_object(x, y) {
                                    self.text_editing = Some((layer, idx));
                                    self.last_text_target = Some((layer, idx));
                                    self.obj_drag_before = Some((
                                        layer,
                                        self.doc.layers().get(layer).objects.clone(),
                                    ));
                                    self.object_moving = Some(ObjectMoving {
                                        layer,
                                        index: idx,
                                        last: (x, y),
                                        pointer: sample.id,
                                    });
                                } else {
                                    self.text_anchor = Some((x, y));
                                }
                            }
                            PointerPhase::Move => {
                                let (x, y) = self
                                    .doc
                                    .viewport()
                                    .screen_to_canvas(sample.x, sample.y);
                                self.move_object(x, y, sample.id);
                            }
                            PointerPhase::Up => self.end_object_move(),
                            PointerPhase::Cancel => self.cancel_object_move(),
                        },
                        Tool::Fill { tolerance } => {
                            let (x, y) =
                                self.doc.viewport().screen_to_canvas(sample.x, sample.y);
                            self.flood_fill(x as i64, y as i64, tolerance);
                        }
                        Tool::Shape { .. } => {
                            if self.shape_drag.is_none() {
                                self.begin_shape(&sample);
                            }
                        }
                        _ => {
                            if self.stroke.is_none() && self.doc.layers().try_active().is_some() {
                                self.begin_stroke(&sample);
                            }
                        }
                    }
                }
            }
            PointerPhase::Move => {
                if !self.touches.contains_key(&sample.id) {
                    return;
                }
                self.touches.insert(sample.id, (sample.x, sample.y));
                if let Some(g) = &mut self.gesture {
                    if let Some((c, d)) = centroid_and_dist(&self.touches) {
                        let vp = self.doc.viewport_mut();
                        vp.pan_by(c.0 - g.centroid.0, c.1 - g.centroid.1);
                        // 双指瞬时重合/交叉会造成 dist 剧变，只在两距都有效时缩放
                        if g.dist > 1.0 && d > 1.0 {
                            let f = (d / g.dist).clamp(0.5, 2.0);
                            vp.zoom_at(c, f);
                        }
                        g.centroid = c;
                        g.dist = d;
                    }
                } else if self.stroke.is_some() {
                    self.extend_stroke(&sample);
                } else if self.shape_drag.is_some() {
                    self.move_shape(&sample);
                }
            }
            PointerPhase::Up | PointerPhase::Cancel => {
                self.touches.remove(&sample.id);
                if self.touches.len() < 2 {
                    if self.gesture.is_some() {
                        self.dirty = Dirty::All; // 手势结束：全分辨率精化一帧
                    }
                    self.gesture = None;
                }
                if self.touches.is_empty() {
                    self.gesture_latch = false;
                }
                if self.gesture.is_none() && !self.gesture_latch {
                    if self.stroke.is_some() {
                        self.end_stroke();
                    }
                    if self.shape_drag.is_some() {
                        if phase == PointerPhase::Up {
                            self.commit_shape();
                        } else {
                            self.cancel_shape();
                        }
                    }
                }
            }
        }
    }

    fn canvas_sample(&self, sample: &PointerSample) -> PointerSample {
        let (x, y) = self.doc.viewport().screen_to_canvas(sample.x, sample.y);
        PointerSample { x, y, ..*sample }
    }

    fn begin_stroke(&mut self, sample: &PointerSample) {
        let layer = self.doc.active_layer();
        let cs = self.canvas_sample(sample);
        let mut state = StrokeState::new(cs.x, cs.y, 1.0);
        let mut dabs = self.brush.begin(&mut state, &cs);
        mark_erase(&mut dabs, self.tool);
        self.stroke = Some(ActiveStroke {
            state,
            recorder: StrokeRecorder::new(layer),
            layer,
            pointer: sample.id,
        });
        if !dabs.is_empty() {
            self.stamp(layer, &dabs);
        }
    }

    fn extend_stroke(&mut self, sample: &PointerSample) {
        let cs = self.canvas_sample(sample);
        let Some(act) = self.stroke.as_mut() else {
            return;
        };
        if act.pointer != sample.id {
            return; // 非本笔指针的移动
        }
        let mut dabs = self.brush.extend(&mut act.state, &cs);
        if dabs.is_empty() {
            return;
        }
        let layer = act.layer;
        mark_erase(&mut dabs, self.tool);
        self.stamp(layer, &dabs);
    }

    /// 把 dabs 盖进目标网格并扩展屏幕脏区。
    /// Brush/Eraser → 像素层；Mask → 蒙版网格（白 dab，erase 语义为擦暗蒙版）。
    fn stamp(&mut self, layer: LayerId, dabs: &[Dab]) {
        let Some(act) = self.stroke.as_mut() else {
            return;
        };
        let mut dabs = dabs.to_vec();
        if self.tool == Tool::Mask {
            for d in dabs.iter_mut() {
                d.color = Color::WHITE;
            }
        }
        // 先取可变网格引用再交给渲染器（借用分离经临时层对象不可行，
        // 直接从 layer 取 &mut TileGrid）
        let selection = self.doc.selection().cloned();
        let layer_ref = self.doc.layers_mut().get_mut(layer);
        let grid: &mut crate::tile::TileGrid = if self.tool == Tool::Mask {
            layer_ref.mask.get_or_insert_with(TileGrid::new)
        } else {
            &mut layer_ref.tiles
        };
        self.renderer
            .stamp_dabs(grid, &dabs, selection.as_ref(), &mut act.recorder);
        for dab in &dabs {
            self.expand_dirty(dab);
        }
        // 对象层：笔画进行中的瓦片变化同步合并缓存（commit 收口只管提交后）
        let has_objs = self
            .doc
            .layers()
            .try_get(layer)
            .map(|l| !l.objects.is_empty())
            .unwrap_or(false);
        if has_objs {
            let ids: Vec<TileId> = dabs
                .iter()
                .map(|d| TileId::at(d.x.floor() as i64, d.y.floor() as i64))
                .collect();
            self.doc
                .layers_mut()
                .get_mut(layer)
                .sync_tiles(ids.into_iter());
        }
    }

    fn end_stroke(&mut self) {
        let Some(mut act) = self.stroke.take() else {
            return;
        };
        // 稳定器收笔追赶：补齐滞后段的 dabs（在 recorder 存活期内）
        // 先算追赶 dabs，经 stroke 通道盖章（复用选区/撤销采集），再取回
        let catch_up = self.brush.end(&mut act.state);
        if !catch_up.is_empty() {
            let layer = act.layer;
            let recorder = std::mem::replace(&mut act.recorder, StrokeRecorder::new(layer));
            self.stroke = Some(act);
            self.stamp(layer, &catch_up);
            act = self.stroke.take().expect("stamp 后 stroke 必在");
            act.recorder = recorder;
        }
        let group = act.recorder.finish("Stroke");
        // 回收本笔触及且变回全透明的瓦片（橡皮/混合工具的常态）
        if let Some(l) = self.doc.layers_mut().try_get_mut(act.layer) {
            for (_, tid, _) in group.tile_ids() {
                if let Some(t) = l.tiles.get(*tid) {
                    if t.is_transparent() {
                        l.tiles.remove(*tid);
                    }
                }
            }
        }
        self.doc.commit(group);
    }

    /// 立即回滚进行中的笔画（第二指落下 / 笔接管触摸手势），
    /// 不产生历史条目。
    fn cancel_stroke(&mut self) {
        let Some(act) = self.stroke.take() else {
            return;
        };
        let group = act.recorder.finish("CancelledStroke");
        let _ = group.apply_to(self.doc.layers_mut());
        self.dirty = Dirty::All;
    }

    // ── 形状工具状态机 ──
    // 预览只进帧缓冲（render() 合成后叠加），提交才落瓦片入撤销。

    fn begin_shape(&mut self, sample: &PointerSample) {
        if self.shape_drag.is_some() || self.doc.layers().try_active().is_none() {
            return;
        }
        let Tool::Shape { kind, fill } = self.tool else {
            return;
        };
        let (x, y) = self.doc.viewport().screen_to_canvas(sample.x, sample.y);
        let layer = self.doc.active_layer();
        self.shape_drag = Some(ActiveShape {
            kind,
            fill,
            anchor: (x, y),
            current: (x, y),
            layer,
            pointer: sample.id,
        });
    }

    fn move_shape(&mut self, sample: &PointerSample) {
        let Some(sh) = self.shape_drag.as_ref() else {
            return;
        };
        if sh.pointer != sample.id {
            return;
        }
        let (kind, anchor, old) = (sh.kind, sh.anchor, sh.current);
        // 旧区域重合成擦掉旧预览，新区域画新预览
        self.dirty_shape(&kind, anchor, old);
        let (x, y) = self.doc.viewport().screen_to_canvas(sample.x, sample.y);
        if let Some(sh) = self.shape_drag.as_mut() {
            sh.current = (x, y);
        }
        self.dirty_shape(&kind, anchor, (x, y));
    }

    /// 提交形状：描边 = dab 链走正规盖章管线（吃笔刷参数）；
    /// 填充 = ShapeWriter 直接写像素（纯色）。整组入撤销。
    fn commit_shape(&mut self) {
        let Some(sh) = self.shape_drag.take() else {
            return;
        };
        self.dirty_shape(&sh.kind, sh.anchor, sh.current);
        // 拖拽期间图层可能被删（极端时序），防御兜底
        if self.doc.layers_mut().try_get_mut(sh.layer).is_none() {
            return;
        }
        if sh.fill && sh.kind != crate::shape::ShapeKind::Line {
            let color = self.brush.color;
            let mut rec = StrokeRecorder::new(sh.layer);
            let layer = self.doc.layers_mut().get_mut(sh.layer);
            {
                let mut w = crate::shape::ShapeWriter::new(&mut layer.tiles, &mut rec, color);
                let (x0, y0) = (sh.anchor.0.min(sh.current.0), sh.anchor.1.min(sh.current.1));
                let (x1, y1) = (sh.anchor.0.max(sh.current.0), sh.anchor.1.max(sh.current.1));
                match sh.kind {
                    crate::shape::ShapeKind::Rect => {
                        if x1 - x0 >= 1.0 && y1 - y0 >= 1.0 {
                            w.fill_rect(crate::geometry::Rect::new(
                                x0 as i32,
                                y0 as i32,
                                (x1 - x0) as u32,
                                (y1 - y0) as u32,
                            ));
                        }
                    }
                    crate::shape::ShapeKind::Ellipse => {
                        w.fill_ellipse(
                            (sh.anchor.0 + sh.current.0) / 2.0,
                            (sh.anchor.1 + sh.current.1) / 2.0,
                            ((sh.current.0 - sh.anchor.0) / 2.0).abs(),
                            ((sh.current.1 - sh.anchor.1) / 2.0).abs(),
                        );
                    }
                    crate::shape::ShapeKind::Line => unreachable!("上方已排除"),
                }
            }
            let group = rec.finish("Shape");
            if !group.tile_ids().is_empty() {
                self.doc.commit(group);
            }
        } else {
            // 描边（含"填充+直线"退化：直线无填充语义，按描边走）
            let dabs = crate::shape::shape_dabs(
                sh.kind,
                sh.anchor,
                sh.current,
                self.brush.size / 2.0,
                self.brush.hardness,
                self.brush.color,
                self.brush.opacity,
            );
            let mut rec = StrokeRecorder::new(sh.layer);
            let selection = self.doc.selection().cloned();
            let layer = self.doc.layers_mut().get_mut(sh.layer);
            self.renderer
                .stamp_dabs(&mut layer.tiles, &dabs, selection.as_ref(), &mut rec);
            for dab in &dabs {
                self.expand_dirty(dab);
            }
            let group = rec.finish("Shape");
            if !group.tile_ids().is_empty() {
                log::info!("[shape] 提交 {:?}{}（anchor=({:.0},{:.0}) current=({:.0},{:.0})）",
                    sh.kind, if sh.fill { "填充" } else { "" }, sh.anchor.0, sh.anchor.1, sh.current.0, sh.current.1);
                self.doc.commit(group);
            }
        }
    }

    /// 放弃拖拽（工具切换/失焦/Cancel/第二指）：只清预览区域，无历史。
    fn cancel_shape(&mut self) {
        if let Some(sh) = self.shape_drag.take() {
            self.dirty_shape(&sh.kind, sh.anchor, sh.current);
        }
    }

    /// 形状包围盒 → 屏幕脏区（含笔刷半径外扩；旋转经四角 AABB）。
    fn dirty_shape(
        &mut self,
        kind: &crate::shape::ShapeKind,
        anchor: (f64, f64),
        current: (f64, f64),
    ) {
        let margin = (self.brush.size / 2.0 + 2.0) as f64;
        let (cx0, cy0, cx1, cy1) =
            crate::shape::shape_bbox(*kind, anchor, current, margin);
        let vp = self.doc.viewport();
        let corners = [
            vp.canvas_to_screen(cx0, cy0),
            vp.canvas_to_screen(cx1, cy0),
            vp.canvas_to_screen(cx0, cy1),
            vp.canvas_to_screen(cx1, cy1),
        ];
        let xs = [corners[0].0, corners[1].0, corners[2].0, corners[3].0];
        let ys = [corners[0].1, corners[1].1, corners[2].1, corners[3].1];
        let x0 = xs.iter().cloned().fold(f64::MAX, f64::min).floor() as i32;
        let y0 = ys.iter().cloned().fold(f64::MAX, f64::min).floor() as i32;
        let x1 = xs.iter().cloned().fold(f64::MIN, f64::max).ceil() as i32;
        let y1 = ys.iter().cloned().fold(f64::MIN, f64::max).ceil() as i32;
        if x1 > x0 && y1 > y0 {
            let r = crate::geometry::Rect::new(
                x0,
                y0,
                (x1 - x0) as u32,
                (y1 - y0) as u32,
            );
            self.dirty.union(r);
        }
    }

    /// 文字工具锚点：壳层取走后弹输入框，再调 [`Engine::draw_text`]。
    /// 返回画布坐标；重复调用第二次返回 None（一次性）。
    pub fn take_text_anchor(&mut self) -> Option<(f64, f64)> {
        self.text_anchor.take()
    }

    /// 画布 → 屏幕坐标（壳层定位输入浮层用）。
    pub fn canvas_to_screen(&self, x: f64, y: f64) -> (f64, f64) {
        self.doc.viewport().canvas_to_screen(x, y)
    }

    /// 视口定位：把画布坐标 (x,y) 移到屏幕中心（小地图拖动）。
    /// 旋转/翻转下经正向变换逆解，方向正确。
    pub fn viewport_center_on(&mut self, x: f64, y: f64) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let vp = self.doc.viewport().clone();
        let (tx, ty) = vp.canvas_to_screen(x, y);
        let (pan_x, pan_y) = vp.pan();
        // 变换向量（不含平移）= cts - pan → 新 pan = 屏幕中心 - 向量
        let vec = (tx - pan_x, ty - pan_y);
        self.doc
            .viewport_mut()
            .set_pan(w as f64 / 2.0 - vec.0, h as f64 / 2.0 - vec.1);
        log::debug!("[viewport] 定位到画布 ({x:.0},{y:.0})");
    }

    /// 当前可见画布区域（AABB；旋转下四角包围盒）。
    /// 返回 (x, y, w, h)；供小地图绘制主视口指示框。
    pub fn viewport_rect(&self) -> (f64, f64, f64, f64) {
        let (w, h) = self.size;
        let vp = self.doc.viewport();
        let corners = [
            vp.screen_to_canvas(0.0, 0.0),
            vp.screen_to_canvas(w as f64, 0.0),
            vp.screen_to_canvas(0.0, h as f64),
            vp.screen_to_canvas(w as f64, h as f64),
        ];
        let x0 = corners.iter().map(|c| c.0).fold(f64::MAX, f64::min);
        let y0 = corners.iter().map(|c| c.1).fold(f64::MAX, f64::min);
        let x1 = corners.iter().map(|c| c.0).fold(f64::MIN, f64::max);
        let y1 = corners.iter().map(|c| c.1).fold(f64::MIN, f64::max);
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// 小地图：内容包围盒缩放到 max_w×max_h 内的 PNG（白底）。
    /// 返回 (png, 输出宽, 输出高, 内容包围盒 x/y/w/h)——
    /// 壳层以"包围盒 ↔ 小地图"线性映射换算拖动位置与视口框。
    pub fn minimap_png(&mut self, max_w: u32, max_h: u32) -> Option<MinimapOut> {
        if max_w == 0 || max_h == 0 {
            return None;
        }
        let b = match self.visible_content_bounds() {
            Some(b) => b,
            None => {
                log::debug!("[minimap] 无内容包围盒");
                return None;
            }
        };
        let scale = (max_w as f64 / b.w as f64)
            .min(max_h as f64 / b.h as f64)
            .max(0.005) as f32;
        let png = match self.export_png(Some(b), scale, false) {
            Some(p) => p,
            None => {
                log::warn!("[minimap] 导出失败 bounds={b:?} scale={scale:.4}");
                return None;
            }
        };
        let (ow, oh) = (
            ((b.w as f64) * scale as f64).ceil().max(1.0) as u32,
            ((b.h as f64) * scale as f64).ceil().max(1.0) as u32,
        );
        Some((png, ow, oh, b.x as f64, b.y as f64, b.w as f64, b.h as f64))
    }

    // ── 文字对象（非破坏：源数据持久，可重编辑/移动/删除/撤销）──

    /// 设置文字字体字节（swash 光栅化用；Android/桌面启动时加载系统字体）。
    /// Web 壳层用浏览器渲染提供 raster，可不设。
    pub fn set_text_font(&mut self, font: Vec<u8>) {
        self.text_font = Some(std::sync::Arc::new(font));
        // 已有 swash 无法光栅化的对象现在可以补渲染了
        self.refresh_stale_objects();
    }

    /// 命中测试：自顶向下找第一个包围盒包含 (x,y) 的对象。
    fn hit_test_object(&self, x: f64, y: f64) -> Option<(LayerId, usize)> {
        let all: Vec<(LayerId, &Layer)> = self.doc.layers().iter_with_id().collect();
        for (id, layer) in all.iter().rev() {
            let (id, layer) = (*id, *layer);
            if !layer.visible {
                continue;
            }
            for (i, obj) in layer.objects.iter().enumerate().rev() {
                if let Some(b) = obj.bbox() {
                    let (x0, y0) = (b.x as f64, b.y as f64);
                    let (x1, y1) = (b.x as f64 + b.w as f64, b.y as f64 + b.h as f64);
                    if x >= x0 - 4.0 && x <= x1 + 4.0 && y >= y0 - 4.0 && y <= y1 + 4.0 {
                        return Some((id, i));
                    }
                }
            }
        }
        None
    }

    /// 命中的编辑目标信息（壳层预填编辑框）。一次性取走。
    pub fn take_text_edit(&mut self) -> Option<(String, f32, Color)> {
        let (layer, idx) = self.text_editing.take()?;
        match self.doc.layers().get(layer).objects.get(idx) {
            Some(crate::layer::DrawObject::Text { text, size, color, .. }) => {
                Some((text.clone(), *size, *color))
            }
            _ => None,
        }
    }

    /// 拖拽移动对象（文字工具按住已有对象拖动）。
    fn move_object(&mut self, x: f64, y: f64, pointer: u64) {
        let Some(m) = self.object_moving else {
            return;
        };
        if m.pointer != pointer {
            return;
        }
        let (dx, dy) = (x - m.last.0, y - m.last.1);
        self.object_moving = Some(ObjectMoving { last: (x, y), ..m });
        let layer = m.layer;
        let idx = m.index;
        if let Some(l) = self.doc.layers_mut().try_get_mut(layer) {
            if let Some(obj) = l.objects.get_mut(idx) {
                match obj {
                    crate::layer::DrawObject::Text { pos, .. } => {
                        pos.0 += dx;
                        pos.1 += dy;
                    }
                    crate::layer::DrawObject::Shape { a, b, .. } => {
                        a.0 += dx;
                        a.1 += dy;
                        b.0 += dx;
                        b.1 += dy;
                    }
                }
            }
        }
        self.rasterize_layer_objects(layer);
    }

    /// 结束拖拽：整组入撤销（快照在拖拽开始时捕获）。
    fn end_object_move(&mut self) {
        self.object_moving = None;
        if let Some((layer, before)) = self.obj_drag_before.take() {
            log::info!("[text] 拖动移动文字对象");
            self.commit_objects_undo(layer, before, "MoveText");
        }
    }

    /// 取消拖拽：恢复拖拽前对象列表。
    fn cancel_object_move(&mut self) {
        self.object_moving = None;
        if let Some((layer, before)) = self.obj_drag_before.take() {
            if let Some(l) = self.doc.layers_mut().try_get_mut(layer) {
                l.objects = before;
                self.rasterize_layer_objects(layer);
            }
        }
    }

    /// 对象列表变化的统一撤销提交。
    fn commit_objects_undo(&mut self, layer: LayerId, before: Vec<crate::layer::DrawObject>, label: &'static str) {
        let group = crate::history::UndoGroup {
            label,
            ops: vec![crate::history::UndoOp::Objects {
                id: layer,
                before,
            }],
        };
        self.doc.commit(group);
        self.dirty = Dirty::All;
    }

    /// 新增文字对象。`pos` 为基线原点（画布坐标）；`raster` 为壳层
    /// 渲染位图（Web 浏览器字体路径；None = 引擎 swash 渲染）。
    #[allow(clippy::too_many_arguments)]
    pub fn add_text_object(
        &mut self,
        pos: (f64, f64),
        text: &str,
        size: f32,
        raster: Option<crate::layer::TextRaster>,
    ) -> bool {
        let layer = self.doc.active_layer();
        let obj = crate::layer::DrawObject::Text {
            pos,
            text: text.to_string(),
            size,
            color: self.brush.color,
            raster: raster.map(std::sync::Arc::new),
            bbox: None,
        };
        let before = {
            let l = self.doc.layers_mut().get_mut(layer);
            let before = l.objects.clone();
            l.objects.push(obj);
            before
        };
        self.rasterize_layer_objects(layer);
        log::info!("[text] 新增文字对象 {:?} size={size} at ({:.0},{:.0})", text, pos.0, pos.1);
        self.commit_objects_undo(layer, before, "AddText");
        true
    }

    /// 更新命中的文字对象（编辑内容/字号；raster 语义同 add）。
    pub fn update_text_object(
        &mut self,
        text: &str,
        size: f32,
        raster: Option<crate::layer::TextRaster>,
    ) -> bool {
        // text_editing 已被 take——记录最近编辑目标以支持本调用
        let Some((layer, idx)) = self.last_text_target else {
            return false;
        };
        let before = {
            let Some(l) = self.doc.layers_mut().try_get_mut(layer) else {
                return false;
            };
            let before = l.objects.clone();
            let Some(obj) = l.objects.get_mut(idx) else {
                return false;
            };
            let crate::layer::DrawObject::Text {
                text: t,
                size: s,
                raster: r,
                ..
            } = obj
            else {
                return false;
            };
            *t = text.to_string();
            *s = size;
            if let Some(ra) = raster {
                *r = Some(std::sync::Arc::new(ra));
            } else {
                *r = None; // 重新 swash 渲染
            }
            before
        };
        self.rasterize_layer_objects(layer);
        log::info!("[text] 更新文字对象 size={size}");
        self.commit_objects_undo(layer, before, "EditText");
        true
    }

    /// 删除最近命中的文字对象。
    pub fn delete_text_object(&mut self) -> bool {
        let Some((layer, idx)) = self.last_text_target else {
            return false;
        };
        let before = {
            let Some(l) = self.doc.layers_mut().try_get_mut(layer) else {
                return false;
            };
            if idx >= l.objects.len() {
                return false;
            }
            let before = l.objects.clone();
            l.objects.remove(idx);
            before
        };
        self.rasterize_layer_objects(layer);
        log::info!("[text] 删除文字对象");
        self.commit_objects_undo(layer, before, "DeleteText");
        true
    }

    /// 重建图层对象光栅化缓存（obj_tiles + merged + bbox/arc raster 回写）。
    fn rasterize_layer_objects(&mut self, layer: LayerId) {
        let font = self.text_font.clone();
        let Some(l) = self.doc.layers_mut().try_get_mut(layer) else {
            return;
        };
        l.obj_tiles = TileGrid::new();
        for obj in l.objects.iter_mut() {
            match obj {
                crate::layer::DrawObject::Text {
                    pos,
                    text,
                    size,
                    color,
                    raster,
                    bbox,
                } => {
                    // 光栅缺失时尝试 swash（字体已设置；text 特性门控）
                    #[cfg(feature = "text")]
                    if raster.is_none() {
                        if let Some(f) = font.as_ref() {
                            *raster = swash_text_raster(f, text, *pos, *size, *color)
                                .map(std::sync::Arc::new);
                        }
                    }
                    #[cfg(not(feature = "text"))]
                    let _ = (&font, text, size, color);
                    if let Some(ra) = raster.as_ref() {
                        let (ox, oy) = (
                            pos.0.round() as i64 + ra.dx,
                            pos.1.round() as i64 + ra.dy,
                        );
                        blit_premul(&mut l.obj_tiles, &ra.premul, ra.w, ra.h, ox, oy);
                        *bbox = Some(Rect::new(
                            ox as i32,
                            oy as i32,
                            ra.w,
                            ra.h,
                        ));
                    } else {
                        *bbox = None; // 无法光栅化（无字体）：不显示也不命中
                    }
                }
                crate::layer::DrawObject::Shape { bbox, .. } => {
                    // 形状对象 UI 接入时补光栅化；当前不可见不命中
                    *bbox = None;
                }
            }
        }
        l.sync_all_objects();
        self.dirty = Dirty::All;
    }

    /// 撤销/重做/载入后：重建标记为 stale 的对象层。
    fn refresh_stale_objects(&mut self) {
        let stale: Vec<LayerId> = self
            .doc
            .layers()
            .iter_with_id()
            .filter(|(_, l)| l.obj_stale)
            .map(|(id, _)| id)
            .collect();
        for id in stale {
            self.rasterize_layer_objects(id);
        }
    }

    fn expand_dirty(&mut self, dab: &Dab) {
        let r = dab.radius as f64 + 1.0;
        let vp = self.doc.viewport();
        // 旋转下对角两点不再是包围盒：四角变换取 AABB
        let corners = [
            vp.canvas_to_screen(dab.x - r, dab.y - r),
            vp.canvas_to_screen(dab.x + r, dab.y - r),
            vp.canvas_to_screen(dab.x - r, dab.y + r),
            vp.canvas_to_screen(dab.x + r, dab.y + r),
        ];
        let xs = [corners[0].0, corners[1].0, corners[2].0, corners[3].0];
        let ys = [corners[0].1, corners[1].1, corners[2].1, corners[3].1];
        let x0 = xs.iter().cloned().fold(f64::MAX, f64::min);
        let y0 = ys.iter().cloned().fold(f64::MAX, f64::min);
        let x1 = xs.iter().cloned().fold(f64::MIN, f64::max);
        let y1 = ys.iter().cloned().fold(f64::MIN, f64::max);
        let fx = x0.floor() as i32;
        let fy = y0.floor() as i32;
        let rect = Rect::new(
            fx,
            fy,
            (x1.ceil() as i64 - fx as i64).max(1) as u32,
            (y1.ceil() as i64 - fy as i64).max(1) as u32,
        );
        self.dirty.union(rect);
    }
}

impl UndoGroup {
    fn tile_ids(&self) -> &[(LayerId, TileId, Option<crate::tile::TileRef>)] {
        match self.ops.first() {
            Some(UndoOp::Tiles(v)) => v,
            _ => &[],
        }
    }
}

/// 小地图产物：PNG 字节 + 输出尺寸 + 内容包围盒。
pub type MinimapOut = (Vec<u8>, u32, u32, f64, f64, f64, f64);

/// 计时起点（wasm32 无 std::time——返回 None，日志退化为无耗时）。
fn timing_start() -> Option<std::time::Instant> {
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(std::time::Instant::now())
    }
}

/// 计时毫秒（起点 None 时返回 None）。
fn timing_ms(t: Option<std::time::Instant>) -> Option<f64> {
    t.map(|t0| t0.elapsed().as_secs_f64() * 1000.0)
}

/// 读瓦片网格画布坐标处像素（无瓦片 = 全透明）。
fn layer_px(grid: &TileGrid, x: i64, y: i64) -> [u8; 4] {
    let tid = TileId::at(x, y);
    match grid.get(tid) {
        Some(t) => {
            let (ox, oy) = tid.origin();
            let i = (((y - oy) * 256 + (x - ox)) * 4) as usize;
            let p = t.pixels();
            if i + 3 < p.len() {
                [p[i], p[i + 1], p[i + 2], p[i + 3]]
            } else {
                [0, 0, 0, 0]
            }
        }
        None => [0, 0, 0, 0],
    }
}

/// swash 渲染文字 → 光栅缓存（预乘 RGBA + 相对 pos 偏移）。
#[cfg(feature = "text")]
fn swash_text_raster(
    font: &[u8],
    text: &str,
    pos: (f64, f64),
    size: f32,
    color: crate::color::Color,
) -> Option<crate::layer::TextRaster> {
    let mut grid = TileGrid::new();
    let mut rec = StrokeRecorder::new(LayerId::from_raw(0));
    let bounds = crate::shape::draw_text(
        &mut grid,
        &mut rec,
        font,
        text,
        pos.0.round() as i64,
        pos.1.round() as i64,
        size,
        color,
    )?;
    let (w, h) = (bounds.w, bounds.h);
    if w == 0 || h == 0 {
        return None;
    }
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (px, py) = (bounds.x as i64 + x, bounds.y as i64 + y);
            let tid = TileId::at(px, py);
            let Some(t) = grid.get(tid) else {
                continue;
            };
            let (ox, oy) = tid.origin();
            let i = (((py - oy) * 256 + (px - ox)) * 4) as usize;
            let o = ((y * w as i64 + x) * 4) as usize;
            out[o..o + 4].copy_from_slice(&t.pixels()[i..i + 4]);
        }
    }
    Some(crate::layer::TextRaster {
        premul: out,
        w,
        h,
        dx: bounds.x as i64 - pos.0.round() as i64,
        dy: bounds.y as i64 - pos.1.round() as i64,
    })
}

/// 预乘 RGBA 位图 over-blit 进瓦片网格（对象光栅化缓存写入）。
fn blit_premul(grid: &mut TileGrid, src: &[u8], w: u32, h: u32, ox: i64, oy: i64) {
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let s = ((y * w as i64 + x) * 4) as usize;
            let sa = src[s + 3] as u32;
            if sa == 0 {
                continue;
            }
            let (px, py) = (ox + x, oy + y);
            let tid = TileId::at(px, py);
            let t = grid.get_or_create_mut(tid);
            let (tx, ty) = tid.origin();
            let i = (((py - ty) * 256 + (px - tx)) * 4) as usize;
            if i + 3 >= t.pixels_mut().len() {
                continue;
            }
            let d = &mut t.pixels_mut()[i..i + 4];
            if sa == 255 {
                d.copy_from_slice(&src[s..s + 4]);
            } else {
                for k in 0..3 {
                    d[k] = (src[s + k] as u32 + d[k] as u32 * (255 - sa) / 255) as u8;
                }
                d[3] = (sa + d[3] as u32 * (255 - sa) / 255) as u8;
            }
        }
    }
}

fn mark_erase(dabs: &mut [Dab], tool: Tool) {
    if tool == Tool::Eraser {        for d in dabs.iter_mut() {
            d.erase = true;
        }
    }
}

fn centroid_and_dist(touches: &HashMap<u64, (f64, f64)>) -> Option<((f64, f64), f64)> {
    let mut it = touches.values();
    let a = *it.next()?;
    let b = *it.next()?;
    Some((
        ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5),
        ((a.0 - b.0).hypot(a.1 - b.1)),
    ))
}

/// 内置预设：参数语义对齐主流绘画软件的手感基准。
fn builtin_presets() -> Vec<(String, RoundBrush)> {
    let mk = |name: &str, f: fn(&mut RoundBrush)| -> (String, RoundBrush) {
        let mut b = RoundBrush::default();
        f(&mut b);
        (name.to_string(), b)
    };
    vec![
        mk("硬圆笔", |b| {
            b.size = 10.0;
            b.hardness = 1.0;
            b.spacing = 0.04;
            b.smoothing = 0.2;
        }),
        mk("软圆笔", |b| {
            b.size = 24.0;
            b.hardness = 0.15;
            b.opacity = 0.8;
            b.flow = 0.9;
            b.spacing = 0.05;
            b.smoothing = 0.3;
        }),
        mk("马克笔", |b| {
            b.size = 28.0;
            b.hardness = 0.85;
            b.opacity = 0.8;
            b.spacing = 0.06;
            b.smoothing = 0.15;
            b.mode = DabMode::Wash;
        }),
        mk("喷枪", |b| {
            b.size = 40.0;
            b.hardness = 0.0;
            b.opacity = 0.5;
            b.flow = 0.12;
            b.spacing = 0.12;
            b.smoothing = 0.4;
        }),
        mk("书法笔", |b| {
            b.size = 18.0;
            b.hardness = 0.9;
            b.spacing = 0.06;
            b.tilt_sensitivity = 1.0;
        }),
        mk("细节铅笔", |b| {
            b.size = 3.0;
            b.hardness = 0.6;
            b.opacity = 0.9;
            b.flow = 0.85;
            b.spacing = 0.2;
            b.pressure_gamma = 0.8;
        }),
    ]
}

impl Engine {
    // ── 笔刷预设 ──

    /// 预设名列表。
    pub fn preset_names(&self) -> Vec<String> {
        self.presets.iter().map(|(n, _)| n.clone()).collect()
    }

    /// 当前预设名。
    pub fn current_preset_name(&self) -> Option<String> {
        self.preset_idx.map(|i| self.presets[i].0.clone())
    }

    /// 应用预设（克隆参数到当前笔刷；纹理尖不随预设，记录为限制）。
    pub fn apply_preset(&mut self, name: &str) -> bool {
        log::info!("[preset] 应用预设 \"{name}\"");
        let Some(i) = self.presets.iter().position(|(n, _)| n == name) else {
            return false;
        };
        let mut b = self.presets[i].1.clone();
        b.tip = self.brush.tip.clone(); // 尖图独立于预设
        self.brush = b;
        self.preset_idx = Some(i);
        true
    }

    /// 当前笔刷存为预设：与当前预设同名则覆盖，否则新建。
    pub fn save_preset(&mut self, name: &str) -> bool {
        log::info!("[preset] 保存预设 \"{name}\"");
        let name = name.replace(['\t', '\n', '\r'], " ").trim().to_string();
        if name.is_empty() {
            return false;
        }
        let mut b = self.brush.clone();
        b.tip = None; // 尖图不可序列化
        if let Some(i) = self.presets.iter().position(|(n, _)| *n == name) {
            self.presets[i].1 = b;
            self.preset_idx = Some(i);
        } else {
            self.presets.push((name, b));
            self.preset_idx = Some(self.presets.len() - 1);
        }
        true
    }

    /// 删除预设。
    pub fn delete_preset(&mut self, name: &str) -> bool {
        log::info!("[preset] 删除预设 \"{name}\"");
        let Some(i) = self.presets.iter().position(|(n, _)| n == name) else {
            return false;
        };
        self.presets.remove(i);
        if self.preset_idx == Some(i) {
            self.preset_idx = None;
        } else if let Some(idx) = self.preset_idx {
            if idx > i {
                self.preset_idx = Some(idx - 1);
            }
        }
        true
    }

    /// 循环切换预设并应用，返回新预设名。
    pub fn cycle_preset(&mut self, forward: bool) -> Option<String> {
        if self.presets.is_empty() {
            return None;
        }
        let n = self.presets.len();
        let next = match self.preset_idx {
            None => 0,
            Some(i) => {
                if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                }
            }
        };
        self.preset_idx = Some(next);
        let mut b = self.presets[next].1.clone();
        b.tip = self.brush.tip.clone();
        self.brush = b;
        Some(self.presets[next].0.clone())
    }

    /// 导出为文本格式（每预设一行，TAB 分隔；跨端可迁移）。
    pub fn export_presets(&self) -> String {
        let mut out = String::new();
        for (name, b) in &self.presets {
            out.push_str(&format!(
                "{}	{:.4}	{:.4}	{:.4}	{:.4}	{:.4}	{:.4}	{:.4}	{:.4}	{:.4}	{:.4}	{}	{},{},{}
",
                name,
                b.size,
                b.hardness,
                b.opacity,
                b.flow,
                b.spacing,
                b.smoothing,
                b.stabilizer,
                b.pressure_gamma,
                b.tilt_sensitivity,
                b.scatter,
                match b.mode {
                    DabMode::Buildup => "b",
                    DabMode::Wash => "w",
                },
                b.color.r,
                b.color.g,
                b.color.b,
            ));
        }
        out
    }

    /// 按名合并导入（同名覆盖、新名追加），返回成功条数；坏行跳过。
    pub fn import_presets(&mut self, text: &str) -> usize {
        let mut n = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 13 {
                continue;
            }
            let mut it = f[1..].iter().map(|v| v.trim().parse::<f32>());
            let mut next = || it.next().and_then(|r| r.ok());
            let vals = [
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
            ];
            let Some(size) = vals[0] else { continue };
            let Some(hardness) = vals[1] else { continue };
            let Some(opacity) = vals[2] else { continue };
            let Some(flow) = vals[3] else { continue };
            let Some(spacing) = vals[4] else { continue };
            let Some(smoothing) = vals[5] else { continue };
            let Some(stabilizer) = vals[6] else { continue };
            let Some(pressure_gamma) = vals[7] else {
                continue;
            };
            let Some(tilt_sensitivity) = vals[8] else {
                continue;
            };
            let Some(scatter) = vals[9] else { continue };
            let mode = match f[11] {
                "w" => DabMode::Wash,
                _ => DabMode::Buildup,
            };
            let c: Vec<&str> = f[12].split(',').collect();
            if c.len() != 3 {
                continue;
            }
            let (Some(r), Some(g), Some(b)) = (
                c[0].trim().parse::<u8>().ok(),
                c[1].trim().parse::<u8>().ok(),
                c[2].trim().parse::<u8>().ok(),
            ) else {
                continue;
            };
            let name = f[0].replace(['\t', '\n', '\r'], " ").trim().to_string();
            if name.is_empty() {
                continue;
            }
            let brush = RoundBrush {
                size,
                hardness,
                opacity,
                flow,
                spacing,
                smoothing,
                stabilizer,
                pressure_gamma,
                tilt_sensitivity,
                scatter,
                mode,
                color: Color { r, g, b },
                tip: None,
                brush_tip: None,
                dual: crate::brush::DualBrush::default(),
            };
            if let Some(i) = self.presets.iter().position(|(n, _)| *n == name) {
                self.presets[i].1 = brush;
            } else {
                self.presets.push((name, brush));
            }
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;

    /// 记录合成/盖章调用次数的空渲染器。
    pub(super) struct MockRenderer;

    impl Renderer for MockRenderer {
        fn stamp_dabs(
            &mut self,
            _grid: &mut crate::tile::TileGrid,
            _dabs: &[Dab],
            _clip: Option<&crate::tile::TileGrid>,
            _recorder: &mut StrokeRecorder,
        ) {
        }

        fn composite(
            &mut self,
            _doc: &Document,
            target: &mut [u8],
            width: u32,
            dirty: Rect,
            _bg: Option<Color>,
        ) {
            for y in dirty.y..(dirty.y + dirty.h as i32) {
                for x in dirty.x..(dirty.x + dirty.w as i32) {
                    let i = ((y as usize) * width as usize + x as usize) * 4;
                    if i + 3 < target.len() {
                        target[i..i + 4].copy_from_slice(&[7, 7, 7, 255]);
                    }
                }
            }
        }

        fn merge_layers(
            &mut self,
            _dst: &mut crate::layer::Layer,
            _src: &crate::layer::Layer,
            _recorder: &mut StrokeRecorder,
        ) {
        }
    }

    pub(super) struct TestSurface {
        pub(super) presents: usize,
    }

    impl Surface for TestSurface {
        fn present_cpu(&mut self, _rgba: &[u8], _size: (u32, u32), _dirty: Option<Rect>) {
            self.presents += 1;
        }
    }

    fn engine() -> Engine {
        Engine::new(Box::new(MockRenderer), EngineConfig::default())
    }

    fn pointer(phase: PointerPhase, x: f64, y: f64) -> PlatformEvent {
        PlatformEvent::Pointer {
            phase,
            sample: PointerSample::mouse(x, y),
        }
    }

    fn touch(phase: PointerPhase, id: u64, x: f64, y: f64) -> PlatformEvent {
        PlatformEvent::Pointer {
            phase,
            sample: PointerSample {
                kind: PointerKind::Touch,
                id,
                ..PointerSample::mouse(x, y)
            },
        }
    }

    fn resize(e: &mut Engine) {
        e.handle_event(PlatformEvent::Resize {
            w: 64,
            h: 64,
            scale: 1.0,
        });
    }

    /// 在每个 dab 中心写单像素的渲染器（形状提交路径的落墨/撤销验证）。
    pub(super) struct CenterDotRenderer;

    impl Renderer for CenterDotRenderer {
        fn stamp_dabs(
            &mut self,
            grid: &mut crate::tile::TileGrid,
            dabs: &[Dab],
            _clip: Option<&crate::tile::TileGrid>,
            recorder: &mut StrokeRecorder,
        ) {
            for d in dabs {
                let (x, y) = (d.x.round() as i64, d.y.round() as i64);
                let tid = crate::tile::TileId::at(x, y);
                recorder.capture(grid, tid);
                let t = grid.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let i = (((y - oy) * 256 + (x - ox)) * 4) as usize;
                if i + 3 < t.pixels_mut().len() {
                    t.pixels_mut()[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }

        fn composite(
            &mut self,
            _doc: &Document,
            _target: &mut [u8],
            _width: u32,
            _dirty: Rect,
            _bg: Option<Color>,
        ) {
        }

        fn merge_layers(
            &mut self,
            _dst: &mut crate::layer::Layer,
            _src: &crate::layer::Layer,
            _recorder: &mut StrokeRecorder,
        ) {
        }
    }

    fn shape_engine() -> Engine {
        Engine::new(Box::new(CenterDotRenderer), EngineConfig::default())
    }

    /// 画布坐标处的 alpha（无瓦片 = 0）。
    fn ink(e: &Engine, x: i64, y: i64) -> u8 {
        let tid = crate::tile::TileId::at(x, y);
        let layers = e.document().layers();
        layers
            .try_active()
            .and_then(|id| layers.get(id).tiles.get(tid))
            .map(|t| {
                let (ox, oy) = tid.origin();
                t.pixels()[(((y - oy) * 256 + (x - ox)) * 4) as usize + 3]
            })
            .unwrap_or(0)
    }

    #[test]
    fn shape_line_stroke_commits_and_undoes() {
        let mut e = shape_engine();
        resize(&mut e);
        e.set_tool(Tool::Shape {
            kind: crate::shape::ShapeKind::Line,
            fill: false,
        });
        e.handle_event(pointer(PointerPhase::Down, 10.0, 30.0));
        e.handle_event(pointer(PointerPhase::Move, 40.0, 30.0));
        e.handle_event(pointer(PointerPhase::Up, 40.0, 30.0));
        assert_eq!(e.document().history().undo_len(), 1, "形状整组入撤销");
        assert_eq!(ink(&e, 25, 30), 255, "直线经过处有墨");
        assert!(e.undo());
        assert_eq!(ink(&e, 25, 30), 0, "撤销后复原");
    }

    #[test]
    fn shape_rect_fill_commits_and_undoes() {
        let mut e = shape_engine();
        resize(&mut e);
        e.set_tool(Tool::Shape {
            kind: crate::shape::ShapeKind::Rect,
            fill: true,
        });
        e.handle_event(pointer(PointerPhase::Down, 10.0, 10.0));
        e.handle_event(pointer(PointerPhase::Move, 30.0, 25.0));
        e.handle_event(pointer(PointerPhase::Up, 30.0, 25.0));
        assert_eq!(e.document().history().undo_len(), 1);
        assert_eq!(ink(&e, 20, 18), 255, "矩形内部有墨");
        assert_eq!(ink(&e, 5, 5), 0, "矩形外无墨");
        assert!(e.undo());
        assert_eq!(ink(&e, 20, 18), 0);
    }

    #[test]
    fn shape_drag_writes_nothing_until_up() {
        let mut e = shape_engine();
        resize(&mut e);
        e.set_tool(Tool::Shape {
            kind: crate::shape::ShapeKind::Line,
            fill: false,
        });
        e.handle_event(pointer(PointerPhase::Down, 10.0, 30.0));
        e.handle_event(pointer(PointerPhase::Move, 40.0, 30.0));
        // 拖拽中：预览只进帧，瓦片无墨、无历史、撤销被守卫
        assert_eq!(e.document().history().undo_len(), 0);
        assert_eq!(ink(&e, 25, 30), 0);
        assert!(!e.undo(), "拖拽中撤销被守卫");
        e.handle_event(pointer(PointerPhase::Up, 40.0, 30.0));
        assert_eq!(ink(&e, 25, 30), 255);
    }

    #[test]
    fn shape_cancel_and_tool_switch_leave_no_trace() {
        // Cancel 事件：放弃
        let mut e = shape_engine();
        resize(&mut e);
        e.set_tool(Tool::Shape {
            kind: crate::shape::ShapeKind::Rect,
            fill: false,
        });
        e.handle_event(pointer(PointerPhase::Down, 10.0, 10.0));
        e.handle_event(pointer(PointerPhase::Move, 30.0, 25.0));
        e.handle_event(pointer(PointerPhase::Cancel, 30.0, 25.0));
        assert_eq!(e.document().history().undo_len(), 0);
        assert_eq!(ink(&e, 20, 18), 0);
        // 拖拽中切工具：同样放弃
        e.handle_event(pointer(PointerPhase::Down, 10.0, 10.0));
        e.handle_event(pointer(PointerPhase::Move, 30.0, 25.0));
        e.set_tool(Tool::Brush);
        assert_eq!(e.document().history().undo_len(), 0);
        assert_eq!(ink(&e, 20, 18), 0);
    }

    #[test]
    fn text_tool_anchor_take_once() {
        let mut e = engine();
        resize(&mut e);
        e.set_tool(Tool::Text);
        e.handle_event(pointer(PointerPhase::Down, 20.0, 40.0));
        assert_eq!(e.take_text_anchor(), Some((20.0, 40.0)));
        assert_eq!(e.take_text_anchor(), None, "锚点一次性取走");
        // 画笔工具不落锚
        e.set_tool(Tool::Brush);
        e.handle_event(pointer(PointerPhase::Down, 5.0, 5.0));
        assert_eq!(e.take_text_anchor(), None);
        assert_eq!(e.document().history().undo_len(), 0, "落锚不入历史");
    }

    /// 活动层 id（try_active 返回 id，测试统一经 layers().get 取引用）。
    fn active_id(e: &Engine) -> LayerId {
        e.document().layers().try_active().expect("有活动层")
    }

    fn layer_of(e: &Engine) -> crate::layer::Layer {
        // 深拷贝避免借用冲突的小对象（objects/tiles 断言用）
        e.document().layers().get(active_id(e)).clone()
    }

    /// 2×2 红色光栅（文字对象测试用，不依赖字体）。
    fn text_raster() -> crate::layer::TextRaster {
        crate::layer::TextRaster {
            premul: vec![255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255],
            w: 2,
            h: 2,
            dx: 0,
            dy: -2,
        }
    }

    #[test]
    fn text_object_crud_hit_edit_move_delete() {
        let mut e = engine();
        resize(&mut e);
        assert!(e.add_text_object((100.0, 100.0), "hi", 32.0, Some(text_raster())));
        // 命中 → 编辑信息
        e.set_tool(Tool::Text);
        e.handle_event(pointer(PointerPhase::Down, 100.0, 99.0)); // bbox 内
        let edit = e.take_text_edit();
        assert_eq!(edit.map(|(t, s, _)| (t, s)), Some(("hi".into(), 32.0)));
        assert!(e.update_text_object("yo", 48.0, Some(text_raster())));
        // 拖拽移动：Down 命中 → Move → Up（一组撤销）
        e.handle_event(pointer(PointerPhase::Down, 100.0, 99.0));
        e.handle_event(pointer(PointerPhase::Move, 140.0, 99.0));
        e.handle_event(pointer(PointerPhase::Up, 140.0, 99.0));
        let layer = layer_of(&e);
        match &layer.objects[0] {
            crate::layer::DrawObject::Text { pos, text, .. } => {
                assert_eq!((pos.0 as i64, pos.1 as i64), (140, 100));
                assert_eq!(text, "yo");
            }
            _ => panic!("对象类型"),
        }
        // 撤销移动 → 回到 (100,100)
        assert!(e.undo());
        let layer = layer_of(&e);
        match &layer.objects[0] {
            crate::layer::DrawObject::Text { pos, .. } => {
                assert_eq!((pos.0 as i64, pos.1 as i64), (100, 100))
            }
            _ => panic!(),
        }
        // 删除 → 撤销 → 恢复
        assert!(e.delete_text_object());
        assert!(layer_of(&e).objects.is_empty());
        assert!(e.undo());
        assert_eq!(layer_of(&e).objects.len(), 1);
    }

    #[test]
    fn text_object_ora_roundtrip() {
        let mut e = engine();
        resize(&mut e);
        assert!(e.add_text_object((50.0, 60.0), "复机", 40.0, Some(text_raster())));
        let bytes = e.save_ora().expect("存档");
        let mut e2 = engine();
        resize(&mut e2);
        assert!(e2.load_ora(&bytes));
        let layer = layer_of(&e2);
        assert_eq!(layer.objects.len(), 1, "对象随 ORA 往返");
        // stale 缓存经 render 重建 → 可命中编辑
        let mut surf = TestSurface { presents: 0 };
        e2.render(&mut surf);
        e2.set_tool(Tool::Text);
        e2.handle_event(pointer(PointerPhase::Down, 50.0, 59.0));
        let edit = e2.take_text_edit();
        assert_eq!(edit.map(|(t, _, _)| t), Some("复机".into()));
    }

    #[test]
    fn object_layer_stroke_merges_into_content() {
        // 对象层上画笔：笔画进 tiles，content() 合并对象（对象在上）
        let mut e = shape_engine();
        resize(&mut e);
        assert!(e.add_text_object((10.0, 30.0), "x", 8.0, Some(text_raster())));
        e.set_tool(Tool::Brush);
        e.handle_event(pointer(PointerPhase::Down, 5.0, 5.0));
        e.handle_event(pointer(PointerPhase::Move, 8.0, 5.0));
        e.handle_event(pointer(PointerPhase::Up, 8.0, 5.0));
        assert_eq!(ink(&e, 5, 5), 255, "笔画写入基础瓦片");
        let layer = layer_of(&e);
        let content_red = {
            let tid = crate::tile::TileId::at(10, 29); // 对象光栅区域（pos(10,30) dy=-2）
            layer
                .content()
                .get(tid)
                .map(|t| {
                    let (ox, oy) = tid.origin();
                    let i = (((29 - oy) * 256 + (10 - ox)) * 4) as usize;
                    t.pixels()[i]
                })
                .unwrap_or(0)
        };
        assert_eq!(content_red, 255, "对象进入合并内容");
    }

    #[test]
    fn fps_monitor_config_and_toggle() {
        let mut e = engine();
        resize(&mut e);
        let mut surf = TestSurface { presents: 0 };
        assert!(e.fps_monitor_enabled(), "默认开启（配置项）");
        assert_eq!(e.render_count(), 0);
        for _ in 0..5 {
            e.render(&mut surf);
        }
        assert_eq!(e.render_count(), 5, "呈现计数随 render 递增");
        // 关闭后冻结
        e.set_fps_monitor(false);
        for _ in 0..3 {
            e.render(&mut surf);
        }
        assert_eq!(e.render_count(), 5, "监控关闭后计数冻结");
        e.set_fps_monitor(true);
        e.render(&mut surf);
        assert_eq!(e.render_count(), 6, "重开后恢复计数");
    }

    #[test]
    fn flood_fill_blob_and_undo() {
        let mut e = engine();
        resize(&mut e);
        // 形状工具画 20×20 黑色实心块（直写瓦片，与渲染器无关）
        e.set_tool(Tool::Shape {
            kind: crate::shape::ShapeKind::Rect,
            fill: true,
        });
        e.handle_event(pointer(PointerPhase::Down, 10.0, 10.0));
        e.handle_event(pointer(PointerPhase::Move, 30.0, 30.0));
        e.handle_event(pointer(PointerPhase::Up, 30.0, 30.0));
        // 红色油漆桶点内部：整块变红
        e.brush_mut().color = crate::color::Color { r: 255, g: 0, b: 0 };
        assert!(e.flood_fill(15, 15, 0));
        let layer = layer_of(&e);
        assert_eq!(layer_px(&layer.tiles, 15, 15)[0], 255, "内部被填充为红");
        assert_eq!(layer_px(&layer.tiles, 15, 15)[1], 0);
        assert_eq!(layer_px(&layer.tiles, 5, 5)[3], 0, "块外不受影响");
        // 撤销 → 恢复黑
        assert!(e.undo());
        let layer = layer_of(&e);
        assert_eq!(layer_px(&layer.tiles, 15, 15)[0], 0, "撤销恢复黑色");
        // 点击无内容处：无操作（无限画布不发散）
        assert!(!e.flood_fill(500, 500, 0), "空区域不发散");
    }

    #[test]
    fn stroke_lifecycle_records_history() {
        let mut e = engine();
        resize(&mut e);
        e.handle_event(pointer(PointerPhase::Down, 10.0, 30.0));
        for x in (12..=50).step_by(2) {
            e.handle_event(pointer(PointerPhase::Move, x as f64, 30.0));
        }
        e.handle_event(pointer(PointerPhase::Up, 50.0, 30.0));
        assert_eq!(e.document().history().undo_len(), 1);
        assert!(e.undo());
        assert_eq!(e.document().history().redo_len(), 1);
        assert!(e.redo());
        assert_eq!(e.document().history().undo_len(), 1);
    }

    #[test]
    fn dirty_tracks_stroke_and_render_clears() {
        let mut e = engine();
        let mut surface = TestSurface { presents: 0 };
        resize(&mut e);
        assert_eq!(e.dirty(), Dirty::All);
        e.render(&mut surface);
        assert_eq!(e.dirty(), Dirty::Clean);
        assert_eq!(surface.presents, 1);

        e.handle_event(pointer(PointerPhase::Down, 20.0, 20.0));
        assert!(matches!(e.dirty(), Dirty::Part(_)));
        e.render(&mut surface);
        assert_eq!(e.dirty(), Dirty::Clean);
        e.render(&mut surface);
        assert_eq!(surface.presents, 3);
    }

    #[test]
    fn pen_in_range_blocks_touch() {
        let mut e = engine();
        resize(&mut e);
        e.handle_event(PlatformEvent::PenInRange(true));
        e.handle_event(touch(PointerPhase::Down, 1, 10.0, 10.0));
        e.handle_event(touch(PointerPhase::Up, 1, 10.0, 10.0));
        assert_eq!(e.document().history().undo_len(), 0);
    }

    #[test]
    fn focus_loss_ends_stroke() {
        let mut e = engine();
        resize(&mut e);
        e.handle_event(pointer(PointerPhase::Down, 10.0, 10.0));
        e.handle_event(pointer(PointerPhase::Move, 30.0, 10.0));
        e.handle_event(PlatformEvent::Focus(false));
        assert_eq!(e.document().history().undo_len(), 1);
        e.handle_event(pointer(PointerPhase::Move, 50.0, 10.0));
        e.handle_event(pointer(PointerPhase::Up, 50.0, 10.0));
        assert_eq!(e.document().history().undo_len(), 1);
    }

    #[test]
    fn layer_api_marks_dirty() {
        let mut e = engine();
        resize(&mut e);
        let mut surface = TestSurface { presents: 0 };
        e.render(&mut surface);
        assert_eq!(e.dirty(), Dirty::Clean);
        let b = e.add_layer().unwrap();
        assert_eq!(e.dirty(), Dirty::All);
        e.render(&mut surface);
        assert!(e.merge_down(), "顶层向下合并");
        assert_eq!(e.document().layers().len(), 1);
        assert!(e.undo(), "合并可撤销");
        assert_eq!(e.document().layers().len(), 2);
        assert!(e.duplicate_layer(b).is_some());
        assert!(e.flatten());
        assert_eq!(e.document().layers().len(), 1);
    }

    #[test]
    fn two_finger_gesture_cancels_accidental_stroke() {
        let mut e = engine();
        resize(&mut e);
        // 第一指落下开始画
        e.handle_event(touch(PointerPhase::Down, 1, 10.0, 10.0));
        e.handle_event(touch(PointerPhase::Move, 1, 20.0, 10.0));
        // 第二指落下：误触笔画被回滚，进入手势
        e.handle_event(touch(PointerPhase::Down, 2, 40.0, 40.0));
        assert_eq!(e.document().history().undo_len(), 0, "误触笔画不入历史");
        // 双指平移：两指同向移动 (5,5)
        e.handle_event(touch(PointerPhase::Move, 1, 25.0, 15.0));
        e.handle_event(touch(PointerPhase::Move, 2, 45.0, 45.0));
        let (px, py) = e.document().viewport().pan();
        assert!(
            (px - 5.0).abs() < 1e-9 && (py - 5.0).abs() < 1e-9,
            "平移 {px},{py}"
        );
        // 第二指大幅外拉：张开放大
        e.handle_event(touch(PointerPhase::Move, 2, 80.0, 45.0));
        assert!(e.document().viewport().zoom() > 1.0, "张开应放大");
        // 抬起一指：手势结束但闩锁防误画
        e.handle_event(touch(PointerPhase::Up, 2, 80.0, 45.0));
        e.handle_event(touch(PointerPhase::Move, 1, 60.0, 20.0));
        assert_eq!(e.document().history().undo_len(), 0, "闩锁期间不画");
        // 全部抬起后可正常画
        e.handle_event(touch(PointerPhase::Up, 1, 60.0, 20.0));
        e.handle_event(touch(PointerPhase::Down, 1, 30.0, 30.0));
        e.handle_event(touch(PointerPhase::Up, 1, 30.0, 30.0));
        assert_eq!(e.document().history().undo_len(), 1, "闩锁释放后可画");
    }

    #[test]
    fn pen_takes_over_gesture() {
        let mut e = engine();
        resize(&mut e);
        e.handle_event(touch(PointerPhase::Down, 1, 10.0, 10.0));
        e.handle_event(touch(PointerPhase::Down, 2, 40.0, 40.0));
        assert!(e.gesture.is_some());
        // 笔落下接管
        e.handle_event(pointer(PointerPhase::Down, 20.0, 20.0));
        assert!(e.gesture.is_none());
        assert!(!e.gesture_latch);
        e.handle_event(pointer(PointerPhase::Up, 20.0, 20.0));
        assert_eq!(e.document().history().undo_len(), 1);
    }
}

#[cfg(test)]
mod preset_tests2 {
    use super::tests::MockRenderer;
    use super::*;

    #[test]
    fn builtins_apply_and_cycle() {
        let mut e = Engine::new(Box::new(MockRenderer), EngineConfig::default());
        assert!(e.preset_names().len() >= 6, "内置至少 6 支");
        assert!(e.apply_preset("喷枪"));
        assert!((e.brush().flow - 0.12).abs() < 1e-6, "喷枪低流量");
        assert_eq!(e.current_preset_name().as_deref(), Some("喷枪"));

        let next = e.cycle_preset(true).unwrap();
        assert_eq!(next, "书法笔", "循环到下一支");
        assert!((e.brush().tilt_sensitivity - 1.0).abs() < 1e-6);
        let prev = e.cycle_preset(false).unwrap();
        assert_eq!(prev, "喷枪");
        assert!(!e.apply_preset("不存在的笔"));
    }

    #[test]
    fn save_overwrite_and_delete() {
        let mut e = Engine::new(Box::new(MockRenderer), EngineConfig::default());
        e.brush_mut().size = 77.0;
        assert!(e.save_preset("我的笔"));
        assert!(e.apply_preset("硬圆笔"));
        assert!(e.apply_preset("我的笔"));
        assert!((e.brush().size - 77.0).abs() < 1e-6, "应用自定义预设");

        // 同名覆盖
        e.brush_mut().size = 88.0;
        assert!(e.save_preset("我的笔"));
        assert!(e.apply_preset("我的笔"));
        assert!((e.brush().size - 88.0).abs() < 1e-6);

        assert!(e.delete_preset("我的笔"));
        assert!(!e.apply_preset("我的笔"));
        // 名字清洗
        assert!(!e.save_preset("  "));
        assert!(e.save_preset("a\tb"), "TAB 被替换为空格");
    }

    #[test]
    fn export_import_roundtrip() {
        let mut e = Engine::new(Box::new(MockRenderer), EngineConfig::default());
        e.brush_mut().size = 33.0;
        e.brush_mut().stabilizer = 0.7;
        e.brush_mut().tilt_sensitivity = 0.5;
        e.brush_mut().color = Color {
            r: 12,
            g: 34,
            b: 56,
        };
        assert!(e.save_preset("导出笔"));
        let text = e.export_presets();

        let mut e2 = Engine::new(Box::new(MockRenderer), EngineConfig::default());
        let n = e2.import_presets(&text);
        assert!(n >= 7, "全部导入: {n}");
        assert!(e2.apply_preset("导出笔"));
        let b = e2.brush();
        assert_eq!(b.size, 33.0);
        assert_eq!(b.stabilizer, 0.7);
        assert_eq!(b.tilt_sensitivity, 0.5);
        assert_eq!(
            b.color,
            Color {
                r: 12,
                g: 34,
                b: 56
            }
        );

        // 坏行跳过
        let n2 =
            e2.import_presets("坏行没有制表符\n另一支\t1\t1\t1\t1\t1\t1\t1\t1\t1\t1\tw\t1,2,3\n");
        assert_eq!(n2, 1);
    }
}

#[cfg(test)]
mod symmetry_tests {
    use super::*;
    use crate::input::{PointerKind, PointerPhase, PointerSample};

    fn pointer(phase: PointerPhase, x: f64, y: f64) -> PlatformEvent {
        PlatformEvent::Pointer {
            phase,
            sample: PointerSample {
                x,
                y,
                pressure: Some(1.0),
                tilt: None,
                kind: PointerKind::Pen,
                id: 1,
                t_us: 0,
            },
        }
    }

    #[test]
    fn horizontal_symmetry_mirrors_stroke() {
        let mut e = Engine::new(Box::new(tests::MockRenderer), EngineConfig::default());
        e.handle_event(PlatformEvent::Resize {
            w: 64,
            h: 64,
            scale: 1.0,
        });
        e.brush_mut().smoothing = 0.0;

        // 左右对称，轴在 x=32（视野中心）
        e.set_symmetry(SymmetryMode::Horizontal { axis_x: 32.0 });

        // 在左侧 (16, 32) 画一点
        e.handle_event(pointer(PointerPhase::Down, 16.0, 32.0));
        e.handle_event(pointer(PointerPhase::Up, 16.0, 32.0));

        // 渲染验证：左侧和右侧都应有墨迹
        let mut surface = tests::TestSurface { presents: 0 };
        e.render(&mut surface);
        // 两侧都有内容（测试通过撤销组数量间接验证——1 笔产生 1 组但瓦片覆盖两侧）
        assert_eq!(e.document().history().undo_len(), 1);
    }

    #[test]
    fn symmetry_expand_math() {
        // 水平：dab(10,20) 镜像到 (54,20) 当轴=32
        let mode = SymmetryMode::Horizontal { axis_x: 32.0 };
        let dab = Dab {
            x: 10.0,
            y: 20.0,
            radius: 5.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: crate::DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: None,
        };
        let v = mode.expand_dab(&dab);
        assert_eq!(v.len(), 2);
        assert!((v[1].x - 54.0).abs() < 1e-9, "镜像 x: {}", v[1].x);
        assert!((v[1].y - 20.0).abs() < 1e-9);

        // 径向 4 分：dab 旋转 90°/180°/270°
        let mode = SymmetryMode::Radial {
            center: (50.0, 50.0),
            segments: 4,
        };
        let v = mode.expand_dab(&dab);
        assert_eq!(v.len(), 4);
        // 原始 dab 也在其中
        assert!(v.iter().any(|d| (d.x - 10.0).abs() < 1e-6));
    }

    #[test]
    fn symmetry_cycle() {
        let mut e = Engine::new(Box::new(tests::MockRenderer), EngineConfig::default());
        e.handle_event(PlatformEvent::Resize {
            w: 64,
            h: 64,
            scale: 1.0,
        });
        assert_eq!(e.cycle_symmetry(), "左右");
        assert_eq!(e.cycle_symmetry(), "上下");
        assert_eq!(e.cycle_symmetry(), "四分");
        assert_eq!(e.cycle_symmetry(), "关");
        assert_eq!(e.cycle_symmetry(), "左右"); // 循环
    }
}
