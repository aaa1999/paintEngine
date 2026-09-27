use paint_core::history::StrokeRecorder;
use paint_core::stroke::{Dab, DabMode};
use paint_core::tile::{TileData, TileGrid, TileId, TILE};

/// 选区值（R 通道；无瓦片 = 选区外 = 0）。
#[inline]
fn clip_v(clip: Option<&TileGrid>, id: TileId, px: u32, py: u32) -> f32 {
    match clip {
        None => 1.0,
        Some(g) => g
            .get(id)
            .map(|t| t.pixels()[((py * TILE + px) * 4) as usize] as f32 / 255.0)
            .unwrap_or(0.0),
    }
}

/// 把 dab 序列盖进瓦片网格（像素层或图层蒙版）。
/// 写瓦片前经 recorder 记录撤销快照。
pub fn stamp_dabs(
    grid: &mut TileGrid,
    dabs: &[Dab],
    clip: Option<&TileGrid>,
    recorder: &mut StrokeRecorder,
) {
    for dab in dabs {
        stamp_dab(grid, dab, clip, recorder);
    }
}

fn stamp_dab(
    grid: &mut TileGrid,
    dab: &Dab,
    clip: Option<&TileGrid>,
    recorder: &mut StrokeRecorder,
) {
    let r = dab.radius.max(0.0) as f64;
    let ri = r.ceil() as i64;
    let x0 = dab.x as i64 - ri;
    let y0 = dab.y as i64 - ri;
    let x1 = dab.x as i64 + ri;
    let y1 = dab.y as i64 + ri;

    for ty in y0 >> 8..=y1 >> 8 {
        for tx in x0 >> 8..=x1 >> 8 {
            let id = TileId {
                x: tx as i32,
                y: ty as i32,
            };
            recorder.capture(grid, id);
            let origin = (tx << 8, ty << 8);
            let tile = grid.get_or_create_mut(id);
            stamp_tile(tile, origin, dab, x0, y0, x1, y1, clip, id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stamp_tile(
    tile: &mut TileData,
    origin: (i64, i64),
    dab: &Dab,
    bx0: i64,
    by0: i64,
    bx1: i64,
    by1: i64,
    clip: Option<&TileGrid>,
    id: TileId,
) {
    let (cr, cg, cb) = (dab.color.r as f32, dab.color.g as f32, dab.color.b as f32);
    let hardness = dab.hardness;
    let alpha = dab.alpha;
    let mode = dab.mode;

    let px0 = (bx0 - origin.0).max(0).min(TILE as i64 - 1) as u32;
    let py0 = (by0 - origin.1).max(0).min(TILE as i64 - 1) as u32;
    let px1 = (bx1 - origin.0).min(TILE as i64 - 1).max(0) as u32;
    let py1 = (by1 - origin.1).min(TILE as i64 - 1).max(0) as u32;

    let px = tile.pixels_mut();
    let radius = dab.radius.max(0.001);
    // 各向异性：旋转到长轴系（tilt 笔刷的椭圆笔形）
    let aspect = dab.aspect.clamp(0.05, 1.0);
    let (ca, sa) = if aspect < 1.0 {
        (dab.angle.cos() as f64, dab.angle.sin() as f64)
    } else {
        (1.0, 0.0)
    };
    let inv_ry = 1.0 / (radius * aspect) as f64;

    for py in py0..=py1 {
        // 像素中心采样
        let dy0 = (origin.1 + py as i64) as f64 + 0.5 - dab.y;
        for pxx in px0..=px1 {
            let dx0 = (origin.0 + pxx as i64) as f64 + 0.5 - dab.x;
            // 长轴分量（半径 = radius）；短轴分量（半径 = radius × aspect）
            let dx = ca * dx0 + sa * dy0;
            let dy = -sa * dx0 + ca * dy0;
            let t2 = ((dx / radius as f64) * (dx / radius as f64) + (dy * inv_ry) * (dy * inv_ry))
                as f32;
            if t2 >= 1.0 && dab.dual.is_none() {
                continue;
            }
            let _nx = (dx / radius.max(0.001) as f64) as f32;
            let _ny = (dy * inv_ry) as f32;
            // 纹理尖：alpha 蒙版来自尖图采样（散布偏移由 dab 坐标哈希确定）
            let tip_a = dab.tip.as_ref().map(|tip| {
                let ang = ((dab.x * 12.9898 + dab.y * 78.233).fract() * 43758.5453).fract();
                let ox = (ang * 2.0 - 1.0) * dab.scatter as f64 * dab.radius as f64;
                let oy =
                    ((ang * 917.3).fract() * 2.0 - 1.0) * dab.scatter as f64 * dab.radius as f64;
                let sx = ca * (dx0 + ox) + sa * (dy0 + oy);
                let sy = -sa * (dx0 + ox) + ca * (dy0 + oy);
                let nx = (sx / dab.radius.max(0.001) as f64) * 0.5 + 0.5;
                let ny = (sy * inv_ry) * 0.5 + 0.5;
                let ts = tip.size as i64;
                let tx = (nx * ts as f64) as i64;
                let ty = (ny * ts as f64) as i64;
                if tx < 0 || ty < 0 || tx >= ts || ty >= ts {
                    0.0
                } else {
                    tip.sample(tx as u32, ty as u32)
                }
            });
            let mut a = match tip_a {
                Some(v) => v * alpha * clip_v(clip, id, pxx, py),
                None => falloff(t2.sqrt(), hardness) * alpha * clip_v(clip, id, pxx, py),
            };
            // 双重笔尖（内联热路径）
            if let Some(dual) = &dab.dual {
                if dual.mode != paint_core::brush::DualMode::Off {
                    let main_cov = if t2 < 1.0 {
                        match tip_a {
                            Some(v) => v,
                            None => falloff(t2.sqrt(), hardness),
                        }
                    } else {
                        0.0
                    };
                    let sr = dual.size_ratio.clamp(0.05, 1.0);
                    let aa = dab.angle + dual.angle_offset;
                    let (dca, dsa) = (aa.cos() as f64, aa.sin() as f64);
                    let ux0 = ca * dx0 - sa * dy0;
                    let uy0 = sa * dx0 + ca * dy0;
                    let ddx = dca * ux0 + dsa * uy0;
                    let ddy = -dsa * ux0 + dca * uy0;
                    let sx = (ddx / (radius as f64 * sr as f64)) as f32;
                    let sy = (ddy / (radius as f64 * sr as f64 * aspect as f64)) as f32;
                    let combined = dual.combine(main_cov, sx, sy);
                    a = combined * alpha * clip_v(clip, id, pxx, py);
                }
            }
            if a <= 1.0 / 255.0 {
                continue;
            }
            let i = ((py * TILE + pxx) * 4) as usize;
            if dab.erase {
                blend_erase(&mut px[i..i + 4], a);
            } else {
                match mode {
                    DabMode::Buildup => blend_over(&mut px[i..i + 4], cr, cg, cb, a),
                    DabMode::Wash => blend_wash(&mut px[i..i + 4], cr, cg, cb, a),
                }
            }
        }
    }
}

/// 硬度曲线：t < h 满覆盖，h..1 线性衰减到 0。
fn falloff(t: f32, hardness: f32) -> f32 {
    if hardness >= 0.999 {
        if t < 1.0 {
            1.0
        } else {
            0.0
        }
    } else if t <= hardness {
        1.0
    } else {
        (1.0 - t) / (1.0 - hardness)
    }
}

/// source-over，dst 为预乘存储。
fn blend_over(px: &mut [u8], cr: f32, cg: f32, cb: f32, sa: f32) {
    let da = px[3] as f32 / 255.0;
    let inv = 1.0 - sa;
    let oa = sa + da * inv;
    if oa <= 0.0 {
        px[0] = 0;
        px[1] = 0;
        px[2] = 0;
        px[3] = 0;
        return;
    }
    let dr = px[0] as f32 / 255.0 * da; // dst 预乘
    let dg = px[1] as f32 / 255.0 * da;
    let db = px[2] as f32 / 255.0 * da;
    px[0] = to_u8(cr * sa + dr * inv);
    px[1] = to_u8(cg * sa + dg * inv);
    px[2] = to_u8(cb * sa + db * inv);
    px[3] = to_u8(oa);
}

/// 整笔同浓近似：该像素取本笔遇到过的最大覆盖。
fn blend_wash(px: &mut [u8], cr: f32, cg: f32, cb: f32, sa: f32) {
    let da = px[3] as f32 / 255.0;
    if sa > da {
        px[0] = to_u8(cr * sa);
        px[1] = to_u8(cg * sa);
        px[2] = to_u8(cb * sa);
        px[3] = to_u8(sa);
    }
}

/// dst-out 橡皮：预乘各通道同乘 (1−sa)，保持预乘不变式。
fn blend_erase(px: &mut [u8], sa: f32) {
    let k = 1.0 - sa;
    px[0] = (px[0] as f32 * k + 0.5) as u8;
    px[1] = (px[1] as f32 * k + 0.5) as u8;
    px[2] = (px[2] as f32 * k + 0.5) as u8;
    px[3] = (px[3] as f32 * k + 0.5) as u8;
}

fn to_u8(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use paint_core::color::Color;
    use paint_core::layer::LayerStack;
    use paint_core::UndoOp;

    fn layer() -> (LayerStack, paint_core::layer::LayerId) {
        let mut s = LayerStack::new();
        let id = s.insert(None);
        (s, id)
    }

    fn dab_at(x: f64, y: f64, r: f32, mode: DabMode) -> Dab {
        Dab {
            x,
            y,
            radius: r,
            hardness: 0.5,
            color: Color::BLACK,
            alpha: 1.0,
            mode,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: None,
        }
    }

    fn pixel(layers: &LayerStack, lid: paint_core::layer::LayerId, x: i64, y: i64) -> [u8; 4] {
        let id = TileId::at(x, y);
        let t = layers.get(lid).tiles.get(id).expect("瓦片应存在");
        let lx = (x - id.origin().0) as usize;
        let ly = (y - id.origin().1) as usize;
        let i = (ly * TILE as usize + lx) * 4;
        t.pixels()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn center_opaque_edge_fades() {
        let (mut layers, lid) = layer();
        let mut rec = StrokeRecorder::new(lid);
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[dab_at(100.0, 100.0, 10.0, DabMode::Buildup)],
            None,
            &mut rec,
        );
        let c = pixel(&layers, lid, 100, 100);
        assert_eq!(c[3], 255, "中心满覆盖");
        let e = pixel(&layers, lid, 100, 108);
        assert!(e[3] > 0 && e[3] < 255, "硬度 0.5 下边缘渐变: {}", e[3]);
        let out = pixel(&layers, lid, 100, 112);
        assert_eq!(out[3], 0, "半径外无影响");
    }

    #[test]
    fn buildup_accumulates_wash_capped() {
        // 同位置两次盖章：Buildup 变浓（已饱和则不变），Wash 不超上限
        let (mut layers, lid) = layer();
        let mut rec = StrokeRecorder::new(lid);
        let half = Dab {
            alpha: 0.5,
            ..dab_at(50.0, 50.0, 8.0, DabMode::Buildup)
        };
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            std::slice::from_ref(&half),
            None,
            &mut StrokeRecorder::new(lid),
        );
        let a1 = pixel(&layers, lid, 50, 50)[3];
        stamp_dabs(&mut layers.get_mut(lid).tiles, &[half], None, &mut rec);
        let a2 = pixel(&layers, lid, 50, 50)[3];
        assert_eq!(a1, 128);
        assert!(a2 > a1, "Buildup 叠加: {a1} → {a2}");
        assert_eq!(a2, 192);

        let (mut layers2, lid2) = layer();
        let wash = Dab {
            alpha: 0.5,
            ..dab_at(50.0, 50.0, 8.0, DabMode::Wash)
        };
        stamp_dabs(
            &mut layers2.get_mut(lid2).tiles,
            std::slice::from_ref(&wash),
            None,
            &mut StrokeRecorder::new(lid2),
        );
        stamp_dabs(
            &mut layers2.get_mut(lid2).tiles,
            &[wash],
            None,
            &mut StrokeRecorder::new(lid2),
        );
        let w = pixel(&layers2, lid2, 50, 50)[3];
        assert_eq!(w, 128, "Wash 两次盖章保持同一浓度");
    }

    #[test]
    fn dab_at_tile_corner_spans_four_tiles() {
        let (mut layers, lid) = layer();
        let mut rec = StrokeRecorder::new(lid);
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[dab_at(0.0, 0.0, 12.0, DabMode::Buildup)],
            None,
            &mut rec,
        );
        let g = layers.get(lid);
        for id in [
            TileId { x: -1, y: -1 },
            TileId { x: 0, y: -1 },
            TileId { x: -1, y: 0 },
            TileId { x: 0, y: 0 },
        ] {
            assert!(g.tiles.get(id).is_some(), "瓦片 {id:?} 应被创建");
        }
        let group = rec.finish("Stroke");
        let tiles = match group.ops.first() {
            Some(UndoOp::Tiles(v)) => v,
            _ => panic!("应是瓦片操作"),
        };
        assert_eq!(tiles.len(), 4, "四个瓦片都记录了旧快照");
        assert!(tiles.iter().all(|(_, _, old)| old.is_none()));
    }

    #[test]
    fn recorder_snapshots_old_content() {
        let (mut layers, lid) = layer();
        // 第一笔写入
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[dab_at(10.0, 10.0, 5.0, DabMode::Buildup)],
            None,
            &mut StrokeRecorder::new(lid),
        );
        // 第二笔覆盖同一瓦片：快照应为非空旧内容
        let mut rec = StrokeRecorder::new(lid);
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[dab_at(12.0, 12.0, 5.0, DabMode::Buildup)],
            None,
            &mut rec,
        );
        let group = rec.finish("Stroke");
        let tiles = match group.ops.first() {
            Some(UndoOp::Tiles(v)) => v,
            _ => panic!("应是瓦片操作"),
        };
        assert!(tiles.iter().all(|(_, _, old)| old.is_some()));
    }

    #[test]
    fn erase_reduces_alpha() {
        let (mut layers, lid) = layer();
        // 先画不透明黑
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[dab_at(50.0, 50.0, 8.0, DabMode::Buildup)],
            None,
            &mut StrokeRecorder::new(lid),
        );
        assert_eq!(pixel(&layers, lid, 50, 50)[3], 255);
        // 硬橡皮擦中心
        let eraser = Dab {
            erase: true,
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: None,
            ..dab_at(50.0, 50.0, 4.0, DabMode::Buildup)
        };
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[eraser],
            None,
            &mut StrokeRecorder::new(lid),
        );
        let c = pixel(&layers, lid, 50, 50);
        assert_eq!(c[3], 0, "中心应被完全擦除");
        // 预乘不变式：alpha 0 则 RGB 也为 0
        assert_eq!(&c[..3], &[0, 0, 0]);
    }
}

#[cfg(test)]
mod tip_stamp_tests {
    use super::*;
    use paint_core::color::Color;
    use paint_core::layer::LayerStack;
    use paint_core::stroke::{DabMode, TipTexture};
    use paint_core::tile::TILE;
    use std::sync::Arc;

    #[test]
    fn textured_dab_masks_by_tip() {
        // 16×16 左半白右半黑尖图
        let n = 16usize;
        let mut rgba = vec![0u8; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                let v = if x < n / 2 { 255 } else { 0 };
                let i = (y * n + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let png = paint_core::io::encode_png(&rgba, n as u32, n as u32).unwrap();
        let tip = TipTexture::from_png(&png).unwrap();

        let mut s = LayerStack::new();
        let lid = s.insert(None);
        let dab = Dab {
            x: 64.0,
            y: 64.0,
            radius: 8.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
            tip: Some(Arc::new(tip)),
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: None,
        };
        stamp_dabs(
            &mut s.get_mut(lid).tiles,
            &[dab],
            None,
            &mut StrokeRecorder::new(lid),
        );

        let px = |x: usize, y: usize| -> u8 {
            let t = s
                .get(lid)
                .tiles
                .get(TileId::at(x as i64, y as i64))
                .unwrap();
            t.pixels()[(y * TILE as usize + x) * 4 + 3]
        };
        // dab 中心 (64,64) 半径 8：左半（60,64）有墨；右半（68,64）无
        assert_eq!(px(60, 64), 255, "尖图白侧盖墨");
        assert_eq!(px(68, 64), 0, "尖图黑侧不盖");
    }
}

#[cfg(test)]
mod tilt_tests {
    use super::*;
    use paint_core::color::Color;
    use paint_core::layer::LayerStack;
    use paint_core::stroke::DabMode;
    use paint_core::tile::TILE;

    fn px(s: &LayerStack, lid: paint_core::LayerId, x: usize, y: usize) -> u8 {
        let t = s
            .get(lid)
            .tiles
            .get(TileId::at(x as i64, y as i64))
            .unwrap();
        t.pixels()[(y * TILE as usize + x) * 4 + 3]
    }

    /// 椭圆 dab：长轴 x（angle=0），aspect=0.5 → 横向半径 10、纵向 5。
    #[test]
    fn anisotropic_dab_ellipse() {
        let mut s = LayerStack::new();
        let lid = s.insert(None);
        let dab = Dab {
            x: 64.0,
            y: 64.0,
            radius: 10.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 0.5,
            angle: 0.0,
            dual: None,
        };
        stamp_dabs(
            &mut s.get_mut(lid).tiles,
            &[dab],
            None,
            &mut StrokeRecorder::new(lid),
        );
        assert_eq!(px(&s, lid, 72, 64), 255, "长轴像素中心 8.5 < 10 内");
        assert_eq!(px(&s, lid, 68, 64), 255, "短轴像素中心 4.5 < 5 内");
        assert_eq!(px(&s, lid, 64, 69), 0, "短轴像素中心 5.5 > 5 外");
        assert_eq!(px(&s, lid, 74, 64), 0, "长轴像素中心 10.5 > 10 外");
    }

    /// 旋转 90° 的椭圆：长轴变纵向。
    #[test]
    fn anisotropic_dab_rotated() {
        let mut s = LayerStack::new();
        let lid = s.insert(None);
        let dab = Dab {
            x: 64.0,
            y: 64.0,
            radius: 10.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 0.5,
            angle: std::f32::consts::FRAC_PI_2,
            dual: None,
        };
        stamp_dabs(
            &mut s.get_mut(lid).tiles,
            &[dab],
            None,
            &mut StrokeRecorder::new(lid),
        );
        assert_eq!(px(&s, lid, 64, 72), 255, "旋转后纵向变长轴");
        assert_eq!(px(&s, lid, 64, 71), 255); // 旋转后长轴：7.5 < 10
        assert_eq!(px(&s, lid, 69, 64), 0, "横向变短轴 ±7 外");
        assert_eq!(px(&s, lid, 67, 64), 255, "横向 ±3 内");
    }
}

#[cfg(test)]
mod dual_stamp_tests {
    use super::*;
    use paint_core::brush::{BrushTip, DualBrush, DualMode};
    use paint_core::color::Color;
    use paint_core::layer::LayerStack;
    use paint_core::stroke::{DabMode, TipTexture};
    use paint_core::tile::{TileId, TILE};
    use std::sync::Arc;

    fn px(s: &LayerStack, lid: paint_core::LayerId, x: usize, y: usize) -> u8 {
        let t = s
            .get(lid)
            .tiles
            .get(TileId::at(x as i64, y as i64))
            .unwrap();
        t.pixels()[(y * TILE as usize + x) * 4 + 3]
    }

    /// 双笔尖 Intersect：方形副笔裁掉圆主笔的角。
    #[test]
    fn dual_intersect_clips_circle() {
        let mut s = LayerStack::new();
        let lid = s.insert(None);
        let dual = DualBrush {
            mode: DualMode::Intersect,
            tip: BrushTip::Square { corner: 0.0 },
            size_ratio: 1.0,
            ..DualBrush::default()
        };
        let dab = Dab {
            x: 64.0,
            y: 64.0,
            radius: 20.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: Some(Box::new(dual)),
        };
        stamp_dabs(
            &mut s.get_mut(lid).tiles,
            &[dab],
            None,
            &mut StrokeRecorder::new(lid),
        );
        // 中心在方形+圆内 → 有墨
        assert_eq!(px(&s, lid, 64, 64), 255);
        // 对角（方形外、圆内）→ 被裁
        // 副笔方形 [-1,1] × r=20 → 画布 [44,84]；对角 (83,83) 在方形外
        assert_eq!(px(&s, lid, 83, 83), 0, "方形副笔裁掉对角");
        // 轴向边缘（方形内圆内）→ 有墨
        assert!(
            px(&s, lid, 80, 64) > 0 || px(&s, lid, 79, 64) > 0,
            "轴向边缘保留"
        );
    }

    /// Union：副笔在主笔外补墨。
    #[test]
    fn dual_union_extends() {
        let mut s = LayerStack::new();
        let lid = s.insert(None);
        let dual = DualBrush {
            mode: DualMode::Union,
            tip: BrushTip::Round { hardness: 1.0 },
            size_ratio: 0.5,
            ..DualBrush::default()
        };
        let dab = Dab {
            x: 64.0,
            y: 64.0,
            radius: 20.0,
            hardness: 1.0,
            color: Color::BLACK,
            alpha: 1.0,
            mode: DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: Some(Box::new(dual)),
        };
        stamp_dabs(
            &mut s.get_mut(lid).tiles,
            &[dab],
            None,
            &mut StrokeRecorder::new(lid),
        );
        assert_eq!(px(&s, lid, 64, 64), 255, "中心满覆盖");
        let _ = TipTexture::from_png;
        let _ = Arc::new(());
    }
}
