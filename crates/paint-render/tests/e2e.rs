//! M1 端到端验收：事件 → 笔画 → 盖章 → 合成 → 呈现 → 撤销/重做。

use paint_core::input::{PointerKind, PointerPhase, PointerSample};
use paint_core::render::{EngineConfig, Surface};
use paint_core::{Engine, PlatformEvent, Rect};
use paint_render::SoftwareRenderer;

struct TestSurface {
    frames: Vec<Vec<u8>>,
}

impl Surface for TestSurface {
    fn present_cpu(&mut self, rgba: &[u8], _size: (u32, u32), _dirty: Option<Rect>) {
        self.frames.push(rgba.to_vec());
    }
}

fn pen(phase: PointerPhase, x: f64, y: f64, pressure: f32) -> PlatformEvent {
    PlatformEvent::Pointer {
        phase,
        sample: PointerSample {
            x,
            y,
            pressure: Some(pressure),
            tilt: None,
            kind: PointerKind::Pen,
            t_us: 0,
        },
    }
}

fn draw_line(e: &mut Engine, y: f64, pressure: f32) {
    e.handle_event(pen(PointerPhase::Down, 10.0, y, pressure));
    for x in 12..=54i32 {
        e.handle_event(pen(PointerPhase::Move, x as f64, y, pressure));
    }
    e.handle_event(pen(PointerPhase::Up, 54.0, y, pressure));
}

/// 统计一行 y 上非背景像素的跨度（笔迹宽度）。
fn ink_span(frame: &[u8], w: u32, y: u32) -> (u32, u32) {
    let mut first = u32::MAX;
    let mut last = 0;
    for x in 0..w {
        let i = ((y * w + x) * 4) as usize;
        if frame[i] < 128 {
            first = first.min(x);
            last = last.max(x);
        }
    }
    (first, last)
}

fn ink_count(frame: &[u8]) -> usize {
    frame
        .chunks_exact(4)
        .filter(|p| p[0] < 128 && p[3] == 255)
        .count()
}

#[test]
fn draw_undo_redo_full_cycle() {
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    e.handle_event(PlatformEvent::Resize {
        w: 64,
        h: 64,
        scale: 1.0,
    });
    let mut surface = TestSurface { frames: vec![] };

    draw_line(&mut e, 20.0, 1.0);
    e.render(&mut surface);
    let frame = surface.frames.last().unwrap().clone();
    let ink = ink_count(&frame);
    assert!(ink > 20, "应有可见墨迹: {ink}");
    let (a, b) = ink_span(&frame, 64, 20);
    assert!(a <= 10 && b >= 54, "墨迹应覆盖 10..54: {a}..{b}");

    // 撤销后画面回到纯背景
    assert!(e.undo());
    e.render(&mut surface);
    assert_eq!(ink_count(surface.frames.last().unwrap()), 0, "撤销后无墨迹");

    // 重做恢复
    assert!(e.redo());
    e.render(&mut surface);
    assert_eq!(
        ink_count(surface.frames.last().unwrap()),
        ink,
        "重做恢复墨迹"
    );
}

#[test]
fn pressure_changes_stroke_width() {
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    e.handle_event(PlatformEvent::Resize {
        w: 128,
        h: 128,
        scale: 1.0,
    });
    let mut surface = TestSurface { frames: vec![] };

    // 关平滑保证确定性
    e.brush_mut().smoothing = 0.0;
    e.brush_mut().hardness = 1.0;

    draw_line(&mut e, 40.0, 0.25); // 轻压
    draw_line(&mut e, 90.0, 1.0); // 重压
    e.render(&mut surface);
    let frame = surface.frames.last().unwrap();

    let width = |y: u32| -> u32 {
        let (a, b) = ink_span(frame, 128, y);
        if a == u32::MAX {
            0
        } else {
            b - a + 1
        }
    };
    // 垂直跨度即笔宽：size=12 时轻压 ≈3px、满压 =12px
    let mut light = 0;
    for y in 30..50 {
        light += (ink_span(frame, 128, y).0 != u32::MAX) as u32;
    }
    let mut heavy = 0;
    for y in 80..100 {
        heavy += (ink_span(frame, 128, y).0 != u32::MAX) as u32;
    }
    assert!(
        heavy > light * 2,
        "重压笔宽应显著大于轻压: 轻 {light} 行 vs 重 {heavy} 行"
    );
    let _ = width;
}

#[test]
fn pan_zoom_redraw() {
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    e.handle_event(PlatformEvent::Resize {
        w: 64,
        h: 64,
        scale: 1.0,
    });
    let mut surface = TestSurface { frames: vec![] };
    draw_line(&mut e, 20.0, 1.0);
    e.render(&mut surface);
    let before = ink_count(surface.frames.last().unwrap());
    assert!(before > 0);

    // 平移 80px：整条线移出 64px 窗口
    e.document_mut().viewport_mut().pan_by(80.0, 0.0);
    e.render(&mut surface);
    assert_eq!(
        ink_count(surface.frames.last().unwrap()),
        0,
        "平移出窗后无墨迹"
    );

    // 缩放 0.5：画面内容缩小一半回到窗口
    e.document_mut().viewport_mut().pan_by(-80.0, 0.0);
    e.document_mut().viewport_mut().zoom_at((32.0, 32.0), 0.5);
    e.render(&mut surface);
    assert!(
        ink_count(surface.frames.last().unwrap()) > 0,
        "缩放后仍可见"
    );

    // 两笔产生两个撤销组，逐步撤销
    draw_line(&mut e, 40.0, 1.0);
    assert_eq!(e.document().history().undo_len(), 2);
    assert!(e.undo());
    assert!(e.undo());
    assert!(!e.undo(), "历史耗尽");
}
