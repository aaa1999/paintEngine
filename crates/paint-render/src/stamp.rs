use paint_core::history::StrokeRecorder;
use paint_core::stroke::{Dab, DabMode};
use paint_core::tile::{TileData, TileGrid, TileId, TILE};

/// 把 dab 序列盖进瓦片网格（像素层或图层蒙版）。
/// 写瓦片前经 recorder 记录撤销快照。
pub fn stamp_dabs(grid: &mut TileGrid, dabs: &[Dab], recorder: &mut StrokeRecorder) {
    for dab in dabs {
        stamp_dab(grid, dab, recorder);
    }
}

fn stamp_dab(grid: &mut TileGrid, dab: &Dab, recorder: &mut StrokeRecorder) {
    let r = dab.radius.max(0.0) as f64;
    let ri = r.ceil() as i64;
    let x0 = dab.x as i64 - ri;
    let y0 = dab.y as i64 - ri;
    let x1 = dab.x as i64 + ri;
    let y1 = dab.y as i64 + ri;

    let r2 = (r * r) as f32;
    for ty in y0 >> 8..=y1 >> 8 {
        for tx in x0 >> 8..=x1 >> 8 {
            let id = TileId {
                x: tx as i32,
                y: ty as i32,
            };
            recorder.capture(grid, id);
            let origin = (tx << 8, ty << 8);
            let tile = grid.get_or_create_mut(id);
            stamp_tile(tile, origin, dab, r2, x0, y0, x1, y1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn stamp_tile(
    tile: &mut TileData,
    origin: (i64, i64),
    dab: &Dab,
    r2: f32,
    bx0: i64,
    by0: i64,
    bx1: i64,
    by1: i64,
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
    let radius = dab.radius.max(0.0);

    for py in py0..=py1 {
        // 像素中心采样
        let dy = (origin.1 + py as i64) as f64 + 0.5 - dab.y;
        for pxx in px0..=px1 {
            let dx = (origin.0 + pxx as i64) as f64 + 0.5 - dab.x;
            let d2 = (dx * dx + dy * dy) as f32;
            if d2 >= r2 {
                continue;
            }
            let t = d2.sqrt() / radius;
            let a = falloff(t, hardness) * alpha;
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
            &[half],
            &mut StrokeRecorder::new(lid),
        );
        let a1 = pixel(&layers, lid, 50, 50)[3];
        stamp_dabs(&mut layers.get_mut(lid).tiles, &[half], &mut rec);
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
            &[wash],
            &mut StrokeRecorder::new(lid2),
        );
        stamp_dabs(
            &mut layers2.get_mut(lid2).tiles,
            &[wash],
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
            &mut StrokeRecorder::new(lid),
        );
        // 第二笔覆盖同一瓦片：快照应为非空旧内容
        let mut rec = StrokeRecorder::new(lid);
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[dab_at(12.0, 12.0, 5.0, DabMode::Buildup)],
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
            &mut StrokeRecorder::new(lid),
        );
        assert_eq!(pixel(&layers, lid, 50, 50)[3], 255);
        // 硬橡皮擦中心
        let eraser = Dab {
            erase: true,
            ..dab_at(50.0, 50.0, 4.0, DabMode::Buildup)
        };
        stamp_dabs(
            &mut layers.get_mut(lid).tiles,
            &[eraser],
            &mut StrokeRecorder::new(lid),
        );
        let c = pixel(&layers, lid, 50, 50);
        assert_eq!(c[3], 0, "中心应被完全擦除");
        // 预乘不变式：alpha 0 则 RGB 也为 0
        assert_eq!(&c[..3], &[0, 0, 0]);
    }
}
