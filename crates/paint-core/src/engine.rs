use std::collections::HashMap;

use crate::document::Document;
use crate::geometry::Rect;
use crate::history::{StrokeRecorder, UndoGroup, UndoOp};
use crate::input::{PlatformEvent, PointerKind, PointerPhase, PointerSample};
use crate::layer::{Layer, LayerId};
use crate::render::{EngineConfig, Renderer, Surface};
use crate::stroke::{Dab, RoundBrush, StrokeGen, StrokeState};
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

/// 当前工具。橡皮 = 同一 RoundBrush 引擎、dst-out 合成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
}

struct ActiveStroke {
    state: StrokeState,
    recorder: StrokeRecorder,
    layer: LayerId,
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
    pen_in_range: bool,
    vp_rev: u64,
    // 触摸多指状态
    touches: HashMap<u64, (f64, f64)>,
    gesture: Option<Gesture>,
    /// 手势闩锁：手势触发后，剩余手指抬完前不再起笔画
    gesture_latch: bool,
}

impl Engine {
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
            pen_in_range: false,
            vp_rev: 0,
            touches: HashMap::new(),
            gesture: None,
            gesture_latch: false,
        }
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.doc
    }

    pub fn brush(&self) -> &RoundBrush {
        &self.brush
    }

    pub fn brush_mut(&mut self) -> &mut RoundBrush {
        &mut self.brush
    }

    pub fn tool(&self) -> Tool {
        self.tool
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
    }

    pub fn dirty(&self) -> Dirty {
        self.dirty
    }

    /// 空白区点阵网格开关（无限画布空间指示）。
    pub fn show_grid(&self) -> bool {
        self.doc.show_grid()
    }

    pub fn set_show_grid(&mut self, on: bool) {
        self.doc.set_show_grid(on);
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

    pub fn add_layer(&mut self) -> Option<LayerId> {
        let r = self.doc.add_layer(None);
        if r.is_some() {
            self.dirty = Dirty::All;
        }
        r
    }

    pub fn remove_layer(&mut self, id: LayerId) -> bool {
        let r = self.doc.remove_layer(id);
        if r {
            self.dirty = Dirty::All;
        }
        r
    }

    pub fn duplicate_layer(&mut self, id: LayerId) -> Option<LayerId> {
        let r = self.doc.duplicate_layer(id);
        if r.is_some() {
            self.dirty = Dirty::All;
        }
        r
    }

    pub fn reorder_layer(&mut self, id: LayerId, to: usize) -> bool {
        let r = self.doc.reorder_layer(id, to);
        if r {
            self.dirty = Dirty::All;
        }
        r
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
        let Some(active) = self.doc.layers().try_active() else {
            return false;
        };
        let Some(pos) = self.doc.layers().position(active) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        let below = self.doc.layers().iter_with_id().nth(pos - 1).unwrap().0;
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
        let bounds = bounds.or_else(|| self.visible_content_bounds())?;
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

    /// 保存为 .ora（含合成图与缩略图）。
    pub fn save_ora(&mut self) -> Option<Vec<u8>> {
        let merged = self.export_png(None, 1.0, true)?;
        let b = self.visible_content_bounds()?;
        let scale = (256.0 / b.w as f64).min(256.0 / b.h as f64).min(1.0) as f32;
        let thumb = self.export_png(Some(b), scale, true)?;
        crate::ora::encode_ora(&self.doc, Some(&merged), Some(&thumb)).ok()
    }

    /// 载入 .ora 替换当前文档（历史重置）。
    pub fn load_ora(&mut self, bytes: &[u8]) -> bool {
        let Ok(ora) = crate::ora::decode_ora(bytes) else {
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
            if let Some(b) = l.tiles.content_bounds_precise() {
                acc = Some(match acc {
                    Some(a) => a.union(&b),
                    None => b,
                });
            }
        }
        acc
    }

    // ── 事件与帧 ──

    pub fn handle_event(&mut self, ev: PlatformEvent) {
        match ev {
            PlatformEvent::Resize { w, h, .. } => {
                self.size = (w, h);
                self.frame = vec![0; (w as usize) * (h as usize) * 4];
                self.dirty = Dirty::All;
            }
            PlatformEvent::PenInRange(v) => self.pen_in_range = v,
            PlatformEvent::Focus(f) => {
                if !f {
                    // 失焦：按抬笔处理，保留已画内容与撤销记录
                    self.end_stroke();
                    self.gesture = None;
                    self.gesture_latch = false;
                }
            }
            PlatformEvent::Pointer { phase, sample } => self.on_pointer(phase, sample),
        }
    }

    /// 合成并呈现。Clean 时跳过合成仅重新呈现（窗口恢复等场景）。
    pub fn render(&mut self, surface: &mut dyn Surface) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
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
            Dirty::Clean => unreachable!(),
        };
        let bg = self.doc.background();
        self.renderer
            .composite(&self.doc, &mut self.frame, w, region, Some(bg));
        surface.present_cpu(&self.frame, self.size, Some(region));
        self.dirty = Dirty::Clean;
    }

    pub fn undo(&mut self) -> bool {
        if self.doc.undo() {
            self.dirty = Dirty::All;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.doc.redo() {
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
        // 笔落下即接管：清除进行中的触摸手势
        if phase == PointerPhase::Down && (!self.touches.is_empty() || self.gesture.is_some()) {
            self.cancel_stroke();
            self.touches.clear();
            self.gesture = None;
            self.gesture_latch = false;
        }
        match phase {
            PointerPhase::Down => {
                if self.stroke.is_none() && self.doc.layers().try_active().is_some() {
                    self.begin_stroke(&sample);
                }
            }
            PointerPhase::Move => self.extend_stroke(&sample),
            PointerPhase::Up | PointerPhase::Cancel => self.end_stroke(),
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
                    // 第二指落下：取消误触笔画，进入手势
                    self.cancel_stroke();
                    if let Some((c, d)) = centroid_and_dist(&self.touches) {
                        self.gesture = Some(Gesture {
                            centroid: c,
                            dist: d,
                        });
                        self.gesture_latch = true;
                    }
                } else if !self.gesture_latch
                    && self.stroke.is_none()
                    && self.doc.layers().try_active().is_some()
                {
                    self.begin_stroke(&sample);
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
                }
            }
            PointerPhase::Up | PointerPhase::Cancel => {
                self.touches.remove(&sample.id);
                if self.touches.len() < 2 {
                    self.gesture = None;
                }
                if self.touches.is_empty() {
                    self.gesture_latch = false;
                }
                if self.gesture.is_none() && !self.gesture_latch && self.stroke.is_some() {
                    self.end_stroke();
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

    /// 把 dabs 盖进图层并扩展屏幕脏区。
    fn stamp(&mut self, layer: LayerId, dabs: &[Dab]) {
        let Some(act) = self.stroke.as_mut() else {
            return;
        };
        let l = self.doc.layers_mut().get_mut(layer);
        self.renderer.stamp_dabs(l, dabs, &mut act.recorder);
        for dab in dabs {
            self.expand_dirty(dab);
        }
    }

    fn end_stroke(&mut self) {
        let Some(act) = self.stroke.take() else {
            return;
        };
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

    fn expand_dirty(&mut self, dab: &Dab) {
        let r = dab.radius as f64 + 1.0;
        let vp = self.doc.viewport();
        let (x0, y0) = vp.canvas_to_screen(dab.x - r, dab.y - r);
        let (x1, y1) = vp.canvas_to_screen(dab.x + r, dab.y + r);
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

fn mark_erase(dabs: &mut [Dab], tool: Tool) {
    if tool == Tool::Eraser {
        for d in dabs.iter_mut() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;

    /// 记录合成/盖章调用次数的空渲染器。
    struct MockRenderer;

    impl Renderer for MockRenderer {
        fn stamp_dabs(
            &mut self,
            _layer: &mut crate::layer::Layer,
            _dabs: &[Dab],
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

    struct TestSurface {
        presents: usize,
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
