//! 文字与矢量形状工具（M4）。
//!
//! - 文字：swash 光栅化（字体由调用方提供 TTF/OTF 字节，不内置字体，
//!   保持核心零资源依赖；各端壳层可加载系统字体或用户字体文件）
//! - 形状：矩形/椭圆扫描线填充、直线（dab 链，复用笔刷参数），
//!   均写入活动图层并入撤销历史

use crate::color::Color;
use crate::geometry::Rect;
use crate::history::StrokeRecorder;
use crate::stroke::{Dab, DabMode};
use crate::tile::{TileGrid, TileId};

/// 在网格上以纯色填充像素（画布绝对坐标）。
fn put_pixel(grid: &mut TileGrid, x: i64, y: i64, rgba: [u8; 4]) {
    let tid = TileId::at(x, y);
    let t = grid.get_or_create_mut(tid);
    let (ox, oy) = tid.origin();
    let i = (((y - oy) * 256 + (x - ox)) * 4) as usize;
    if i + 3 < t.pixels_mut().len() {
        t.pixels_mut()[i..i + 4].copy_from_slice(&rgba);
    }
}

/// 形状工具的公共写入器：带撤销采集。
pub struct ShapeWriter<'a> {
    grid: &'a mut TileGrid,
    recorder: &'a mut StrokeRecorder,
    rgba: [u8; 4],
    touched: Vec<TileId>,
}

impl<'a> ShapeWriter<'a> {
    pub fn new(grid: &'a mut TileGrid, recorder: &'a mut StrokeRecorder, color: Color) -> Self {
        Self {
            grid,
            recorder,
            rgba: [color.r, color.g, color.b, 255],
            touched: Vec::new(),
        }
    }

    pub fn px(&mut self, x: i64, y: i64) {
        let tid = TileId::at(x, y);
        if !self.touched.contains(&tid) {
            self.recorder.capture(self.grid, tid); // 写前采集旧值（幂等）
            self.touched.push(tid);
        }
        put_pixel(self.grid, x, y, self.rgba);
    }

    /// 矩形填充。
    pub fn fill_rect(&mut self, rect: Rect) {
        for y in rect.y as i64..rect.y2() {
            for x in rect.x as i64..rect.x2() {
                self.px(x, y);
            }
        }
    }

    /// 椭圆填充（中点扫描线）。
    pub fn fill_ellipse(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) {
        let (rx, ry) = (rx.max(0.5), ry.max(0.5));
        for dy in -(ry.ceil() as i64)..=(ry.ceil() as i64) {
            let t = 1.0 - (dy as f64 / ry) * (dy as f64 / ry);
            if t <= 0.0 {
                continue;
            }
            let half = (t.sqrt() * rx).round() as i64;
            let y = cy as i64 + dy;
            for x in (cx as i64 - half)..=(cx as i64 + half) {
                self.px(x, y);
            }
        }
    }

    /// 椭圆轮廓。
    pub fn stroke_ellipse(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) {
        let (rx, ry) = (rx.max(0.5), ry.max(0.5));
        let n = ((rx + ry) * 6.0) as usize;
        for i in 0..n {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            let x = cx + a.cos() * rx;
            let y = cy + a.sin() * ry;
            self.px(x.round() as i64, y.round() as i64);
        }
    }

    /// 触及的瓦片（调用方用于撤销采集）。
    pub fn touched(self) -> Vec<TileId> {
        self.touched
    }
}

/// 直线：生成 dab 链（复用笔刷参数，硬度/间距语义一致）。
#[allow(clippy::too_many_arguments)]
pub fn line_dabs(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    radius: f32,
    hardness: f32,
    color: Color,
    alpha: f32,
) -> Vec<Dab> {
    let d = ((x1 - x0).hypot(y1 - y0)).max(1e-6);
    let step = (radius * 0.15).max(0.75) as f64;
    let n = (d / step).ceil() as usize;
    (0..=n)
        .map(|i| {
            let f = i as f64 / n as f64;
            Dab {
                x: x0 + (x1 - x0) * f,
                y: y0 + (y1 - y0) * f,
                radius,
                hardness,
                color,
                alpha,
                mode: DabMode::Buildup,
                erase: false,
                tip: None,
                scatter: 0.0,
                aspect: 1.0,
                angle: 0.0,
            }
        })
        .collect()
}

/// swash 文本光栅化到网格。`x, y` 为首字基线左原点（画布坐标）。
/// 返回写入的包围盒，失败（字体无效）返回 None。
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    grid: &mut TileGrid,
    recorder: &mut StrokeRecorder,
    font: &[u8],
    text: &str,
    x: i64,
    y: i64,
    size: f32,
    color: Color,
) -> Option<Rect> {
    use swash::scale::{Render, ScaleContext, Source, StrikeWith};
    use swash::shape::ShapeContext;
    use swash::FontRef;

    let font_ref = FontRef::from_index(font, 0)?;
    let mut scaler_ctx = ScaleContext::new();
    let mut shaper_ctx = ShapeContext::new();

    // shaping：收集 (glyph_id, pen_x, pen_y)
    let mut run = shaper_ctx.builder(font_ref).size(size).build();
    run.add_str(text);
    let mut pen_x = 0f32;
    let mut glyphs: Vec<(u32, f32, f32)> = Vec::new();
    run.shape_with(|cluster| {
        for g in cluster.glyphs {
            glyphs.push((g.id as u32, pen_x + g.x, g.y));
            pen_x += g.advance;
        }
    });
    if glyphs.is_empty() {
        return None;
    }

    // 光栅化：Alpha 源，直接写入瓦片
    let sources = [Source::Outline, Source::Bitmap(StrikeWith::BestFit)];
    let render = Render::new(&sources);
    let mut scaler = scaler_ctx.builder(font_ref).size(size).build();
    let mut min_x = i64::MAX;
    let mut min_y = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = i64::MIN;
    let rgba = [color.r, color.g, color.b];

    for (gid, gx, gy) in glyphs {
        let Some(image) = render.render(&mut scaler, gid as u16) else {
            continue;
        };
        let ox = x + gx.round() as i64 + image.placement.left as i64;
        let oy = y - image.placement.top as i64 - gy.round() as i64;
        let (w, h) = (image.placement.width as i64, image.placement.height as i64);
        for row in 0..h {
            for col in 0..w {
                let a = image.data[(row * w + col) as usize];
                if a == 0 {
                    continue;
                }
                let (px, py) = (ox + col, oy + row);
                recorder.capture(grid, TileId::at(px, py));
                let tid = TileId::at(px, py);
                let t = grid.get_or_create_mut(tid);
                let (tox, toy) = tid.origin();
                let i = (((py - toy) * 256 + (px - tox)) * 4) as usize;
                if i + 3 < t.pixels_mut().len() {
                    let sa = a as f32 / 255.0;
                    let p = t.pixels_mut();
                    for k in 0..3 {
                        p[i + k] = (rgba[k] as f32 * sa + p[i + k] as f32 * (1.0 - sa)) as u8;
                    }
                    let oa = sa + p[i + 3] as f32 / 255.0 * (1.0 - sa);
                    p[i + 3] = (oa * 255.0 + 0.5) as u8;
                }
                min_x = min_x.min(px);
                min_y = min_y.min(py);
                max_x = max_x.max(px + 1);
                max_y = max_y.max(py + 1);
            }
        }
    }
    if max_x < min_x {
        return None;
    }
    Some(Rect::new(
        min_x as i32,
        min_y as i32,
        (max_x - min_x) as u32,
        (max_y - min_y) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{LayerId, LayerStack};

    fn layer() -> (LayerStack, LayerId) {
        let mut s = LayerStack::new();
        let id = s.insert(None);
        (s, id)
    }

    #[test]
    fn rect_and_ellipse_fill() {
        let (mut s, lid) = layer();
        {
            let l = s.get_mut(lid);
            let mut rec = StrokeRecorder::new(lid);
            let mut w = ShapeWriter::new(&mut l.tiles, &mut rec, Color::BLACK);
            w.fill_rect(Rect::new(10, 10, 20, 10));
            w.fill_ellipse(60.0, 20.0, 10.0, 5.0);
            let _ = w.touched();
        }
        let g = &s.get(lid).tiles;
        let px = |x: i64, y: i64| -> u8 {
            g.get(TileId::at(x, y))
                .map(|t| {
                    let (ox, oy) = TileId::at(x, y).origin();
                    t.pixels()[(((y - oy) * 256 + (x - ox)) * 4) as usize + 3]
                })
                .unwrap_or(0)
        };
        assert_eq!(px(15, 15), 255, "矩形内");
        assert_eq!(px(60, 20), 255, "椭圆中心");
        assert_eq!(px(5, 5), 0, "矩形外");
        // 椭圆纵向半径 5：y=20±5 内、±7 外
        assert_eq!(px(60, 24), 255);
        assert_eq!(px(60, 28), 0);
    }

    #[test]
    fn invalid_font_fails_cleanly() {
        let (mut s, lid) = layer();
        let l = s.get_mut(lid);
        let mut rec = StrokeRecorder::new(lid);
        assert!(draw_text(
            &mut l.tiles,
            &mut rec,
            b"not a font",
            "hi",
            10,
            30,
            32.0,
            Color::BLACK
        )
        .is_none());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod font_tests {
    use super::*;

    #[test]
    fn arial_text_rasterizes() {
        let font = std::fs::read("/System/Library/Fonts/Supplemental/Arial.ttf").unwrap();
        let mut s = crate::layer::LayerStack::new();
        let lid = s.insert(None);
        let l = s.get_mut(lid);
        let mut rec = StrokeRecorder::new(lid);
        let bounds = draw_text(
            &mut l.tiles,
            &mut rec,
            &font,
            "Hi",
            20,
            60,
            48.0,
            Color::BLACK,
        )
        .expect("Arial 渲染");
        assert!(bounds.w > 30, "Hi 两个字符宽度合理: {:?}", bounds);
        assert!(bounds.h > 20, "48px 字高合理");
        // 像素验证：bounds 内有墨
        let g = &s.get(lid).tiles;
        let mut ink = 0;
        for y in bounds.y as i64..bounds.y2() {
            for x in bounds.x as i64..bounds.x2() {
                if let Some(t) = g.get(TileId::at(x, y)) {
                    let (ox, oy) = TileId::at(x, y).origin();
                    if t.pixels()[(((y - oy) * 256 + (x - ox)) * 4) as usize + 3] > 0 {
                        ink += 1;
                    }
                }
            }
        }
        assert!(ink > 100, "文字像素数: {ink}");
    }
}
