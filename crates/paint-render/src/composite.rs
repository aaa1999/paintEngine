use paint_core::blend::composite_pixel;
use paint_core::color::Color;
use paint_core::document::Document;
use paint_core::geometry::Rect;
use paint_core::layer::BlendMode;
use paint_core::tile::{TileData, TileId, TILE};

/// 合成可见图层到目标缓冲（RGBA8 预乘）。只处理 `dirty` 区域。
///
/// 采样为最近邻（缩放不模糊、瓦片接缝无缝），双线性/mipmap 为
/// P1 优化项。`background: None` 时目标保持透明（PNG 导出等）。
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
    }

    let vp = doc.viewport();
    let zoom = vp.zoom();
    let (pan_x, pan_y) = vp.pan();
    let inv_zoom = 1.0 / zoom;
    // 每行常数增量：canvas_x = x*inv + row_base_x
    let row_base_x = (0.5 - pan_x) * inv_zoom;
    let step_x = inv_zoom;

    for layer in doc.layers().iter().filter(|l| l.visible && l.opacity > 0.0) {
        let opacity = layer.opacity.clamp(0.0, 1.0);
        let mode = layer.blend_mode;
        for y in clip.y..(clip.y + clip.h as i32) {
            let iy = ((y as f64 + 0.5 - pan_y) * inv_zoom).floor() as i64;
            let ty = iy >> 8; // 瓦片索引（负坐标下算术移位正确）
            let ly = iy & 255; // 瓦片内 y 偏移
            let row = (y as u32 * width) as usize * 4;

            // 瓦片行内缓存：x 单调递增，瓦片 id 只增不减
            let mut cache_key = u64::MAX;
            let mut cache: Option<&TileData> = None;

            let mut cx = row_base_x + clip.x as f64 * step_x;
            for x in clip.x..(clip.x + clip.w as i32) {
                let ix = cx.floor() as i64;
                let tx = ix >> 8;
                let lx = ix & 255; // 瓦片内 x 偏移
                cx += step_x;

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
                let i = ((ly as usize * TILE as usize) + lx as usize) * 4;
                let sa = tile.pixels()[i + 3] as f64 / 255.0 * opacity as f64;
                if sa <= 0.0 {
                    continue;
                }
                let p = tile.pixels();
                let d = row + x as usize * 4;
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

fn over_ch(src_premul: f64, dst_premul: f64, inv: f64) -> u8 {
    (src_premul + dst_premul * inv + 0.5).clamp(0.0, 255.0) as u8
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
