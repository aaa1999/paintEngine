//! 滤镜系统：对活动图层（或选区内）像素应用效果。
//!
//! 框架两级：逐像素滤镜（亮度/色相饱和——无邻域依赖）与
//! 区域滤镜（高斯模糊——需要邻域缓冲）。统一从瓦片提取像素
//! → 处理 → 写回，撤销走 StrokeRecorder 快照。

use crate::history::StrokeRecorder;
use crate::layer::{Layer, LayerId};
use crate::tile::{TileGrid, TileId, TILE};

/// 滤镜参数。
#[derive(Debug, Clone)]
pub enum Filter {
    /// 高斯模糊（box blur ×3 近似）。radius 1-50。
    Blur { radius: u32 },
    /// 亮度/对比度。brightness -100..100, contrast -100..100。
    BrightnessContrast { brightness: f32, contrast: f32 },
    /// 色相/饱和度/明度。hue -180..180, saturation -100..100, lightness -100..100。
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    /// 反色。
    Invert,
    /// 灰度。
    Grayscale,
}

impl Filter {
    pub fn name(&self) -> &'static str {
        match self {
            Filter::Blur { .. } => "模糊",
            Filter::BrightnessContrast { .. } => "亮度/对比度",
            Filter::HueSaturation { .. } => "色相/饱和度",
            Filter::Invert => "反色",
            Filter::Grayscale => "灰度",
        }
    }
}

/// 选区内提取 + 滤镜 + 写回（撤销采集）。
pub fn apply_filter(
    layer: &mut Layer,
    _lid: LayerId,
    filter: &Filter,
    selection: Option<&TileGrid>,
    recorder: &mut StrokeRecorder,
) {
    // 确定处理范围：选区 bbox 或图层内容 bbox
    // 模糊需要扩展 bbox 以捕获周围透明像素（否则边缘不扩散）
    let blur_pad = match filter {
        Filter::Blur { radius } => (*radius).min(32) as i32,
        _ => 0,
    };
    let bbox = match selection {
        Some(sel) => sel.content_bounds(),
        None => layer.tiles.content_bounds_precise(),
    };
    let Some(mut bbox) = bbox else { return };
    if blur_pad > 0 {
        bbox = crate::geometry::Rect::new(
            bbox.x - blur_pad,
            bbox.y - blur_pad,
            bbox.w + (blur_pad * 2) as u32,
            bbox.h + (blur_pad * 2) as u32,
        );
    }

    // 提取像素到连续缓冲（直行域处理更直观，最后转回预乘）
    let (w, h) = (bbox.w, bbox.h);
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut mask = vec![0u8; (w as usize) * (h as usize)]; // 选区蒙版（255=处理）
    let mut affected_tiles = Vec::new();

    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let (cx, cy) = (bbox.x as i64 + x, bbox.y as i64 + y);
            let tid = TileId::at(cx, cy);
            // 选区检查
            let sel_v = match selection {
                Some(sel) => sel
                    .get(tid)
                    .map(|t| {
                        let (ox, oy) = tid.origin();
                        let lx = (cx - ox) as usize;
                        let ly = (cy - oy) as usize;
                        t.pixels()[(ly * TILE as usize + lx) * 4]
                    })
                    .unwrap_or(0),
                None => 255,
            };
            mask[y as usize * w as usize + x as usize] = sel_v;
            if sel_v == 0 {
                continue;
            }
            if let Some(t) = layer.tiles.get(tid) {
                let (ox, oy) = tid.origin();
                let lx = (cx - ox) as usize;
                let ly = (cy - oy) as usize;
                let src = (ly * TILE as usize + lx) * 4;
                let dst = (y as usize * w as usize + x as usize) * 4;
                let p = t.pixels();
                // 预乘 → 直行
                let a = p[src + 3] as u32;
                if a == 0 {
                    continue;
                }
                for k in 0..3 {
                    buf[dst + k] = ((p[src + k] as u32 * 255 + a / 2) / a).min(255) as u8;
                }
                buf[dst + 3] = a as u8;
            }
            if !affected_tiles.contains(&tid) {
                affected_tiles.push(tid);
            }
        }
    }

    // 撤销采集
    for tid in &affected_tiles {
        recorder.capture(&layer.tiles, *tid);
    }

    // 应用滤镜
    match filter {
        Filter::Blur { radius } => blur(&mut buf, w, h, *radius),
        Filter::BrightnessContrast {
            brightness,
            contrast,
        } => brightness_contrast(&mut buf, w, h, *brightness, *contrast),
        Filter::HueSaturation {
            hue,
            saturation,
            lightness,
        } => hue_saturation(&mut buf, w, h, *hue, *saturation, *lightness),
        Filter::Invert => invert(&mut buf, w, h),
        Filter::Grayscale => grayscale(&mut buf, w, h),
    }

    // 写回（直行 → 预乘，选区混合）
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let idx = (y as usize * w as usize + x as usize) * 4;
            let sel_v = mask[y as usize * w as usize + x as usize] as f32 / 255.0;
            if sel_v == 0.0 {
                continue;
            }
            let (cx, cy) = (bbox.x as i64 + x, bbox.y as i64 + y);
            let tid = TileId::at(cx, cy);
            let tile = layer.tiles.get_or_create_mut(tid);
            let (ox, oy) = tid.origin();
            let lx = (cx - ox) as usize;
            let ly = (cy - oy) as usize;
            let dst = (ly * TILE as usize + lx) * 4;
            let tp = tile.pixels_mut();
            // 直行 → 预乘
            let a = buf[idx + 3] as u32;
            if a == 0 {
                continue; // 透明像素不写回（保持稀疏语义）
            }
            let r = ((buf[idx] as u32 * a + 127) / 255) as u8;
            let g = ((buf[idx + 1] as u32 * a + 127) / 255) as u8;
            let b = ((buf[idx + 2] as u32 * a + 127) / 255) as u8;
            // 选区边缘混合：sel_v < 1 时与原像素线性插值
            if sel_v < 1.0 {
                let sa = a as f32 * sel_v;
                let da = tp[dst + 3] as f32;
                let oa = sa + da * (1.0 - sa);
                if oa > 0.0 {
                    for (k, nv) in [r, g, b, a as u8].iter().enumerate() {
                        let sv = *nv as f32 * sel_v;
                        tp[dst + k] = (sv + tp[dst + k] as f32 * (1.0 - sel_v)) as u8;
                    }
                }
            } else {
                tp[dst] = r;
                tp[dst + 1] = g;
                tp[dst + 2] = b;
                tp[dst + 3] = a as u8;
            }
        }
    }
    layer.tiles.prune();
}

// ── 逐像素滤镜 ──

fn invert(buf: &mut [u8], _w: u32, _h: u32) {
    for px in buf.as_chunks_mut::<4>().0 {
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
    }
}

fn grayscale(buf: &mut [u8], _w: u32, _h: u32) {
    for px in buf.as_chunks_mut::<4>().0 {
        let v = (px[0] as u32 * 30 + px[1] as u32 * 59 + px[2] as u32 * 11) / 100;
        px[0] = v as u8;
        px[1] = v as u8;
        px[2] = v as u8;
    }
}

fn brightness_contrast(buf: &mut [u8], _w: u32, _h: u32, brightness: f32, contrast: f32) {
    let b = brightness * 255.0 / 100.0;
    let c = 1.0 + contrast / 100.0;
    for px in buf.as_chunks_mut::<4>().0 {
        for c_val in px.iter_mut().take(3) {
            let v = *c_val as f32;
            let v = (v - 128.0) * c + 128.0 + b;
            *c_val = v.clamp(0.0, 255.0) as u8;
        }
    }
}

fn hue_saturation(buf: &mut [u8], _w: u32, _h: u32, hue: f32, saturation: f32, lightness: f32) {
    let h_shift = hue * std::f32::consts::PI / 180.0;
    let s_mul = 1.0 + saturation / 100.0;
    let l_add = lightness / 100.0;
    for px in buf.as_chunks_mut::<4>().0 {
        let (r, g, b) = (
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        );
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let l = (max + min) * 0.5;
        let (mut h, mut s) = (0.0f32, 0.0f32);
        if max != min {
            let d = max - min;
            s = if l > 0.5 {
                d / (2.0 - max - min)
            } else {
                d / (max + min)
            };
            h = if max == r {
                (g - b) / d + if g < b { 6.0 } else { 0.0 }
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
            h /= 6.0;
        }
        h = (h + h_shift / std::f32::consts::TAU).fract();
        s = (s * s_mul).clamp(0.0, 1.0);
        let l2 = (l + l_add).clamp(0.0, 1.0);
        // HSL → RGB
        let (r2, g2, b2) = if s == 0.0 {
            (l2, l2, l2)
        } else {
            let q = if l2 < 0.5 {
                l2 * (1.0 + s)
            } else {
                l2 + s - l2 * s
            };
            let p = 2.0 * l2 - q;
            let hue_to_rgb = |t: f32| {
                let mut t = t;
                if t < 0.0 {
                    t += 1.0;
                }
                if t > 1.0 {
                    t -= 1.0;
                }
                if t < 1.0 / 6.0 {
                    p + (q - p) * 6.0 * t
                } else if t < 1.0 / 2.0 {
                    q
                } else if t < 2.0 / 3.0 {
                    p + (q - p) * (2.0 / 3.0 - t) * 6.0
                } else {
                    p
                }
            };
            (
                hue_to_rgb(h + 1.0 / 3.0),
                hue_to_rgb(h),
                hue_to_rgb(h - 1.0 / 3.0),
            )
        };
        px[0] = (r2 * 255.0 + 0.5) as u8;
        px[1] = (g2 * 255.0 + 0.5) as u8;
        px[2] = (b2 * 255.0 + 0.5) as u8;
    }
}

// ── 区域滤镜（邻域依赖）──

/// Box blur ×3 近似高斯。分离水平/垂直两趟。
fn blur(buf: &mut [u8], w: u32, h: u32, radius: u32) {
    let r = radius.clamp(1, 50) as usize;
    let (w, h) = (w as usize, h as usize);
    let mut tmp = buf.to_vec();
    // 3 趟 box blur
    for _ in 0..3 {
        // 水平
        box_blur_h(buf, &mut tmp, w, h, r);
        // 垂直
        box_blur_v(&tmp, buf, w, h, r);
    }
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let win = 2 * r + 1;
    for y in 0..h {
        for c in 0..4 {
            let mut sum = 0u32;
            // 初始窗口
            for dx in 0..win {
                let x = dx.saturating_sub(r).min(w - 1);
                sum += src[(y * w + x) * 4 + c] as u32;
            }
            for x in 0..w {
                dst[(y * w + x) * 4 + c] = (sum / win as u32) as u8;
                // 滑动窗口
                let out_x = x.saturating_sub(r);
                let in_x = (x + r + 1).min(w - 1);
                sum -= src[(y * w + out_x) * 4 + c] as u32;
                sum += src[(y * w + in_x) * 4 + c] as u32;
            }
        }
    }
}

fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let win = 2 * r + 1;
    for x in 0..w {
        for c in 0..4 {
            let mut sum = 0u32;
            for dy in 0..win {
                let y = dy.saturating_sub(r).min(h - 1);
                sum += src[(y * w + x) * 4 + c] as u32;
            }
            for y in 0..h {
                dst[(y * w + x) * 4 + c] = (sum / win as u32) as u8;
                let out_y = y.saturating_sub(r);
                let in_y = (y + r + 1).min(h - 1);
                sum -= src[(out_y * w + x) * 4 + c] as u32;
                sum += src[(in_y * w + x) * 4 + c] as u32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::LayerStack;

    fn make_layer_with_rect() -> (LayerStack, LayerId) {
        let mut s = LayerStack::new();
        let id = s.insert(None);
        let l = s.get_mut(id);
        for y in 30..34 {
            for x in 30..34 {
                let tid = TileId::at(x as i64, y as i64);
                let t = l.tiles.get_or_create_mut(tid);
                let (ox, oy) = tid.origin();
                let i = (((y as i64 - oy) * 256 + (x as i64 - ox)) * 4) as usize;
                t.pixels_mut()[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        (s, id)
    }

    #[test]
    fn blur_softens_edges() {
        let (mut s, lid) = make_layer_with_rect();
        let mut rec = StrokeRecorder::new(lid);
        let layer = s.get_mut(lid);
        apply_filter(layer, lid, &Filter::Blur { radius: 3 }, None, &mut rec);
        // 中心 (32,32) alpha 应从 255 降低（边缘扩散为半透明）
        let tid = TileId::at(32, 32);
        let t = s.get(lid).tiles.get(tid).unwrap();
        let (ox, oy) = tid.origin();
        let i = (((32 - oy as i32) as usize * 256) + (32 - ox as i32) as usize) * 4;
        let a = t.pixels()[i + 3];
        assert!(a > 0 && a < 255, "模糊后中心 alpha: {a}");
        // 矩形外一点（原透明）应有非零 alpha（模糊扩散）
        let tid2 = TileId::at(36, 32);
        if let Some(t2) = s.get(lid).tiles.get(tid2) {
            let i2 = (((32 - oy as i32) as usize * 256) + (36 - ox as i32) as usize) * 4;
            let a2 = t2.pixels()[i2 + 3];
            assert!(a2 > 0, "扩散区域 alpha: {a2}");
        }
    }

    #[test]
    fn invert_flips() {
        let (mut s, lid) = make_layer_with_rect();
        let mut rec = StrokeRecorder::new(lid);
        let layer = s.get_mut(lid);
        apply_filter(layer, lid, &Filter::Invert, None, &mut rec);
        let tid = TileId::at(32, 32);
        let t = s.get(lid).tiles.get(tid).unwrap();
        let (ox, oy) = tid.origin();
        let i = (((32 - oy as i32) as usize * 256) + (32 - ox as i32) as usize) * 4;
        assert_eq!(t.pixels()[i], 255, "黑反色→白");
    }

    #[test]
    fn brightness_increases() {
        let (mut s, lid) = make_layer_with_rect();
        let mut rec = StrokeRecorder::new(lid);
        let layer = s.get_mut(lid);
        apply_filter(
            layer,
            lid,
            &Filter::BrightnessContrast {
                brightness: 50.0,
                contrast: 0.0,
            },
            None,
            &mut rec,
        );
        // 黑色区域 +50 亮度 → 灰（预乘域：a=255 时 R ≈ 128）
        let tid2 = TileId::at(32, 32);
        let t2 = s.get(lid).tiles.get(tid2).unwrap();
        let (ox2, oy2) = tid2.origin();
        let i2 = (((32 - oy2 as i32) as usize * 256) + (32 - ox2 as i32) as usize) * 4;
        assert!(
            t2.pixels()[i2] > 80,
            "黑+50 亮度变灰: R={} A={}",
            t2.pixels()[i2],
            t2.pixels()[i2 + 3]
        );
    }
}
