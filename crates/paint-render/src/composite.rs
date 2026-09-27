use paint_core::blend::composite_pixel;
use paint_core::color::Color;
use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::layer::BlendMode;
use paint_core::tile::{TileData, TileId, TILE};

/// 合成可见图层到目标缓冲（RGBA8 预乘）。只处理 `dirty` 区域。
///
/// 采样：放大（zoom ≥ 1）双线性——在瓦片内钳位邻域，避免跨瓦片
/// 采样造成的接缝；缩小最近邻（mipmap 为后续优化）。
/// `background: None` 时目标保持透明（PNG 导出等）。
/// Normal 走快速路径，其余混合模式走 blend::composite_pixel。
pub fn composite(
    doc: &Document,
    target: &mut [u8],
    width: u32,
    dirty: Rect,
    background: Option<Color>,
) {
    let h = if width == 0 {
        0
    } else {
        (target.len() / (width as usize * 4)) as u32
    };
    if h == 0 {
        return;
    }
    let full = Rect::new(0, 0, width, h);
    let Some(clip) = dirty.intersect(&full) else {
        return;
    };

    if let Some(bg) = background {
        paint_background(bg, target, width, &clip);
        if doc.show_grid() {
            paint_grid(doc, target, width, &clip);
        }
    }

    let vp = doc.viewport();
    let zoom = vp.zoom();
    let (pan_x, pan_y) = vp.pan();
    let inv_zoom = 1.0 / zoom;
    // 放大（>1）才双线性；1:1 时像素中心正落在源像素上，
    // 双线性反而混合相邻像素造成模糊
    let bilinear = zoom > 1.0;
    // 每行常数增量：canvas_x = x*inv + row_base_x
    let row_base_x = (0.5 - pan_x) * inv_zoom;
    let step_x = inv_zoom;

    for layer in doc.layers().iter().filter(|l| l.visible && l.opacity > 0.0) {
        let opacity = layer.opacity.clamp(0.0, 1.0);
        let mode = layer.blend_mode;
        for y in clip.y..(clip.y + clip.h as i32) {
            let cy = (y as f64 + 0.5 - pan_y) * inv_zoom;
            let iy = cy.floor() as i64;
            let ty = iy >> 8; // 瓦片索引（负坐标下算术移位正确）
            let fy = cy - ((ty << 8) as f64); // 瓦片内浮点 y（双线性用）
            let ly = (iy & 255) as usize; // 瓦片内 y 偏移（最近邻用）
            let row = (y as u32 * width) as usize * 4;

            // 瓦片行内缓存：x 单调递增，瓦片 id 只增不减
            let mut cache_key = u64::MAX;
            let mut cache: Option<&TileData> = None;

            let mut cx = row_base_x + clip.x as f64 * step_x;
            for x in clip.x..(clip.x + clip.w as i32) {
                let cx_cur = cx;
                cx += step_x;
                let ix = cx_cur.floor() as i64;
                let tx = ix >> 8;
                let fx = cx_cur - ((tx << 8) as f64); // 瓦片内浮点 x
                let lx = (ix & 255) as usize; // 最近邻 x 偏移

                let key = ((ty as u32 as u64) << 32) | (tx as u32 as u64);
                if key != cache_key {
                    cache = layer
                        .tiles
                        .get(TileId {
                            x: tx as i32,
                            y: ty as i32,
                        })
                        .map(|t| &**t);
                    cache_key = key;
                }

                let Some(tile) = cache else {
                    continue;
                };
                let p = tile.pixels();
                let d = row + x as usize * 4;

                if bilinear {
                    // 双线性：锚点对齐标准纹理语义（texel i 在 L=i+0.5 满权重），
                    // 与 GPU 采样器一致；邻域钳位在瓦片内（边缘外推，无接缝）
                    let (ax, bx, xi, xj) = bilerp_idx(fx - 0.5);
                    let (ay, by, yi, yj) = bilerp_idx(fy - 0.5);
                    let t = TILE as usize;
                    let i00 = (yi * t + xi) * 4;
                    let i10 = (yi * t + xj) * 4;
                    let i01 = (yj * t + xi) * 4;
                    let i11 = (yj * t + xj) * 4;
                    let a = (ax * ay) as f64;
                    let b = (bx * ay) as f64;
                    let c = (ax * by) as f64;
                    let e = (bx * by) as f64;
                    let sa = (p[i00 + 3] as f64 * a
                        + p[i10 + 3] as f64 * b
                        + p[i01 + 3] as f64 * c
                        + p[i11 + 3] as f64 * e)
                        / 255.0
                        * opacity as f64;
                    if sa <= 0.0 {
                        continue;
                    }
                    let mix = |k: usize| {
                        p[i00 + k] as f64 * a
                            + p[i10 + k] as f64 * b
                            + p[i01 + k] as f64 * c
                            + p[i11 + k] as f64 * e
                    };
                    if mode == BlendMode::Normal {
                        let inv = 1.0 - sa;
                        target[d] = over_ch(mix(0) * opacity as f64, target[d] as f64, inv);
                        target[d + 1] = over_ch(mix(1) * opacity as f64, target[d + 1] as f64, inv);
                        target[d + 2] = over_ch(mix(2) * opacity as f64, target[d + 2] as f64, inv);
                        target[d + 3] = over_ch(mix(3) * opacity as f64, target[d + 3] as f64, inv);
                    } else {
                        let q = [
                            (mix(0) * opacity as f64) as u8,
                            (mix(1) * opacity as f64) as u8,
                            (mix(2) * opacity as f64) as u8,
                            (mix(3) * opacity as f64) as u8,
                        ];
                        composite_pixel(&mut target[d..d + 4], &q, opacity, mode);
                    }
                } else {
                    let i = ((ly * TILE as usize) + lx) * 4;
                    let sa = p[i + 3] as f64 / 255.0 * opacity as f64;
                    if sa <= 0.0 {
                        continue;
                    }
                    if mode == BlendMode::Normal {
                        // 快速路径：预乘 source-over
                        let inv = 1.0 - sa;
                        target[d] = over_ch(p[i] as f64 * opacity as f64, target[d] as f64, inv);
                        target[d + 1] =
                            over_ch(p[i + 1] as f64 * opacity as f64, target[d + 1] as f64, inv);
                        target[d + 2] =
                            over_ch(p[i + 2] as f64 * opacity as f64, target[d + 2] as f64, inv);
                        target[d + 3] =
                            over_ch(p[i + 3] as f64 * opacity as f64, target[d + 3] as f64, inv);
                    } else {
                        composite_pixel(&mut target[d..d + 4], &p[i..i + 4], opacity, mode);
                    }
                }
            }
        }
    }
}

fn over_ch(src_premul: f64, dst_premul: f64, inv: f64) -> u8 {
    (src_premul + dst_premul * inv + 0.5).clamp(0.0, 255.0) as u8
}

/// 双线性采样索引：返回 (低位权重, 高位权重, 低位索引, 高位索引)。
/// `f` 为瓦片内浮点坐标 0..256，索引钳位在 0..=255（边缘外推防接缝）。
fn bilerp_idx(f: f64) -> (f32, f32, usize, usize) {
    let i0 = f.floor() as i64;
    let i0c = i0.clamp(0, 255) as usize;
    let i1c = (i0 + 1).clamp(0, 255) as usize;
    let t = (f - i0 as f64).clamp(0.0, 1.0) as f32;
    (1.0 - t, t, i0c, i1c)
}

/// 空白区点阵网格：锚定画布空间（平移时随之移动，传达无限空间感），
/// 间距在 256 的倍数中自适应（屏幕间距 ≥ 32px），颜色随背景亮度
/// 取柔和对比。只在背景绘制阶段叠加，图层内容覆盖其上。
fn paint_grid(doc: &Document, target: &mut [u8], width: u32, clip: &Rect) {
    let vp = doc.viewport();
    let zoom = vp.zoom();
    let (pan_x, pan_y) = vp.pan();

    let mut spacing = 256.0f64;
    while spacing * zoom < 32.0 {
        spacing *= 2.0;
    }

    let bg = doc.background();
    let lum = 0.299 * bg.r as f32 + 0.587 * bg.g as f32 + 0.114 * bg.b as f32;
    let mix = |c: u8| -> u8 {
        if lum >= 128.0 {
            ((c as u16 * 205) / 255) as u8 // 亮底 → 深点
        } else {
            (c as u16 + ((255 - c as u16) * 45) / 255) as u8 // 暗底 → 浅点
        }
    };
    let (dr, dg, db) = (mix(bg.r), mix(bg.g), mix(bg.b));

    let (cx0, cy0) = vp.screen_to_canvas(clip.x as f64, clip.y as f64);
    let (cx1, cy1) = vp.screen_to_canvas(clip.x2() as f64, clip.y2() as f64);

    // 判定窗口与 GPU 着色器一致：像素中心 ±0.5（开区间）
    let hit = |s: f64, p: i64| -> bool {
        // 与 GPU 着色器同精度（f32）：避免判定边界处两侧四舍五入不同
        let d = (s as f32) - p as f32 - 0.5;
        (-0.5..0.5).contains(&d)
    };
    let mut cy = (cy0 / spacing).floor() * spacing;
    while cy <= cy1 {
        let sy_f = cy * zoom + pan_y;
        let py_lo = ((sy_f - 0.5).floor() as i64).max(clip.y as i64);
        let py_hi = ((sy_f + 0.5).ceil() as i64).min(clip.y2());
        for sy in py_lo..py_hi {
            if !hit(sy_f, sy) {
                continue;
            }
            let mut cx = (cx0 / spacing).floor() * spacing;
            while cx <= cx1 {
                let sx_f = cx * zoom + pan_x;
                let px_lo = ((sx_f - 0.5).floor() as i64).max(clip.x as i64);
                let px_hi = ((sx_f + 0.5).ceil() as i64).min(clip.x2());
                for sx in px_lo..px_hi {
                    if !hit(sx_f, sx) {
                        continue;
                    }
                    let i = ((sy as u32 * width) as usize + sx as usize) * 4;
                    if i + 3 < target.len() {
                        target[i] = dr;
                        target[i + 1] = dg;
                        target[i + 2] = db;
                        target[i + 3] = 255;
                    }
                }
                cx += spacing;
            }
        }
        cy += spacing;
    }
}

fn paint_background(bg: Color, target: &mut [u8], width: u32, clip: &Rect) {
    let (r, g, b) = (bg.r, bg.g, bg.b);
    for y in clip.y..(clip.y + clip.h as i32) {
        let row = (y as u32 * width) as usize * 4;
        let start = row + clip.x as usize * 4;
        let end = start + clip.w as usize * 4;
        let mut i = start;
        while i < end {
            target[i] = r;
            target[i + 1] = g;
            target[i + 2] = b;
            target[i + 3] = 255;
            i += 4;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paint_core::history::StrokeRecorder;
    use paint_core::stroke::{Dab, DabMode};

    /// 64×64 白底帧，在画布 (32,32) 处盖章黑色 dab（半径 6）。
    fn scene() -> (Document, u32) {
        let mut doc = Document::new(usize::MAX);
        doc.set_background(Color::WHITE);
        let lid = doc.active_layer();
        let layers = doc.layers_mut();
        let dabs = vec![Dab {
            x: 32.0,
            y: 32.0,
            radius: 6.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
        }];
        let layer = layers.get_mut(lid);
        super::super::stamp::stamp_dabs(layer, &dabs, &mut StrokeRecorder::new(lid));
        (doc, 64)
    }

    fn px(frame: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        frame[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn renders_dab_on_background() {
        let (doc, w) = scene();
        let mut frame = vec![9u8; (w * w * 4) as usize];
        let dirty = Rect::new(0, 0, w, w);
        composite(&doc, &mut frame, w, dirty, Some(doc.background()));
        assert_eq!(px(&frame, w, 32, 32), [0, 0, 0, 255]);
        assert_eq!(px(&frame, w, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px(&frame, w, 50, 50), [255, 255, 255, 255]);
    }

    #[test]
    fn transparent_background_keeps_alpha() {
        let (doc, w) = scene();
        let mut frame = vec![9u8; (w * w * 4) as usize];
        composite(&doc, &mut frame, w, Rect::new(0, 0, w, w), None);
        // dab 处为不透明黑；无背景填充，空白区不被触碰（保持哨兵值）
        assert_eq!(px(&frame, w, 32, 32), [0, 0, 0, 255]);
        assert_eq!(px(&frame, w, 0, 0), [9, 9, 9, 9], "无背景时目标保持原值");
    }

    #[test]
    fn dirty_region_limits_composite() {
        let (doc, w) = scene();
        let mut frame = vec![9u8; (w * w * 4) as usize];
        // 只重绘左上 16×16（远离 dab）
        let dirty = Rect::new(0, 0, 16, 16);
        composite(&doc, &mut frame, w, dirty, Some(doc.background()));
        assert_eq!(px(&frame, w, 0, 0), [255, 255, 255, 255], "脏区内被重绘");
        assert_eq!(px(&frame, w, 32, 32), [9, 9, 9, 9], "脏区外保持原值");
    }

    #[test]
    fn pan_moves_content() {
        let (mut doc, w) = scene();
        doc.viewport_mut().pan_by(20.0, 0.0); // 画布内容在屏幕上右移 20px
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        assert_eq!(
            px(&frame, w, 52, 32),
            [0, 0, 0, 255],
            "平移后墨迹出现在 32+20 处"
        );
        assert_eq!(px(&frame, w, 32, 32), [255, 255, 255, 255]);
    }

    #[test]
    fn zoom_out_shrinks_content() {
        let (mut doc, w) = scene();
        doc.viewport_mut().set_zoom(0.5);
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        assert_eq!(
            px(&frame, w, 16, 16),
            [0, 0, 0, 255],
            "缩放 0.5 后墨迹中心移到 (16,16)"
        );
    }

    #[test]
    fn hidden_layer_skipped() {
        let (mut doc, w) = scene();
        let lid = doc.active_layer();
        doc.layers_mut().get_mut(lid).visible = false;
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        assert_eq!(px(&frame, w, 32, 32), [255, 255, 255, 255]);
    }

    #[test]
    fn layer_opacity_blends() {
        let (mut doc, w) = scene();
        let lid = doc.active_layer();
        doc.layers_mut().get_mut(lid).opacity = 0.5;
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        // 黑 50% 叠白 → 128
        let c = px(&frame, w, 32, 32);
        assert_eq!(c[0], 128);
    }

    #[test]
    fn multiply_mode_darkens() {
        let (mut doc, w) = scene();
        let lid = doc.active_layer();
        doc.layers_mut().get_mut(lid).blend_mode = BlendMode::Multiply;
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        // 黑 multiply 白底 → 黑不变；白底区域保持白
        assert_eq!(px(&frame, w, 32, 32), [0, 0, 0, 255]);
        assert_eq!(px(&frame, w, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn bilinear_smooths_zoomed_edges() {
        let (mut doc, _) = scene();
        doc.viewport_mut().set_zoom(2.0);
        let w = 128u32;
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        // dab 中心画布(32,32) → 屏幕(64,64)；硬边 r=6 → 屏幕边缘在 x≈76。
        // 双线性应产生中间灰度（最近邻只会是 0 或 255）
        let row = 64;
        let grads: Vec<u8> = (70..=82)
            .map(|x| {
                let i = ((row * w + x) * 4) as usize;
                frame[i]
            })
            .filter(|&v| v > 0 && v < 255)
            .collect();
        assert!(!grads.is_empty(), "放大边缘应有渐变像素");
    }

    #[test]
    fn dot_grid_anchors_to_canvas() {
        let mut doc = Document::new(usize::MAX);
        doc.set_background(Color::WHITE);
        doc.set_show_grid(true);
        let w = 300u32;
        let full = Rect::new(0, 0, w, w);
        let mut frame = vec![255u8; (w * w * 4) as usize];
        composite(&doc, &mut frame, w, full, Some(doc.background()));
        // 画布 (256,256) 网格点 → 屏幕 (256,256)，白底深点；(0,0) 也是网格点
        assert_eq!(px(&frame, w, 256, 256), [205, 205, 205, 255], "网格点");
        assert_eq!(px(&frame, w, 0, 0), [205, 205, 205, 255], "原点网格点");
        assert_eq!(
            px(&frame, w, 255, 255),
            [255, 255, 255, 255],
            "非网格点保持背景"
        );
        // 平移 +37：网格点随之移动（画布空间锚定，传达空间感）
        doc.viewport_mut().pan_by(37.0, 0.0);
        composite(&doc, &mut frame, w, full, Some(doc.background()));
        assert_eq!(
            px(&frame, w, 256 + 37, 256),
            [205, 205, 205, 255],
            "网格随平移移动"
        );
        // 关闭后无网格
        doc.set_show_grid(false);
        composite(&doc, &mut frame, w, full, Some(doc.background()));
        assert_eq!(px(&frame, w, 256 + 37, 256), [255, 255, 255, 255]);
    }

    #[test]
    fn grid_absent_on_transparent_export_path() {
        let mut doc = Document::new(usize::MAX);
        doc.set_background(Color::WHITE);
        doc.set_show_grid(true);
        let w = 300u32;
        let mut frame = vec![0u8; (w * w * 4) as usize];
        // 导出走 background: None —— 不应有网格
        composite(&doc, &mut frame, w, Rect::new(0, 0, w, w), None);
        assert_eq!(px(&frame, w, 256, 256), [0, 0, 0, 0], "透明导出无网格点");
    }

    #[test]
    fn two_layers_stack() {
        let (mut doc, w) = scene();
        let top = doc.layers_mut().insert(None);
        let layers = doc.layers_mut();
        let dabs = vec![Dab {
            x: 32.0,
            y: 32.0,
            radius: 3.0,
            hardness: 1.0,
            color: Color::WHITE,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
        }];
        let layer = layers.get_mut(top);
        super::super::stamp::stamp_dabs(layer, &dabs, &mut StrokeRecorder::new(top));
        let mut frame = vec![0u8; (w * w * 4) as usize];
        composite(
            &doc,
            &mut frame,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        // 顶层白 dab 半径 3 覆盖中心；半径 3..6 环带仍是黑
        assert_eq!(px(&frame, w, 32, 32), [255, 255, 255, 255]);
        assert_eq!(px(&frame, w, 36, 32)[0], 0, "半径 3..6 环带仍是底黑");
    }
}
