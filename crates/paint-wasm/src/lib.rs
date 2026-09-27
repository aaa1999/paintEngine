//! paint-wasm：Web 壳。
//!
//! - 呈现：`ImageData` + `putImageData`（帧缓冲不透明，预乘与直行等价）
//! - 输入：Pointer Events；高采样走 `pointerrawupdate`（Chromium），
//!   `pointermove` 兜底（重复 Move 无害：零距离不产生 dab）
//! - 手势：双指平移缩放由引擎状态机处理，壳层只转发原始事件
//! - 重绘：requestAnimationFrame 按需渲染（needs_render 标记）

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, ImageData, PointerEvent, ResizeObserver,
};

use paint_core::input::{PointerKind, PointerPhase, PointerSample};
use paint_core::render::{EngineConfig, Surface};
use paint_core::{BlendMode, Color, Engine, PlatformEvent, Rect, Tool};
use paint_render::SoftwareRenderer;

type PointerClosure = Closure<dyn FnMut(PointerEvent)>;
type IdleClosure = Closure<dyn FnMut()>;
type WheelClosure = Closure<dyn FnMut(web_sys::WheelEvent)>;
type KeyClosure = Closure<dyn FnMut(web_sys::KeyboardEvent)>;

struct Inner {
    engine: Engine,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    needs_render: Cell<bool>,
    last_size: Cell<(u32, u32)>,
    // 壳层平移（空格拖拽 / 中键拖拽）——不动引擎笔画状态
    panning: Cell<bool>,
    pan_last: Cell<(f64, f64)>,
    space_down: Cell<bool>,
}

#[wasm_bindgen]
pub struct PaintApp {
    inner: Rc<RefCell<Inner>>,
    /// 事件闭包必须保活（Drop 即解绑）
    keep: RefCell<Vec<PointerClosure>>,
    wheel_keep: RefCell<Vec<WheelClosure>>,
    key_keep: RefCell<Vec<KeyClosure>>,
    hover_keep: RefCell<Vec<IdleClosure>>,
    raf_keep: RefCell<Option<Rc<RefCell<Option<IdleClosure>>>>>,
    observer: RefCell<Option<ResizeObserver>>,
}

#[wasm_bindgen]
impl PaintApp {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: HtmlCanvasElement) -> Result<PaintApp, JsValue> {
        let ctx: CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .ok_or(JsValue::from_str("无法获取 2d 上下文"))?
            .dyn_into()?;

        let engine = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
        let inner = Rc::new(RefCell::new(Inner {
            engine,
            canvas: canvas.clone(),
            ctx,
            needs_render: Cell::new(true),
            last_size: Cell::new((0, 0)),
            panning: Cell::new(false),
            pan_last: Cell::new((0.0, 0.0)),
            space_down: Cell::new(false),
        }));

        let app = PaintApp {
            inner: inner.clone(),
            keep: RefCell::new(Vec::new()),
            wheel_keep: RefCell::new(Vec::new()),
            key_keep: RefCell::new(Vec::new()),
            hover_keep: RefCell::new(Vec::new()),
            raf_keep: RefCell::new(None),
            observer: RefCell::new(None),
        };
        app.bind_pointer_events(inner.clone())?;
        app.observe_resize(inner.clone());
        app.start_render_loop(inner);
        Ok(app)
    }

    // ── 对 JS 暴露的控制面 ──

    pub fn set_tool(&self, tool: &str) {
        let t = match tool {
            "eraser" => Tool::Eraser,
            "mask" => Tool::Mask,
            _ => Tool::Brush,
        };
        self.inner.borrow_mut().engine.set_tool(t);
    }

    pub fn tool(&self) -> String {
        match self.inner.borrow().engine.tool() {
            Tool::Eraser => "eraser".into(),
            Tool::Mask => "mask".into(),
            Tool::Brush => "brush".into(),
        }
    }

    pub fn set_brush_size(&self, size: f32) {
        self.inner.borrow_mut().engine.brush_mut().size = size.clamp(1.0, 512.0);
    }

    pub fn set_brush_color(&self, r: u8, g: u8, b: u8) {
        self.inner.borrow_mut().engine.brush_mut().color = Color { r, g, b };
    }

    /// 设置纹理笔刷尖（PNG 字节；空数组恢复圆头笔）。
    pub fn set_brush_tip(&self, png: Vec<u8>) -> bool {
        let mut inner = self.inner.borrow_mut();
        let r = if png.is_empty() {
            inner.engine.set_brush_tip(None)
        } else {
            inner.engine.set_brush_tip(Some(&png))
        };
        inner.needs_render.set(true);
        r
    }

    /// 笔倾斜灵敏度 0..1（tilt 笔刷，书法效果）。
    pub fn set_tilt_sensitivity(&self, v: f32) {
        self.inner.borrow_mut().engine.brush_mut().tilt_sensitivity = v.clamp(0.0, 1.0);
    }

    /// 吸管取色（屏幕物理像素）。
    pub fn pick_color(&self, x: u32, y: u32) -> Option<Vec<u8>> {
        self.inner
            .borrow()
            .engine
            .pick_color(x, y)
            .map(|c| vec![c.r, c.g, c.b])
    }

    pub fn set_stabilizer(&self, v: f32) {
        self.inner.borrow_mut().engine.brush_mut().stabilizer = v.clamp(0.0, 0.98);
    }

    pub fn set_brush_opacity(&self, v: f32) {
        let mut inner = self.inner.borrow_mut();
        let v = v.clamp(0.01, 1.0);
        inner.engine.brush_mut().opacity = v;
        inner.engine.brush_mut().flow = v;
    }

    pub fn undo(&self) -> bool {
        self.mark_render(|e| e.undo())
    }

    pub fn redo(&self) -> bool {
        self.mark_render(|e| e.redo())
    }

    pub fn add_layer(&self) -> bool {
        self.mark_render(|e| e.add_layer().is_some())
    }

    pub fn merge_down(&self) -> bool {
        self.mark_render(|e| e.merge_down())
    }

    pub fn flatten(&self) -> bool {
        self.mark_render(|e| e.flatten())
    }

    /// 设置活动图层的混合模式（[`BlendMode::ALL`] 的下标）。
    pub fn set_active_blend_mode(&self, index: usize) {
        if let Some(m) = BlendMode::ALL.get(index) {
            self.mark_render(|e| {
                let active = e.document().active_layer();
                e.document_mut().layers_mut().get_mut(active).blend_mode = *m;
                true
            });
        }
    }

    /// 混合模式名列表，供 JS 构建 UI。
    pub fn blend_mode_names(&self) -> Vec<JsValue> {
        BlendMode::ALL
            .iter()
            .map(|m| JsValue::from_str(m.name()))
            .collect()
    }

    pub fn export_png(&self) -> Vec<u8> {
        self.inner
            .borrow_mut()
            .engine
            .export_png(None, 1.0, true)
            .unwrap_or_default()
    }

    pub fn import_png(&self, data: Vec<u8>) -> bool {
        self.mark_render(|e| e.import_png(&data).is_some())
    }

    /// 保存为 .ora 工程（下载用）。
    pub fn export_ora(&self) -> Vec<u8> {
        self.inner
            .borrow_mut()
            .engine
            .save_ora()
            .unwrap_or_default()
    }

    /// 载入 .ora 工程（替换当前文档）。
    pub fn import_ora(&self, data: Vec<u8>) -> bool {
        self.mark_render(|e| e.load_ora(&data))
    }

    pub fn layer_count(&self) -> usize {
        self.inner.borrow().engine.document().layers().len()
    }

    /// 视野适配到全部内容（无限画布导航：迷路后"回家"）。
    pub fn fit_to_content(&self) {
        self.mark_render(|e| {
            e.fit_to_content(48.0);
            true
        });
    }

    /// 100% 缩放（保持屏幕中心不动）。
    /// 绕屏幕中心旋转视图（弧度）；翻转/复位同族操作。
    pub fn rotate_view(&self, delta: f64) {
        self.mark_render(|e| {
            e.rotate_view(delta);
            true
        });
    }

    /// 内容级变换：Ctrl+T 语义。
    pub fn begin_transform(&self) -> bool {
        self.mark_render(|e| e.begin_transform())
    }
    pub fn transform_translate(&self, dx: f64, dy: f64) {
        self.mark_render(|e| {
            e.transform_translate(dx, dy);
            true
        });
    }
    pub fn transform_rotate(&self, delta: f64) {
        self.mark_render(|e| {
            e.transform_rotate(delta);
            true
        });
    }
    pub fn transform_scale(&self, f: f64) {
        self.mark_render(|e| {
            e.transform_scale(f);
            true
        });
    }
    pub fn commit_transform(&self) -> bool {
        self.mark_render(|e| e.commit_transform())
    }
    pub fn cancel_transform(&self) -> bool {
        self.mark_render(|e| e.cancel_transform())
    }
    pub fn transforming(&self) -> bool {
        self.inner.borrow().engine.transforming()
    }

    /// 剪贴板：内部复制/剪切/粘贴 + 外部图像粘贴 + PNG 导出。
    pub fn copy_selection(&self) -> bool {
        self.inner.borrow_mut().engine.copy_selection()
    }
    pub fn cut_selection(&self) -> bool {
        self.mark_render(|e| e.cut_selection())
    }
    pub fn paste_float(&self) -> bool {
        self.mark_render(|e| e.paste_float())
    }
    pub fn paste_image_float(&self, png: Vec<u8>) -> bool {
        self.mark_render(|e| e.paste_image_float(&png))
    }
    pub fn copy_selection_png(&self) -> Vec<u8> {
        self.inner
            .borrow()
            .engine
            .copy_selection_png()
            .unwrap_or_default()
    }

    /// 笔刷预设。
    pub fn preset_names(&self) -> Vec<String> {
        self.inner.borrow().engine.preset_names()
    }
    pub fn apply_preset(&self, name: String) -> bool {
        self.mark_render(|e| e.apply_preset(&name))
    }
    pub fn save_preset(&self, name: String) -> bool {
        self.inner.borrow_mut().engine.save_preset(&name)
    }
    pub fn delete_preset(&self, name: String) -> bool {
        self.inner.borrow_mut().engine.delete_preset(&name)
    }
    pub fn current_preset_name(&self) -> Option<String> {
        self.inner.borrow().engine.current_preset_name()
    }
    pub fn export_presets(&self) -> String {
        self.inner.borrow().engine.export_presets()
    }
    pub fn import_presets(&self, text: String) -> usize {
        self.inner.borrow_mut().engine.import_presets(&text)
    }

    pub fn toggle_layer_mask(&self) -> bool {
        self.mark_render(|e| e.toggle_layer_mask())
    }

    pub fn toggle_layer_clip(&self) -> bool {
        self.mark_render(|e| e.toggle_layer_clip())
    }

    pub fn flip_view(&self) {
        self.mark_render(|e| {
            e.flip_view();
            true
        });
    }

    pub fn reset_view_transform(&self) {
        self.mark_render(|e| {
            e.document_mut().viewport_mut().reset_transform();
            true
        });
    }

    /// 视口参数（变换拖拽换算用）：[pan_x, pan_y, zoom, rotation, flip]
    pub fn viewport_params(&self) -> Vec<f64> {
        let inner = self.inner.borrow();
        let vp = inner.engine.document().viewport();
        let (px, py) = vp.pan();
        let (zoom, rot, flip) = (vp.zoom(), vp.rotation(), vp.flip_x());
        drop(inner);
        vec![px, py, zoom, rot, if flip { 1.0 } else { 0.0 }]
    }

    pub fn zoom_100(&self) {
        self.mark_render(|e| {
            e.zoom_100();
            true
        });
    }

    pub fn set_show_grid(&self, on: bool) {
        self.mark_render(|e| {
            e.set_show_grid(on);
            true
        });
    }

    // ── 内部：事件绑定与渲染循环 ──

    fn mark_render(&self, f: impl FnOnce(&mut Engine) -> bool) -> bool {
        let mut inner = self.inner.borrow_mut();
        let r = f(&mut inner.engine);
        inner.needs_render.set(true);
        r
    }

    fn bind_pointer_events(&self, inner: Rc<RefCell<Inner>>) -> Result<(), JsValue> {
        let target = inner.borrow().canvas.clone();

        let mk = |phase: PointerPhase| {
            let inner = inner.clone();
            PointerClosure::new(move |e: PointerEvent| {
                e.prevent_default();
                dispatch(&inner, phase, &e);
            })
        };

        let down = mk(PointerPhase::Down);
        let up = mk(PointerPhase::Up);
        let cancel = mk(PointerPhase::Cancel);
        let raw_move = mk(PointerPhase::Move);
        let move_evt = mk(PointerPhase::Move);

        target.add_event_listener_with_callback("pointerdown", down.as_ref().unchecked_ref())?;
        target.add_event_listener_with_callback("pointerup", up.as_ref().unchecked_ref())?;
        target
            .add_event_listener_with_callback("pointercancel", cancel.as_ref().unchecked_ref())?;
        // pointerrawupdate：Chromium 高采样；不支持时 addEventListener 静默忽略
        target.add_event_listener_with_callback(
            "pointerrawupdate",
            raw_move.as_ref().unchecked_ref(),
        )?;
        target
            .add_event_listener_with_callback("pointermove", move_evt.as_ref().unchecked_ref())?;

        // 数位笔悬停 → PenInRange（手掌拒绝）
        {
            let inner_hover = inner.clone();
            let leave = IdleClosure::new(move || {
                let mut i = inner_hover.borrow_mut();
                i.engine.handle_event(PlatformEvent::PenInRange(false));
                i.needs_render.set(true);
            });
            target
                .add_event_listener_with_callback("pointerleave", leave.as_ref().unchecked_ref())?;
            self.hover_keep.borrow_mut().push(leave);
        }

        self.keep
            .borrow_mut()
            .extend([down, up, cancel, raw_move, move_evt]);

        // 滚轮缩放（以光标为锚）
        {
            let inner_wheel = inner.clone();
            let wheel = WheelClosure::new(move |e: web_sys::WheelEvent| {
                e.prevent_default();
                let rect = inner_wheel.borrow().canvas.get_bounding_client_rect();
                let dpr = web_sys::window()
                    .map(|w| w.device_pixel_ratio())
                    .unwrap_or(1.0);
                let pos = (
                    (e.client_x() as f64 - rect.left()) * dpr,
                    (e.client_y() as f64 - rect.top()) * dpr,
                );
                let factor = if e.delta_y() < 0.0 { 1.1 } else { 1.0 / 1.1 };
                let mut i = inner_wheel.borrow_mut();
                i.engine.document_mut().viewport_mut().zoom_at(pos, factor);
                i.needs_render.set(true);
            });
            target.add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref())?;
            self.wheel_keep.borrow_mut().push(wheel);
        }

        // 空格平移修饰键
        if let Some(win) = web_sys::window() {
            let inner_key = inner.clone();
            let keydown = KeyClosure::new(move |e: web_sys::KeyboardEvent| {
                if e.code() == "Space" {
                    inner_key.borrow_mut().space_down.set(true);
                    e.prevent_default();
                }
            });
            let inner_key2 = inner.clone();
            let keyup = KeyClosure::new(move |e: web_sys::KeyboardEvent| {
                if e.code() == "Space" {
                    inner_key2.borrow_mut().space_down.set(false);
                }
            });
            win.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())?;
            win.add_event_listener_with_callback("keyup", keyup.as_ref().unchecked_ref())?;
            self.key_keep.borrow_mut().push(keydown);
            self.key_keep.borrow_mut().push(keyup);
        }

        Ok(())
    }

    fn observe_resize(&self, inner: Rc<RefCell<Inner>>) {
        let inner2 = inner.clone();
        let cb = IdleClosure::new(move || {
            sync_size(&inner2);
        });
        if let Ok(obs) = ResizeObserver::new(cb.as_ref().unchecked_ref()) {
            let canvas = inner.borrow().canvas.clone();
            obs.observe(&canvas);
            *self.observer.borrow_mut() = Some(obs);
        }
        self.hover_keep.borrow_mut().push(cb);
    }

    fn start_render_loop(&self, inner: Rc<RefCell<Inner>>) {
        // 自引用环：闭包经 Rc 槽位调用自身，槽位整体保活。
        // 不能把闭包 take 出来单独存——回调内经槽位取自身会拿到空值 →
        // panic 断链，渲染循环当场死亡。
        let slot: Rc<RefCell<Option<IdleClosure>>> = Rc::new(RefCell::new(None));
        let raf_inner = inner.clone();
        let body_slot = slot.clone();
        *slot.borrow_mut() = Some(IdleClosure::new(move || {
            tick(&raf_inner);
            if let Some(w) = web_sys::window() {
                if let Some(cb) = body_slot.borrow().as_ref() {
                    let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
                }
            }
        }));

        // 兜底：个别 webview 不派发 rAF，16ms 定时器保证渲染持续。
        // 正常浏览器里 tick 因 needs_render 早退，几乎无额外开销。
        let timer_inner = inner.clone();
        let timer = IdleClosure::new(move || tick(&timer_inner));

        if let Some(w) = web_sys::window() {
            if let Some(cb) = slot.borrow().as_ref() {
                let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
            }
            let _ = w.set_interval_with_callback_and_timeout_and_arguments_0(
                timer.as_ref().unchecked_ref(),
                16,
            );
        }
        self.hover_keep.borrow_mut().push(timer);
        *self.raf_keep.borrow_mut() = Some(slot);
    }
}

fn i_am_pan(inner: &Rc<RefCell<Inner>>) -> bool {
    inner.borrow().panning.get()
}

fn dispatch(inner: &Rc<RefCell<Inner>>, phase: PointerPhase, e: &PointerEvent) {
    let dpr = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0);
    let canvas = inner.borrow().canvas.clone();
    let rect = canvas.get_bounding_client_rect();

    let kind = match e.pointer_type().as_str() {
        "pen" => PointerKind::Pen,
        "eraser" => PointerKind::Eraser,
        "touch" => PointerKind::Touch,
        _ => PointerKind::Mouse,
    };

    // 鼠标平移：空格+左键 或 中键拖拽（触摸平移走引擎双指手势）
    if kind == PointerKind::Mouse {
        let pos = (
            (e.client_x() as f64 - rect.left()) * dpr,
            (e.client_y() as f64 - rect.top()) * dpr,
        );
        match phase {
            PointerPhase::Down => {
                let i = inner.borrow();
                if (i.space_down.get() || e.button() == 1) && e.pointer_type().as_str() == "mouse" {
                    drop(i);
                    let i = inner.borrow_mut();
                    i.panning.set(true);
                    i.pan_last.set(pos);
                    i.needs_render.set(true);
                    return;
                }
            }
            PointerPhase::Move => {
                if i_am_pan(inner) {
                    let (lx, ly) = inner.borrow().pan_last.get();
                    let mut i = inner.borrow_mut();
                    i.engine
                        .document_mut()
                        .viewport_mut()
                        .pan_by(pos.0 - lx, pos.1 - ly);
                    i.pan_last.set(pos);
                    i.needs_render.set(true);
                    return;
                }
            }
            PointerPhase::Up | PointerPhase::Cancel => {
                if inner.borrow().panning.get() {
                    inner.borrow_mut().panning.set(false);
                    return;
                }
            }
        }
    }

    // 悬停（无按键）：仅用数位笔悬停驱动手掌拒绝，不产生笔画事件
    if e.buttons() == 0 {
        let mut i = inner.borrow_mut();
        if kind == PointerKind::Pen {
            i.engine.handle_event(PlatformEvent::PenInRange(true));
        }
        i.needs_render.set(true);
        return;
    }

    let pressure = match kind {
        PointerKind::Pen | PointerKind::Eraser => Some(e.pressure().clamp(0.0, 1.0)),
        _ => None,
    };
    let t_us = web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| (p.now() * 1000.0) as u64)
        .unwrap_or(0);

    let sample = PointerSample {
        x: (e.client_x() as f64 - rect.left()) * dpr,
        y: (e.client_y() as f64 - rect.top()) * dpr,
        pressure,
        tilt: Some((
            (e.tilt_x() as f32).to_radians(),
            (e.tilt_y() as f32).to_radians(),
        )),
        kind,
        id: e.pointer_id() as u64,
        t_us,
    };

    let mut i = inner.borrow_mut();
    if kind == PointerKind::Pen {
        i.engine.handle_event(PlatformEvent::PenInRange(true));
    }
    i.engine
        .handle_event(PlatformEvent::Pointer { phase, sample });
    i.needs_render.set(true);
}

/// 单次渲染节拍：尺寸检查 + 按需合成呈现。
fn tick(inner: &Rc<RefCell<Inner>>) {
    sync_size(inner); // 每帧检查：部分 webview 不派发 RO 回调
    let mut borrow = inner.borrow_mut();
    let Inner {
        engine,
        ctx,
        needs_render,
        ..
    } = &mut *borrow;
    if needs_render.get() {
        let mut surface = CanvasSurface { ctx };
        engine.render(&mut surface);
        needs_render.set(false);
    }
}

fn sync_size(inner: &Rc<RefCell<Inner>>) {
    let dpr = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0);
    let w = (inner.borrow().canvas.client_width() as f64 * dpr)
        .round()
        .max(1.0) as u32;
    let h = (inner.borrow().canvas.client_height() as f64 * dpr)
        .round()
        .max(1.0) as u32;
    let mut i = inner.borrow_mut();
    if i.last_size.get() != (w, h) {
        i.last_size.set((w, h));
        i.canvas.set_width(w);
        i.canvas.set_height(h);
        i.engine.handle_event(PlatformEvent::Resize {
            w,
            h,
            scale: dpr as f32,
        });
        i.needs_render.set(true);
    }
}

struct CanvasSurface<'a> {
    ctx: &'a CanvasRenderingContext2d,
}

impl Surface for CanvasSurface<'_> {
    fn present_cpu(&mut self, rgba: &[u8], size: (u32, u32), _dirty: Option<Rect>) {
        if rgba.len() != (size.0 as usize) * (size.1 as usize) * 4 {
            return;
        }
        if let Ok(img) =
            ImageData::new_with_u8_clamped_array_and_sh(wasm_bindgen::Clamped(rgba), size.0, size.1)
        {
            let _ = self.ctx.put_image_data(&img, 0.0, 0.0);
        }
    }
}
