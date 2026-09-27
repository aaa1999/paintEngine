//! M2.5 收尾基准：CPU vs GPU 合成耗时对比。
//!
//! cargo run -p paint-gpu --example bench_composite --release

use paint_core::color::Color;
use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::history::StrokeRecorder;
use paint_core::layer::BlendMode;
use paint_core::render::Renderer;
use paint_core::stroke::{Dab, DabMode};
use paint_gpu::WgpuRenderer;
use paint_render::SoftwareRenderer;

fn dab(x: f64, y: f64, r: f32, c: Color) -> Dab {
    Dab {
        x,
        y,
        radius: r,
        hardness: 0.7,
        color: c,
        alpha: 1.0,
        mode: DabMode::Buildup,
        erase: false,
    }
}

fn scene(w: u32, h: u32, layers: usize) -> Document {
    let mut doc = Document::new(usize::MAX);
    doc.set_background(Color::WHITE);
    // 底层
    let l = doc.active_layer();
    let dabs: Vec<_> = (0..layers * 40)
        .map(|i| {
            dab(
                40.0 + (i % 40) as f64 * (w as f64 - 80.0) / 40.0,
                60.0 + (i / 40) as f64 * 40.0,
                24.0,
                Color::BLACK,
            )
        })
        .collect();
    paint_render::stamp_dabs(
        &mut doc.layers_mut().get_mut(l).tiles,
        &dabs,
        &mut StrokeRecorder::new(l),
    );
    for k in 1..layers {
        let id = doc.layers_mut().insert(None);
        let layer = doc.layers_mut().get_mut(id);
        let dabs: Vec<_> = (0..40)
            .map(|i| {
                dab(
                    40.0 + i as f64 * (w as f64 - 80.0) / 40.0,
                    (h as f64 / 2.0) + (k as f64 * 12.0),
                    18.0,
                    Color {
                        r: 30 + k as u8 * 40,
                        g: 100,
                        b: 200 - k as u8 * 30,
                    },
                )
            })
            .collect();
        paint_render::stamp_dabs(&mut layer.tiles, &dabs, &mut StrokeRecorder::new(id));
        layer.blend_mode = BlendMode::ALL[k % BlendMode::ALL.len()];
        layer.opacity = 0.8;
    }
    doc
}

fn bench(name: &str, mut f: impl FnMut(&mut [u8])) -> f64 {
    // 预热
    let mut buf = vec![0u8; (2560 * 1440 * 4) as usize];
    f(&mut buf);
    let n = 20;
    let t0 = std::time::Instant::now();
    for _ in 0..n {
        f(&mut buf);
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("{name}: {ms:.2} ms/帧");
    ms
}

fn main() {
    let (w, h) = (2560u32, 1440u32);
    let full = Rect::new(0, 0, w, h);

    for &layers in &[3usize, 6] {
        println!("── {layers} 图层 · {w}×{h} ──");
        let doc = scene(w, h, layers);

        let mut cpu = SoftwareRenderer::new();
        let cpu_ms = bench("CPU 全量", |buf| {
            cpu.composite(&doc, buf, w, full, Some(Color::WHITE));
        });
        let Some(mut gpu) = WgpuRenderer::new() else {
            println!("GPU 不可用，跳过");
            return;
        };
        let gpu_ms = bench("GPU 全量（含回读）", |buf| {
            gpu.composite(&doc, buf, w, full, Some(Color::WHITE));
        });

        // 脏区增量（模拟笔画）：64×64 区域
        let dirty = Rect::new(500, 500, 64, 64);
        let mut gpu2 = WgpuRenderer::new().unwrap();
        bench("GPU 脏区 64×64", |buf| {
            gpu2.composite(&doc, buf, w, dirty, Some(Color::WHITE));
        });

        println!("→ GPU/CPU 全量比: {:.2}x", gpu_ms / cpu_ms);
    }
}
