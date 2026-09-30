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
            id: 1,
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
        .as_chunks::<4>()
        .0
        .iter()
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

/// 复刻 Web 序列：文字对象（raster）→ 新层 → 文字对象 → solo → render。
/// 回归 wasm 上的 solo panic。
#[test]
fn solo_render_with_text_objects() {
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    e.handle_event(PlatformEvent::Resize { w: 64, h: 64, scale: 1.0 });
    let raster = |c: u8| paint_core::layer::TextRaster {
        premul: [[c, 0, 255 - c, 255u8]; 16].concat(),
        w: 2,
        h: 2,
        dx: 0,
        dy: 0,
    };
    assert!(e.add_text_object((10.0, 10.0), "R", 8.0, Some(raster(255))));
    e.add_layer();
    assert!(e.add_text_object((40.0, 40.0), "B", 8.0, Some(raster(0))));
    let mut surf = TestSurface { frames: Vec::new() };
    e.render(&mut surf);
    e.set_solo_index(Some(1));
    e.render(&mut surf);
    e.set_solo_index(None);
    e.render(&mut surf);
    assert!(surf.frames.len() >= 3);
}

/// 复刻 Web 滚轮交互：interactive 渲染 + 连续 zoom_at + 每次后 render。
/// 回归"滚轮即挂死"。
#[test]
fn interactive_zoom_render_loop() {
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    e.handle_event(PlatformEvent::Resize { w: 2048, h: 1024, scale: 1.0 });
    let raster = paint_core::layer::TextRaster {
        premul: vec![[60u8, 60, 60, 255]; 16].concat(),
        w: 2,
        h: 2,
        dx: 0,
        dy: 0,
    };
    e.add_text_object((300.0, 300.0), "A", 32.0, Some(raster));
    e.set_interactive(true);
    let mut surf = TestSurface { frames: Vec::new() };
    for i in 0..40 {
        let f = if i % 2 == 0 { 1.1 } else { 1.0 / 1.1 };
        e.document_mut().viewport_mut().zoom_at((1024.0, 512.0), f);
        e.render(&mut surf); // 挂死会超时
    }
    e.set_interactive(false);
    e.render(&mut surf);
    assert!(surf.frames.len() >= 40, "{}", surf.frames.len());
}

/// 交互期低清渲染性能基准：同序列开/关 interactive 的渲染耗时对比。
#[test]
fn interactive_lowres_benchmark() {
    use std::time::Instant;
    let mut e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    // 平板级分辨率（1880×3008 ≈ 5.6MP）
    e.handle_event(PlatformEvent::Resize { w: 1880, h: 3008, scale: 1.0 });
    let raster = paint_core::layer::TextRaster {
        premul: vec![[60u8, 60, 60, 255]; 16].concat(),
        w: 2, h: 2, dx: 0, dy: 0,
    };
    e.add_text_object((500.0, 800.0), "A", 32.0, Some(raster));
    let mut surf = TestSurface { frames: Vec::new() };
    let run = |e: &mut Engine, s: &mut TestSurface, inter: bool| -> f64 {
        e.set_interactive(inter);
        let t0 = Instant::now();
        for i in 0..20 {
            let f = if i % 2 == 0 { 1.1 } else { 1.0 / 1.1 };
            e.document_mut().viewport_mut().zoom_at((940.0, 1504.0), f);
            e.render(s);
        }
        t0.elapsed().as_secs_f64() / 20.0 * 1000.0 // 每帧毫秒
    };
    let full = run(&mut e, &mut surf, false);
    let low = run(&mut e, &mut surf, true);
    // 重场景：多层 + 放大（双线性路径）——贴近用户捏合缩放时
    for l in 0..3 {
        e.add_layer();
        let raster = paint_core::layer::TextRaster {
            premul: vec![[60u8, 60, 60, 255]; 16].concat(),
            w: 2, h: 2, dx: (l * 40) as i64, dy: (l * 40) as i64,
        };
        e.add_text_object((300.0 + l as f64 * 200.0, 500.0), "X", 64.0, Some(raster));
    }
    e.document_mut().viewport_mut().set_zoom(4.0); // 放大 → 双线性
    let heavy_full = run(&mut e, &mut surf, false);
    let heavy_low = run(&mut e, &mut surf, true);
    eprintln!(
        "[bench] 空: {full:.1}→{low:.1}ms | 重(3层+4x双线性): {heavy_full:.1}→{heavy_low:.1}ms（{:.1}x）",
        heavy_full / heavy_low
    );
    assert!(heavy_low < heavy_full, "重场景低清应更快: {heavy_full:.1} vs {heavy_low:.1}");
}
