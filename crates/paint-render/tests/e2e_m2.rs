//! M2 端到端验收：橡皮擦、混合模式、图层合并、PNG 往返、双指手势。

use paint_core::input::{PointerKind, PointerPhase, PointerSample};
use paint_core::render::{EngineConfig, Surface};
use paint_core::{BlendMode, Engine, PlatformEvent, Rect, Tool};
use paint_render::SoftwareRenderer;

struct TestSurface {
    frames: Vec<Vec<u8>>,
}

impl Surface for TestSurface {
    fn present_cpu(&mut self, rgba: &[u8], _size: (u32, u32), _dirty: Option<Rect>) {
        self.frames.push(rgba.to_vec());
    }
}

fn pen(phase: PointerPhase, x: f64, y: f64) -> PlatformEvent {
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

fn touch(phase: PointerPhase, id: u64, x: f64, y: f64) -> PlatformEvent {
    PlatformEvent::Pointer {
        phase,
        sample: PointerSample {
            x,
            y,
            pressure: None,
            tilt: None,
            kind: PointerKind::Touch,
            id,
            t_us: 0,
        },
    }
}

fn engine() -> (Engine, TestSurface) {
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    e.handle_event(PlatformEvent::Resize {
        w: 64,
        h: 64,
        scale: 1.0,
    });
    e.brush_mut().smoothing = 0.0;
    (e, TestSurface { frames: vec![] })
}

fn draw(e: &mut Engine, x0: f64, x1: f64, y: f64) {
    e.handle_event(pen(PointerPhase::Down, x0, y));
    for x in ((x0 as i32 + 2)..=(x1 as i32)).step_by(2) {
        e.handle_event(pen(PointerPhase::Move, x as f64, y));
    }
    e.handle_event(pen(PointerPhase::Up, x1, y));
}

fn frame(e: &mut Engine, s: &mut TestSurface) -> Vec<u8> {
    e.render(s);
    s.frames.last().unwrap().clone()
}

fn ink_count(frame: &[u8]) -> usize {
    frame
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 128 && p[3] == 255)
        .count()
}

#[test]
fn eraser_removes_and_undoes() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 50.0, 32.0);
    let before = ink_count(&frame(&mut e, &mut s));
    assert!(before > 50);

    // 切橡皮擦掉中段
    e.set_tool(Tool::Eraser);
    draw(&mut e, 24.0, 36.0, 32.0);
    e.set_tool(Tool::Brush);
    let after = ink_count(&frame(&mut e, &mut s));
    assert!(after < before - 40, "擦除应显著减墨: {before} → {after}");

    // 撤销两次回到满墨
    assert!(e.undo()); // 撤销橡皮
    let restored = ink_count(&frame(&mut e, &mut s));
    assert!(
        restored >= before - 2,
        "撤销橡皮恢复: {restored} vs {before}"
    );
}

#[test]
fn blend_mode_multiply_visible() {
    let (mut e, mut s) = engine();
    // 底层画黑线
    draw(&mut e, 10.0, 50.0, 32.0);
    // 新图层白色背景色画粗笔（Multiply 下白不改变底色，黑加深）
    e.add_layer();
    let top = e.document().active_layer();
    e.brush_mut().size = 40.0;
    e.brush_mut().color = paint_core::Color::WHITE;
    e.document_mut().layers_mut().get_mut(top).blend_mode = BlendMode::Multiply;
    draw(&mut e, 30.0, 32.0, 32.0);
    let f = frame(&mut e, &mut s);
    // 白色 multiply 白底 → 白底不变（边缘外仍是背景白）
    let bg = |x: usize, y: usize| {
        let i = (y * 64 + x) * 4;
        [f[i], f[i + 1], f[i + 2], f[i + 3]]
    };
    assert_eq!(bg(5, 5), [255, 255, 255, 255]);
    // 原黑线仍黑
    let i = (32 * 64 + 20) * 4;
    assert!(f[i] < 128);
}

#[test]
fn merge_down_flatten_flow() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 30.0, 20.0);
    e.add_layer();
    draw(&mut e, 30.0, 50.0, 40.0);
    assert_eq!(e.document().layers().len(), 2);
    let before = ink_count(&frame(&mut e, &mut s));

    // 合并：顶层并入底层
    assert!(e.merge_down());
    assert_eq!(e.document().layers().len(), 1);
    assert_eq!(ink_count(&frame(&mut e, &mut s)), before, "合并后画面不变");

    // 撤销合并恢复两层
    assert!(e.undo());
    assert_eq!(e.document().layers().len(), 2);
    assert_eq!(ink_count(&frame(&mut e, &mut s)), before);

    // flatten
    assert!(e.flatten());
    assert_eq!(e.document().layers().len(), 1);
    assert_eq!(ink_count(&frame(&mut e, &mut s)), before, "压平后画面不变");
    assert!(e.undo());
    assert_eq!(e.document().layers().len(), 2);
}

#[test]
fn png_export_import_roundtrip() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 50.0, 32.0);
    let frame_before = frame(&mut e, &mut s);

    let png = e
        .export_png(Some(Rect::new(0, 0, 64, 64)), 1.0, false)
        .unwrap();
    assert!(!png.is_empty());

    // 解码回预乘像素对比（白底黑线，无损通道应逐像素一致）
    let (data, w, h) = paint_core::io::decode_png(&png).unwrap();
    assert_eq!((w, h), (64, 64));
    let same = data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(frame_before.as_chunks::<4>().0)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(same, 0, "导出应与屏幕合成一致");

    // 导入为新图层：画面外观不变（同样的内容叠加一层）
    let before_ink = ink_count(&frame(&mut e, &mut s));
    let lid = e.import_png(&png).unwrap();
    assert_eq!(e.document().layers().len(), 2);
    assert_eq!(ink_count(&frame(&mut e, &mut s)), before_ink);
    // 撤销导入：图层被移除
    assert!(e.undo());
    assert_eq!(e.document().layers().len(), 1);
    assert!(!e.document().layers().contains(lid));
}

#[test]
fn gesture_pan_zoom_on_screen() {
    let (mut e, mut s) = engine();
    draw(&mut e, 20.0, 40.0, 32.0);
    let base = ink_count(&frame(&mut e, &mut s));
    assert!(base > 0);

    // 双指同向大幅平移：内容移出画面
    e.handle_event(touch(PointerPhase::Down, 1, 10.0, 50.0));
    e.handle_event(touch(PointerPhase::Down, 2, 30.0, 50.0));
    e.handle_event(touch(PointerPhase::Move, 1, 110.0, 50.0));
    e.handle_event(touch(PointerPhase::Move, 2, 130.0, 50.0));
    let after_pan = ink_count(&frame(&mut e, &mut s));
    assert!(after_pan < base, "平移后墨迹移出: {after_pan} vs {base}");

    e.handle_event(touch(PointerPhase::Up, 1, 110.0, 50.0));
    e.handle_event(touch(PointerPhase::Up, 2, 130.0, 50.0));
    // 无历史条目产生
    assert_eq!(e.document().history().undo_len(), 1, "只有最初的笔画");
}
