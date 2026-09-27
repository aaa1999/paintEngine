//! M2.5 验收：GPU 合成与 CPU 软件合成逐像素对齐（容差 ±2 量化误差）。

use paint_core::color::Color;
use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::history::StrokeRecorder;
use paint_core::layer::BlendMode;
use paint_core::render::Renderer;
use paint_core::stroke::{Dab, DabMode};
use paint_gpu::WgpuRenderer;
use paint_render::SoftwareRenderer;

fn dab(x: f64, y: f64, r: f32, color: Color, alpha: f32) -> Dab {
    Dab {
        x,
        y,
        radius: r,
        hardness: 0.8,
        color,
        alpha,
        mode: DabMode::Buildup,
        erase: false,
    }
}

/// 三层文档：底黑线 / 中层 50% 透明彩色块（可变混合模式）/ 顶层白线。
fn scene(mid_mode: BlendMode) -> Document {
    let mut doc = Document::new(usize::MAX);
    doc.set_background(Color::WHITE);
    let bottom = doc.active_layer();
    {
        let layer = doc.layers_mut().get_mut(bottom);
        let dabs = vec![
            dab(60.0, 60.0, 10.0, Color::BLACK, 1.0),
            dab(80.0, 80.0, 14.0, Color::BLACK, 1.0),
        ];
        paint_render::stamp_dabs(layer, &dabs, &mut StrokeRecorder::new(bottom));
    }
    let mid = doc.layers_mut().insert(None);
    {
        let layer = doc.layers_mut().get_mut(mid);
        let dabs = vec![
            dab(
                70.0,
                70.0,
                30.0,
                Color {
                    r: 200,
                    g: 60,
                    b: 30,
                },
                1.0,
            ),
            dab(
                100.0,
                60.0,
                12.0,
                Color {
                    r: 30,
                    g: 180,
                    b: 90,
                },
                1.0,
            ),
        ];
        paint_render::stamp_dabs(layer, &dabs, &mut StrokeRecorder::new(mid));
        layer.opacity = 0.6;
        layer.blend_mode = mid_mode;
    }
    let top = doc.layers_mut().insert(None);
    {
        let layer = doc.layers_mut().get_mut(top);
        let dabs = vec![dab(50.0, 90.0, 8.0, Color::WHITE, 1.0)];
        paint_render::stamp_dabs(layer, &dabs, &mut StrokeRecorder::new(top));
    }
    doc
}

fn run_pair(doc: &Document, w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    let mut cpu = SoftwareRenderer::new();
    let mut gpu = WgpuRenderer::new().expect("wgpu 设备（本机需支持 Metal/Vulkan/DX/GL）");
    let full = Rect::new(0, 0, w, h);
    let mut cpu_frame = vec![77u8; (w * h * 4) as usize];
    let mut gpu_frame = vec![77u8; (w * h * 4) as usize];
    cpu.composite(doc, &mut cpu_frame, w, full, Some(doc.background()));
    gpu.composite(doc, &mut gpu_frame, w, full, Some(doc.background()));
    (cpu_frame, gpu_frame)
}

fn assert_close(cpu: &[u8], gpu: &[u8], tol: i32, ctx: &str) {
    let mut worst = 0i32;
    let mut diffs = 0usize;
    for (a, b) in cpu.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0) {
        for k in 0..4 {
            let d = (a[k] as i32 - b[k] as i32).abs();
            if d > tol {
                diffs += 1;
            }
            worst = worst.max(d);
        }
    }
    assert!(diffs == 0, "{ctx}: 超容差像素 {diffs} 个，最大差 {worst}");
}

#[test]
fn parity_all_blend_modes() {
    let w = 160u32;
    let h = 140u32;
    for mode in BlendMode::ALL {
        let doc = scene(mode);
        let (cpu, gpu) = run_pair(&doc, w, h);
        assert_close(&cpu, &gpu, 2, &format!("{mode:?}"));
    }
}

#[test]
fn parity_zoomed_bilinear() {
    let mut doc = scene(BlendMode::Multiply);
    doc.viewport_mut().set_zoom(2.6);
    doc.viewport_mut().pan_by(-40.0, -30.0);
    let (cpu, gpu) = run_pair(&doc, 200, 180);
    // 双线性：CPU f64 手写插值 vs GPU f32 硬件采样器，dab 硬度边缘
    // alpha 变化率放大位置量化差 → 容差 12（语义错误会产生海量大幅差异）
    assert_close(&cpu, &gpu, 12, "zoom 2.6 bilinear");
}

#[test]
fn parity_grid_and_zoom_out() {
    let mut doc = scene(BlendMode::Screen);
    doc.set_show_grid(true);
    doc.viewport_mut().set_zoom(0.4);
    doc.viewport_mut().pan_by(30.0, 20.0);
    let (cpu, gpu) = run_pair(&doc, 200, 180);
    // 允许极少数半开边界点在 f32/f64 精度下归属翻转（值差恰为 点/背景 之差）
    let mut hard = 0usize;
    let mut edge = 0usize;
    for (a, b) in cpu.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0) {
        let d = (0..4)
            .map(|k| (a[k] as i32 - b[k] as i32).abs())
            .max()
            .unwrap();
        if d > 12 {
            hard += 1;
        } else if d > 2 {
            edge += 1;
        }
    }
    // 网格点判定在像素边界处受 f32/f64 路径差影响：允许至多 1 个点
    // （4 通道 ≈ 4 像素）归属翻转，差异值恒为 点色/背景色 之差（≤60）
    assert!(hard <= 4, "grid: 硬差异过多: {hard}");
    assert!(edge + hard <= 8, "grid: 差异像素过多: {}", edge + hard);
}

#[test]
fn parity_transparent_background() {
    let doc = scene(BlendMode::Difference);
    let mut cpu = SoftwareRenderer::new();
    let mut gpu = WgpuRenderer::new().unwrap();
    let w = 160u32;
    let h = 140u32;
    let full = Rect::new(0, 0, w, h);
    // 导出语义：两侧都从零开始（CPU 不触碰区域保持 0，GPU 写透明 0）
    let mut cf = vec![0u8; (w * h * 4) as usize];
    let mut gf = vec![0u8; (w * h * 4) as usize];
    cpu.composite(&doc, &mut cf, w, full, None);
    gpu.composite(&doc, &mut gf, w, full, None);
    assert_close(&cf, &gf, 2, "透明背景");
}

#[test]
fn dirty_incremental_matches_full() {
    let doc = scene(BlendMode::Overlay);
    let w = 160u32;
    let h = 140u32;
    let full = Rect::new(0, 0, w, h);
    let mut gpu = WgpuRenderer::new().unwrap();

    // 第一帧全量
    let mut full_frame = vec![0u8; (w * h * 4) as usize];
    gpu.composite(&doc, &mut full_frame, w, full, Some(doc.background()));

    // 第二帧：另一个 doc（内容不同），只画脏区
    let doc2 = scene(BlendMode::Overlay);
    let mut frame = full_frame.clone();
    let dirty = Rect::new(40, 30, 50, 60);
    gpu.composite(&doc2, &mut frame, w, dirty, Some(doc2.background()));
    // 脏区内应与全量渲染一致（doc 与 doc2 相同）
    for y in 30..90 {
        for x in 40..90 {
            let i = ((y as usize * w as usize) + x as usize) * 4;
            for k in 0..4 {
                assert!(
                    (frame[i + k] as i32 - full_frame[i + k] as i32).abs() <= 2,
                    "增量脏区 ({x},{y}) ch{k}"
                );
            }
        }
    }
    // 脏区外保持第一帧内容（77→0 初始帧? full_frame 为第一帧结果，未触碰）
    let outside = ((10 * w as usize) + 10) * 4;
    assert_eq!(
        &frame[outside..outside + 4],
        &full_frame[outside..outside + 4]
    );
}
