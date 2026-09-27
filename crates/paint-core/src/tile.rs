use std::collections::HashMap;
use std::sync::Arc;

use crate::geometry::Rect;

/// 瓦片边长（像素）。
pub const TILE: u32 = 256;
/// 单瓦片字节数：RGBA8 预乘 alpha。
pub const TILE_BYTES: usize = (TILE as usize) * (TILE as usize) * 4;

/// 瓦片网格坐标。画布无界，坐标可为负。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub x: i32,
    pub y: i32,
}

impl TileId {
    /// 由画布像素坐标取所在瓦片。
    pub fn at(canvas_x: i64, canvas_y: i64) -> Self {
        Self {
            x: (canvas_x >> 8) as i32,
            y: (canvas_y >> 8) as i32,
        }
    }

    /// 瓦片左上角在画布上的像素坐标。
    pub fn origin(&self) -> (i64, i64) {
        ((self.x as i64) << 8, (self.y as i64) << 8)
    }

    /// 打包成 u64 作快速缓存键。
    pub fn key(&self) -> u64 {
        ((self.y as u32 as u64) << 32) | self.x as u32 as u64
    }
}

/// 单张瓦片的像素数据。RGBA8、预乘 alpha、行主序。
#[derive(Clone)]
pub struct TileData {
    px: Vec<u8>,
}

impl TileData {
    pub fn transparent() -> Self {
        Self {
            px: vec![0; TILE_BYTES],
        }
    }

    pub fn pixels(&self) -> &[u8] {
        &self.px
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.px
    }

    pub fn is_transparent(&self) -> bool {
        self.px.as_chunks::<4>().0.iter().all(|p| p[3] == 0)
    }
}

/// 瓦片共享句柄。撤销历史与网格共享同一份数据，写入时 COW。
pub type TileRef = Arc<TileData>;

/// 稀疏瓦片网格：无限画布的存储层。
/// 不存在的瓦片等价于全透明，因此 `prune` 可安全回收空瓦片。
/// Clone 为 Arc 共享浅拷贝，写入时 COW。
#[derive(Default, Clone)]
pub struct TileGrid {
    tiles: HashMap<TileId, TileRef>,
}

impl TileGrid {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: TileId) -> Option<&TileRef> {
        self.tiles.get(&id)
    }

    pub fn contains(&self, id: TileId) -> bool {
        self.tiles.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn set(&mut self, id: TileId, tile: TileRef) {
        self.tiles.insert(id, tile);
    }

    pub fn remove(&mut self, id: TileId) -> Option<TileRef> {
        self.tiles.remove(&id)
    }

    pub fn ids(&self) -> impl Iterator<Item = TileId> + '_ {
        self.tiles.keys().copied()
    }

    /// 取可写瓦片，不存在则创建。若数据与撤销快照共享则先克隆（COW）。
    pub fn get_or_create_mut(&mut self, id: TileId) -> &mut TileData {
        let slot = self
            .tiles
            .entry(id)
            .or_insert_with(|| Arc::new(TileData::transparent()));
        Arc::make_mut(slot)
    }

    /// 回收全透明瓦片，返回回收数量。
    pub fn prune(&mut self) -> usize {
        let before = self.tiles.len();
        self.tiles.retain(|_, t| !t.is_transparent());
        before - self.tiles.len()
    }

    /// 像素精确的内容包围盒（扫描非透明像素）。O(内容瓦片数 × 瓦片)，
    /// 用户触发的"适应内容"等操作用；常态路径用瓦片粒度的 [`Self::content_bounds`]。
    pub fn content_bounds_precise(&self) -> Option<Rect> {
        let mut acc: Option<(i64, i64, i64, i64)> = None; // minx, miny, maxx, maxy（含）
        for (id, tile) in self.tiles.iter() {
            let px = tile.pixels();
            let (ox, oy) = (id.origin().0, id.origin().1);
            for y in 0..TILE as usize {
                let row = &px[y * TILE as usize * 4..][..TILE as usize * 4];
                if row.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                    continue;
                }
                for x in 0..TILE as usize {
                    if row[x * 4 + 3] != 0 {
                        let (gx, gy) = (ox + x as i64, oy + y as i64);
                        acc = Some(match acc {
                            None => (gx, gy, gx, gy),
                            Some((a, b, c, d)) => (a.min(gx), b.min(gy), c.max(gx), d.max(gy)),
                        });
                    }
                }
            }
        }
        acc.map(|(x0, y0, x1, y1)| {
            Rect::new(
                x0 as i32,
                y0 as i32,
                (x1 - x0 + 1) as u32,
                (y1 - y0 + 1) as u32,
            )
        })
    }

    /// 已存瓦片的包围盒（画布像素）。调用方应先 `prune` 保证无空瓦片。
    pub fn content_bounds(&self) -> Option<Rect> {
        let mut min = (i32::MAX, i32::MAX);
        let mut max = (i32::MIN, i32::MIN);
        for id in self.tiles.keys() {
            min.0 = min.0.min(id.x);
            min.1 = min.1.min(id.y);
            max.0 = max.0.max(id.x);
            max.1 = max.1.max(id.y);
        }
        if max.0 < min.0 {
            return None;
        }
        Some(Rect::new(
            min.0 * TILE as i32,
            min.1 * TILE as i32,
            (max.0 - min.0 + 1) as u32 * TILE,
            (max.1 - min.1 + 1) as u32 * TILE,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_red(t: &mut TileData) {
        t.pixels_mut()[0..4].copy_from_slice(&[255, 0, 0, 255]);
    }

    #[test]
    fn create_and_fetch() {
        let mut g = TileGrid::new();
        assert!(!g.contains(TileId { x: 0, y: 0 }));
        write_red(g.get_or_create_mut(TileId { x: 0, y: 0 }));
        assert!(g.contains(TileId { x: 0, y: 0 }));
        assert_eq!(g.len(), 1);
        assert_eq!(
            g.get(TileId { x: 0, y: 0 }).unwrap().pixels()[..4],
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn cow_snapshot_stable() {
        let mut g = TileGrid::new();
        write_red(g.get_or_create_mut(TileId { x: 1, y: -2 }));
        // 撤销快照与网格共享同一份 Arc
        let snap = g.get(TileId { x: 1, y: -2 }).unwrap().clone();
        {
            let t = g.get_or_create_mut(TileId { x: 1, y: -2 });
            t.pixels_mut()[0] = 100;
        }
        // 快照不受后续写入影响（COW 已克隆）
        assert_eq!(snap.pixels()[0], 255);
        assert_eq!(g.get(TileId { x: 1, y: -2 }).unwrap().pixels()[0], 100);
    }

    #[test]
    fn prune_and_bounds() {
        let mut g = TileGrid::new();
        write_red(g.get_or_create_mut(TileId { x: 2, y: 3 }));
        g.get_or_create_mut(TileId { x: -1, y: 0 }); // 空瓦片
        assert_eq!(g.len(), 2);
        assert_eq!(g.prune(), 1);
        assert_eq!(g.len(), 1);
        let b = g.content_bounds().unwrap();
        assert_eq!(b, Rect::new(2 * 256, 3 * 256, 256, 256));
    }

    #[test]
    fn tile_id_arith() {
        assert_eq!(TileId::at(0, 0), TileId { x: 0, y: 0 });
        assert_eq!(TileId::at(-1, -300), TileId { x: -1, y: -2 });
        assert_eq!(TileId::at(256, 255), TileId { x: 1, y: 0 });
        assert_eq!(TileId { x: -1, y: 0 }.origin(), (-256, 0));
    }
}
