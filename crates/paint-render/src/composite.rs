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
    if let Some(sel) = doc.selection() {
        paint_selection_outline(doc, sel, target, width, &clip);
    }

    let vp = doc.viewport();
    let zoom = vp.zoom();
    let (pan_x, pan_y) = vp.pan();
    let inv_zoom = 1.0 / zoom;
    // 放大（>1）且恒等变换才双线性：1:1 双线性反而模糊；
    // 旋转/翻转下双线性邻域会跨瓦片（接缝），统一走最近邻
    let ident = vp.transform_ident();
    let bilinear = zoom > 1.0 && ident;
    let (rot_c, rot_s) = (vp.rotation().cos(), vp.rotation().sin());
    let flip = vp.flip_x();
    // 恒等时的行内增量优化；旋转/翻转退化为每像素全算
    let row_base_x = (0.5 - pan_x) * inv_zoom;
    let step_x = if ident { inv_zoom } else { f64::NAN };

    let all_layers: Vec<&paint_core::layer::Layer> = doc.layers().iter().collect();
    for (li, layer) in all_layers
        .iter()
        .enumerate()
        .filter(|(_, l)| l.visible && l.opacity > 0.0)
    {
        let opacity = layer.opacity.clamp(0.0, 1.0);
        let mode = layer.blend_mode;
        // 剪贴层：约束源 = 下方第一个非剪贴层（Procreate 语义）
        let parent: Option<&paint_core::layer::Layer> = if layer.clipped {
            all_layers[..li]
                .iter()
                .rev()
                .copied()
                .find(|l| !l.clipped && l.visible)
        } else {
            None
        };
        for y in clip.y..(clip.y + clip.h as i32) {
            let py = y as f64 + 0.5 - pan_y;
            let cx_row_ident = row_base_x + clip.x as f64 * step_x;

            // 先按行中心 y 定位瓦片行（恒等优化；旋转时按像素定位）
            let cy_probe = if ident { py * inv_zoom } else { 0.0 };
            let iy0 = if ident { cy_probe.floor() as i64 } else { 0 };
            let ty0 = iy0 >> 8;
            let fy = if ident {
                cy_probe - ((ty0 << 8) as f64)
            } else {
                0.0
            };
            let ly = (iy0 & 255) as usize;
            let row = (y as u32 * width) as usize * 4;

            // 瓦片行内缓存：x 单调递增，瓦片 id 只增不减（恒等时有效；
            // 旋转下 x 单调但 canvas 不单调，退化为每像素查缓存键）
            let mut cache_key = u64::MAX;
            let mut cache: Option<&TileData> = None;

            let mut cx_ident = cx_row_ident;
            for x in clip.x..(clip.x + clip.w as i32) {
                let (cx_cur, cy_cur) = if ident {
                    let v = cx_ident;
                    cx_ident += step_x;
                    (v, cy_probe)
                } else {
                    // f32 全链与 GPU 着色器同精度：最近邻 floor 边界不因
                    // f64/f32 差异翻转到邻 texel（渐变边缘差值会被放大）
                    let px = x as f32 + 0.5 - pan_x as f32;
                    let pyf = py as f32;
                    let rx = rot_c as f32 * px + rot_s as f32 * pyf;
                    let ry = -rot_s as f32 * px + rot_c as f32 * pyf;
                    let fx = if flip { -rx } else { rx };
                    let iz = inv_zoom as f32;
                    ((fx * iz) as f64, (ry * iz) as f64)
                };
                let ix = cx_cur.floor() as i64;
                let tx = ix >> 8;
                let fx = cx_cur - ((tx << 8) as f64); // 瓦片内浮点 x
                                                      // 旋转下 y 每像素变化
                let (ty, fy, ly) = if ident {
                    (ty0, fy, ly)
                } else {
                    let iy = cy_cur.floor() as i64;
                    let t = iy >> 8;
                    (t, cy_cur - ((t << 8) as f64), (iy & 255) as usize)
                };
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

                // 蒙版（1:1 最近邻）：有效 alpha ×= mask.R
                let mask_mul: f32 = if let Some(mask) = &layer.mask {
                    mask_nearest(mask, tx, ty, lx, ly)
                } else {
                    1.0
                };
                if mask_mul <= 0.0 {
                    continue;
                }
                // 剪贴层：× 父层该像素 alpha
                let parent_mul: f32 = if let Some(pl) = parent {
                    parent_alpha_at(pl, tx, ty, lx, ly)
                } else {
                    1.0
                };
                if parent_mul <= 0.0 {
                    continue;
                }
                let extra = mask_mul * parent_mul;

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
                    let op_eff = (opacity as f64 * extra as f64) as f32;
                    if mode == BlendMode::Normal {
                        let inv = 1.0 - sa;
                        target[d] = over_ch(mix(0) * op_eff as f64, target[d] as f64, inv);
                        target[d + 1] = over_ch(mix(1) * op_eff as f64, target[d + 1] as f64, inv);
                        target[d + 2] = over_ch(mix(2) * op_eff as f64, target[d + 2] as f64, inv);
                        target[d + 3] = over_ch(mix(3) * op_eff as f64, target[d + 3] as f64, inv);
                    } else {
                        let q = [
                            (mix(0) * op_eff as f64) as u8,
                            (mix(1) * op_eff as f64) as u8,
                            (mix(2) * op_eff as f64) as u8,
                            (mix(3) * op_eff as f64) as u8,
                        ];
                        composite_pixel(&mut target[d..d + 4], &q, op_eff, mode);
                    }
                } else {
                    let i = ((ly * TILE as usize) + lx) * 4;
                    let sa = p[i + 3] as f64 / 255.0 * opacity as f64 * extra as f64;
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

/// 选区轮廓：选区内且四邻存在选区外的像素染半透明红（最近邻到屏幕）。
/// 仅 CPU 路径绘制（GPU 合成模式下裁剪仍生效，轮廓线省略）。
fn paint_selection_outline(
    doc: &Document,
    sel: &paint_core::tile::TileGrid,
    target: &mut [u8],
    width: u32,
    clip: &Rect,
) {
    let vp = doc.viewport();
    let (rot_c, rot_s) = (vp.rotation().cos(), vp.rotation().sin());
    let flip = vp.flip_x();
    let zoom = vp.zoom();
    let _inv = 1.0 / zoom;
    let (pan_x, pan_y) = vp.pan();
    let sel_at = |cx: i64, cy: i64| -> bool {
        sel.get(paint_core::tile::TileId::at(cx, cy))
            .map(|t| {
                let (ox, oy) = paint_core::tile::TileId::at(cx, cy).origin();
                let i = (((cy - oy) * 256 + (cx - ox)) * 4) as usize;
                t.pixels()[i] > 127
            })
            .unwrap_or(false)
    };
    for (tid, _tile) in sel.iter_entries() {
        let (ox, oy) = tid.origin();
        for y in 0..TILE as i64 {
            for x in 0..TILE as i64 {
                let (cx, cy) = (ox + x, oy + y);
                if !sel_at(cx, cy) {
                    continue;
                }
                // 边界：任一邻点在选区外
                let edge = !(sel_at(cx - 1, cy)
                    && sel_at(cx + 1, cy)
                    && sel_at(cx, cy - 1)
                    && sel_at(cx, cy + 1));
                if !edge {
                    continue;
                }
                // 画布 → 屏幕（最近邻）
                let mut px = cx as f64 * zoom;
                if flip {
                    px = -px;
                }
                let py = cy as f64 * zoom;
                let sx = (rot_c * px - rot_s * py + pan_x) as i32;
                let sy = (rot_s * px + rot_c * py + pan_y) as i32;
                if sx >= clip.x
                    && sx < clip.x + clip.w as i32
                    && sy >= clip.y
                    && sy < clip.y + clip.h as i32
                {
                    let i = (sy as usize * width as usize + sx as usize) * 4;
                    if i + 3 < target.len() {
                        target[i] = 255;
                        target[i + 1] = 60;
                        target[i + 2] = 60;
                        target[i + 3] = 255;
                    }
                }
            }
        }
    }
}

/// 蒙版瓦片 R 通道（无瓦片处 = 1 全显）。
fn mask_nearest(mask: &paint_core::tile::TileGrid, tx: i64, ty: i64, lx: usize, ly: usize) -> f32 {
    mask.get(paint_core::tile::TileId {
        x: tx as i32,
        y: ty as i32,
    })
    .map(|t| t.pixels()[(ly * TILE as usize + lx) * 4] as f32 / 255.0)
    .unwrap_or(1.0)
}

/// 父层该像素 alpha（无内容处 = 0）。
fn parent_alpha_at(pl: &paint_core::layer::Layer, tx: i64, ty: i64, lx: usize, ly: usize) -> f32 {
    pl.tiles
        .get(paint_core::tile::TileId {
            x: tx as i32,
            y: ty as i32,
        })
        .map(|t| t.pixels()[(ly * TILE as usize + lx) * 4 + 3] as f32 / 255.0)
        .unwrap_or(0.0)
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
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
        }];
        let layer = layers.get_mut(lid);
        super::super::stamp::stamp_dabs(
            &mut layer.tiles,
            &dabs,
            None,
            &mut StrokeRecorder::new(lid),
        );
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
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
        }];
        let layer = layers.get_mut(top);
        super::super::stamp::stamp_dabs(
            &mut layer.tiles,
            &dabs,
            None,
            &mut StrokeRecorder::new(top),
        );
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

#[cfg(test)]
mod mask_clip_tests {
    use super::*;
    use paint_core::layer::LayerStack;
    use paint_core::tile::{TileGrid, TileId};

    fn paint_fill(
        layers: &mut LayerStack,
        id: paint_core::LayerId,
        x0: i32,
        y0: i32,
        size: i32,
        c: [u8; 4],
    ) {
        let layer = layers.get_mut(id);
        for y in y0..y0 + size {
            for x in x0..x0 + size {
                let tid = TileId::at(x as i64, y as i64);
                let t = layer.tiles.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let lx = (x - ox as i32) as usize;
                let ly = (y - oy as i32) as usize;
                let i = (ly * 256 + lx) * 4;
                t.pixels_mut()[i..i + 4].copy_from_slice(&c);
            }
        }
    }

    fn frame(doc: &Document, w: u32) -> Vec<u8> {
        let mut f = vec![0u8; (w * w * 4) as usize];
        composite(
            doc,
            &mut f,
            w,
            Rect::new(0, 0, w, w),
            Some(doc.background()),
        );
        f
    }

    fn px(f: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        f[((y * w + x) * 4) as usize..][..4].try_into().unwrap()
    }

    #[test]
    fn mask_hides_pixels() {
        let mut doc = Document::new(usize::MAX);
        doc.set_background(Color::WHITE);
        let l = doc.active_layer();
        paint_fill(doc.layers_mut(), l, 10, 10, 30, [0, 0, 0, 255]);
        let w = 64;
        // 无蒙版：黑块可见
        let f = frame(&doc, w);
        assert_eq!(px(&f, w, 25, 25), [0, 0, 0, 255]);

        // 蒙版：中间 10×10 全显（255），其余黑区置 0
        {
            let layer = doc.layers_mut().get_mut(l);
            let mut mask = TileGrid::new();
            for y in 20..30 {
                for x in 20..30 {
                    let tid = TileId::at(x as i64, y as i64);
                    let t = mask.get_or_create_mut(tid);
                    let (ox, oy) = tid.origin();
                    let i = (((y as i64 - oy) * 256 + (x as i64 - ox)) * 4) as usize;
                    t.pixels_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
            layer.mask = Some(mask);
        }
        let f = frame(&doc, w);
        // 蒙版内仍黑
        assert_eq!(px(&f, w, 25, 25), [0, 0, 0, 255]);
        // 蒙版外被遮 → 白底
        assert_eq!(px(&f, w, 12, 12), [255, 255, 255, 255]);
        // 蒙版无瓦片区域（黑块外的白）不受影响
        assert_eq!(px(&f, w, 60, 60), [255, 255, 255, 255]);
    }

    #[test]
    fn clipped_layer_limited_by_parent() {
        let mut doc = Document::new(usize::MAX);
        doc.set_background(Color::WHITE);
        let base = doc.active_layer();
        paint_fill(doc.layers_mut(), base, 10, 10, 20, [0, 0, 0, 255]); // 底层黑 20×20
        let top = doc.layers_mut().insert(None);
        paint_fill(doc.layers_mut(), top, 20, 20, 30, [255, 0, 0, 255]); // 顶层红 30×30
        doc.layers_mut().get_mut(top).clipped = true;

        let w = 64;
        let f = frame(&doc, w);
        // (25,25)：在底层黑块内 → 剪贴层红可见 → 红
        assert_eq!(px(&f, w, 25, 25), [255, 0, 0, 255]);
        // (45,45)：底层无内容（黑块 10..30）→ 剪贴层被完全约束 → 白底
        assert_eq!(px(&f, w, 45, 45), [255, 255, 255, 255]);
        // 边界处半透明父层（硬边无半透明）：黑块边缘外一步即被剪
        assert_eq!(px(&f, w, 31, 25), [255, 255, 255, 255]);
    }
}
