use crate::document::Document;
use crate::geometry::Rect;
use crate::history::StrokeRecorder;
use crate::input::{PlatformEvent, PointerKind, PointerPhase, PointerSample};
use crate::layer::LayerId;
use crate::render::{EngineConfig, Renderer, Surface};
use crate::stroke::{Dab, RoundBrush, StrokeGen, StrokeState};

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

struct ActiveStroke {
    state: StrokeState,
    recorder: StrokeRecorder,
    layer: LayerId,
}

/// 引擎：平台壳持有它，喂事件、要帧。
pub struct Engine {
    doc: Document,
    renderer: Box<dyn Renderer>,
    frame: Vec<u8>,
    size: (u32, u32),
    dirty: Dirty,
    brush: RoundBrush,
    stroke: Option<ActiveStroke>,
    pen_in_range: bool,
    vp_rev: u64,
}

impl Engine {
    pub fn new(renderer: Box<dyn Renderer>, config: EngineConfig) -> Self {
        let background = config.background;
        let mut doc = Document::new(config.undo_memory_limit);
        doc.set_background(background);
        Self {
            doc,
            renderer,
            frame: Vec::new(),
            size: (0, 0),
            dirty: Dirty::All,
            brush: config.brush,
            stroke: None,
            pen_in_range: false,
            vp_rev: 0,
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

    pub fn dirty(&self) -> Dirty {
        self.dirty
    }

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
        self.renderer
            .composite(&self.doc, &mut self.frame, w, region);
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

    fn on_pointer(&mut self, phase: PointerPhase, sample: PointerSample) {
        // 手掌拒绝：笔在感应区时忽略触摸
        if self.pen_in_range && sample.kind == PointerKind::Touch {
            return;
        }
        match phase {
            PointerPhase::Down => {
                if self.stroke.is_none() {
                    self.begin_stroke(&sample);
                }
            }
            PointerPhase::Move => self.extend_stroke(&sample),
            PointerPhase::Up | PointerPhase::Cancel => self.end_stroke(),
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
        let dabs = self.brush.begin(&mut state, &cs);
        self.stroke = Some(ActiveStroke {
            state,
            recorder: StrokeRecorder::new(layer),
            layer,
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
        let dabs = self.brush.extend(&mut act.state, &cs);
        if dabs.is_empty() {
            return;
        }
        let layer = act.layer;
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
        // 回收本笔触及且变回全透明的瓦片（防未来橡皮/混合工具泄漏空瓦片）
        if let Some(l) = self.doc.layers_mut().try_get_mut(act.layer) {
            for (_, tid, _) in &group.tiles {
                if let Some(t) = l.tiles.get(*tid) {
                    if t.is_transparent() {
                        l.tiles.remove(*tid);
                    }
                }
            }
        }
        self.doc.commit(group);
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

#[cfg(test)]
mod tests {
    use super::*;

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

        fn composite(&mut self, _doc: &Document, target: &mut [u8], width: u32, dirty: Rect) {
            for y in dirty.y..(dirty.y + dirty.h as i32) {
                for x in dirty.x..(dirty.x + dirty.w as i32) {
                    let i = ((y as usize) * width as usize + x as usize) * 4;
                    if i + 3 < target.len() {
                        target[i..i + 4].copy_from_slice(&[7, 7, 7, 255]);
                    }
                }
            }
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
        assert_eq!(e.dirty(), Dirty::All);
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
        // Clean 时跳过合成、仅重新呈现
        e.render(&mut surface);
        assert_eq!(surface.presents, 3);
    }

    #[test]
    fn pen_in_range_blocks_touch() {
        let mut e = engine();
        resize(&mut e);
        e.handle_event(PlatformEvent::PenInRange(true));
        e.handle_event(PlatformEvent::Pointer {
            phase: PointerPhase::Down,
            sample: PointerSample {
                kind: PointerKind::Touch,
                ..PointerSample::mouse(10.0, 10.0)
            },
        });
        e.handle_event(PlatformEvent::Pointer {
            phase: PointerPhase::Up,
            sample: PointerSample {
                kind: PointerKind::Touch,
                ..PointerSample::mouse(10.0, 10.0)
            },
        });
        // 触摸被忽略：没有形成撤销组
        assert_eq!(e.document().history().undo_len(), 0);
    }

    #[test]
    fn focus_loss_ends_stroke() {
        let mut e = engine();
        resize(&mut e);
        e.handle_event(pointer(PointerPhase::Down, 10.0, 10.0));
        e.handle_event(pointer(PointerPhase::Move, 30.0, 10.0));
        e.handle_event(PlatformEvent::Focus(false));
        // 失焦按抬笔处理：笔画已提交为撤销组
        assert_eq!(e.document().history().undo_len(), 1);
        // 后续 Move 不再累积
        e.handle_event(pointer(PointerPhase::Move, 50.0, 10.0));
        e.handle_event(pointer(PointerPhase::Up, 50.0, 10.0));
        assert_eq!(e.document().history().undo_len(), 1);
    }
}
