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
    e.set_show_grid(false); // 屏幕默认带网格点，导出不带；关掉做逐像素对比
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
fn fit_to_content_brings_lost_content_back() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 44.0, 32.0);
    let base = ink_count(&frame(&mut e, &mut s));
    assert!(base > 0);

    // 平移到"迷路"：内容完全移出视野
    e.document_mut().viewport_mut().pan_by(5000.0, 5000.0);
    assert_eq!(ink_count(&frame(&mut e, &mut s)), 0, "内容移出视野");

    // 适应内容：回到全部墨迹
    e.fit_to_content(8.0);
    let back = ink_count(&frame(&mut e, &mut s));
    assert!(back > 0, "fit 后内容回到视野");
    let z = e.document().viewport().zoom();
    assert!((0.9..2.0).contains(&z), "适配缩放合理: {z}");
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

#[test]
fn selection_clips_strokes() {
    let (mut e, mut s) = engine();
    // 先无选区画一笔打底（x 10..50），记录右半基准
    draw(&mut e, 10.0, 50.0, 32.0);
    let base = frame(&mut e, &mut s);
    let right_ink = |f: &[u8]| -> usize {
        f.as_chunks::<4>()
            .0
            .chunks(64)
            .map(|row| {
                row.iter()
                    .skip(34)
                    .filter(|p| p[0] < 128 && p[3] == 255)
                    .count()
            })
            .sum()
    };
    let base_right = right_ink(&base);

    // 矩形选区只覆盖左半（x < 32）
    e.select_rect(Rect::new(0, 0, 32, 64), paint_core::SelectionOp::Replace);
    assert!(e.has_selection());
    assert_eq!(e.document().history().undo_len(), 1, "选区不入撤销");

    // 新一笔横穿边界：右半墨迹应不变（选区外裁剪）
    draw(&mut e, 8.0, 56.0, 12.0);
    let after = right_ink(&frame(&mut e, &mut s));
    assert_eq!(after, base_right, "选区外不可画");
    // 左半应有新增
    let f = frame(&mut e, &mut s);
    let left = f
        .as_chunks::<4>()
        .0
        .chunks(64)
        .map(|row| {
            row.iter()
                .take(30)
                .filter(|p| p[0] < 128 && p[3] == 255)
                .count()
        })
        .sum::<usize>();
    assert!(left > 0, "选区内可画");

    // 清除选区后右半可画
    e.clear_selection();
    draw(&mut e, 40.0, 56.0, 12.0);
    let freed = right_ink(&frame(&mut e, &mut s));
    assert!(freed > base_right, "清选区后自由");
}

#[test]
fn lasso_selection_polygon_fill() {
    let (mut e, mut s) = engine();
    // 三角形套索（画布坐标 ≈ 屏幕，zoom1）
    e.select_lasso(
        &[(10.0, 10.0), (50.0, 10.0), (30.0, 50.0)],
        paint_core::SelectionOp::Replace,
    );
    // 三角形重心 (30, ~23) 附近可画；外点 (56, 40) 不可画
    draw(&mut e, 26.0, 34.0, 23.0);
    draw(&mut e, 52.0, 58.0, 40.0);
    let f = frame(&mut e, &mut s);
    let row = |y: usize| -> (usize, usize) {
        let r = &f.as_chunks::<4>().0[y * 64..(y + 1) * 64];
        (
            r.iter()
                .take(40)
                .filter(|p| p[0] < 128 && p[3] == 255)
                .count(),
            r.iter()
                .skip(46)
                .filter(|p| p[0] < 128 && p[3] == 255)
                .count(),
        )
    };
    let (inside, _outside) = row(23);
    assert!(inside > 0, "三角形内可画");
    let (_, far_out) = row(40);
    assert_eq!(far_out, 0, "三角形外不可画");
}

#[test]
fn shapes_undoable_and_clipped() {
    let (mut e, mut s) = engine();
    // 矩形填充入撤销
    assert!(e.fill_rect(Rect::new(10, 10, 20, 20)));
    assert_eq!(e.document().history().undo_len(), 1);
    let f1 = frame(&mut e, &mut s);
    let ink1 = ink_count(&f1);
    assert!(ink1 > 0);
    assert!(e.undo());
    assert_eq!(ink_count(&frame(&mut e, &mut s)), 0, "形状可撤销");

    // 直线：dab 链
    assert!(e.stroke_line(10.0, 30.0, 50.0, 30.0));
    let ink2 = ink_count(&frame(&mut e, &mut s));
    assert!(ink2 > 0);
    assert!(e.undo());

    // 椭圆
    assert!(e.fill_ellipse(32.0, 32.0, 12.0, 8.0));
    assert!(ink_count(&frame(&mut e, &mut s)) > 0);
}

#[test]
fn content_transform_move_commit_undo() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 40.0, 32.0); // 墨迹 x 10..40
    let base = ink_count(&frame(&mut e, &mut s));
    assert!(base > 0);

    // 整层变换（无选区）：提升 → 平移 → 预览可见（位置变化）
    assert!(e.begin_transform());
    assert!(e.transforming());
    e.transform_translate(20.0, 0.0);
    let moved = frame(&mut e, &mut s);
    // 原位置 x 10..40 → 30..60：x 12..18 区间应无墨（被搬走）
    let old_zone = moved
        .as_chunks::<4>()
        .0
        .chunks(64)
        .map(|row| row.iter().take(18).skip(12).filter(|p| p[0] < 128).count())
        .sum::<usize>();
    assert_eq!(old_zone, 0, "原位被清空");
    let ink = ink_count(&moved);
    // 平移后部分边缘出界（帧宽 64）：允许少量损失
    assert!(ink >= base - 8, "平移预览墨量近似不变: {ink} vs {base}");

    // 提交：入撤销（历史 +1）
    let hist = e.document().history().undo_len();
    assert!(e.commit_transform());
    assert_eq!(e.document().history().undo_len(), hist + 1);
    let committed = ink_count(&frame(&mut e, &mut s));
    assert!(
        committed >= base - 8,
        "提交后近似等量: {committed} vs {base}"
    );

    // 撤销 → 完全复原（内容回到原位）
    assert!(e.undo());
    let restored = frame(&mut e, &mut s);
    assert_eq!(ink_count(&restored), base);
    let back = restored
        .as_chunks::<4>()
        .0
        .chunks(64)
        .map(|row| row.iter().take(18).skip(12).filter(|p| p[0] < 128).count())
        .sum::<usize>();
    assert!(back > 0, "撤销后回到原位");
}

#[test]
fn content_transform_cancel_restores() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 40.0, 32.0);
    let before = frame(&mut e, &mut s);
    let hist = e.document().history().undo_len();

    assert!(e.begin_transform());
    e.transform_rotate(1.2);
    e.transform_scale(1.8);
    assert!(e.cancel_transform());

    let after = frame(&mut e, &mut s);
    assert_eq!(ink_count(&after), ink_count(&before));
    assert_eq!(e.document().history().undo_len(), hist, "取消不入历史");
    // 像素级对比（恒等放回）
    let diff = before
        .iter()
        .zip(after.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(diff, 0, "取消应逐像素复原");
}

#[test]
fn content_transform_selection_only_lifts_selected() {
    let (mut e, mut s) = engine();
    draw(&mut e, 10.0, 50.0, 32.0); // 长线 x 10..50
                                    // 选区只罩住左半
    e.select_rect(Rect::new(10, 20, 20, 30), paint_core::SelectionOp::Replace);
    assert!(e.begin_transform());
    e.transform_translate(30.0, 0.0);
    let f = frame(&mut e, &mut s);
    let zone = |x0: usize, x1: usize| -> usize {
        f.as_chunks::<4>()
            .0
            .chunks(64)
            .map(|row| {
                row.iter()
                    .skip(x0)
                    .take(x1 - x0)
                    .filter(|p| p[0] < 128)
                    .count()
            })
            .sum()
    };
    // 选区部分（x 10..30）被搬走 → 落到 40..60
    assert_eq!(zone(12, 18), 0, "选区内原位清空");
    assert!(zone(42, 48) > 0, "选区内容移到 40..60");
    // 选区外（x 30..50）留在原地
    assert!(zone(32, 40) > 0, "选区外不动");
    assert!(e.commit_transform());
}
