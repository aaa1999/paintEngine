use crate::layer::{LayerId, LayerStack};
use crate::tile::{TileGrid, TileId, TileRef, TILE_BYTES};

/// 一组可原子撤销的瓦片变更。M1 只含瓦片操作；
/// 图层结构操作（增删/排序等）撤销在 P1 扩展为 ops 枚举。
#[derive(Clone)]
pub struct UndoGroup {
    pub label: &'static str,
    /// (图层, 瓦片, 旧内容)。`None` 表示写入前该瓦片不存在。
    pub tiles: Vec<(LayerId, TileId, Option<TileRef>)>,
}

impl std::fmt::Debug for UndoGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UndoGroup")
            .field("label", &self.label)
            .field("tiles", &self.tiles.len())
            .finish()
    }
}

impl UndoGroup {
    pub fn bytes(&self) -> usize {
        // 近似记账：只算携带快照的条目；跨组共享同一 Arc 会重复计，
        // 作为淘汰依据足够（宁多勿少）。
        self.tiles.iter().filter(|t| t.2.is_some()).count() * TILE_BYTES
    }

    /// 应用到图层栈并返回逆操作（用于 redo）。
    pub fn apply_to(&self, layers: &mut LayerStack) -> UndoGroup {
        let mut inv = Vec::with_capacity(self.tiles.len());
        for (lid, tid, old) in &self.tiles {
            let Some(layer) = layers.try_get_mut(*lid) else {
                continue; // 图层已删除：该条目无意义
            };
            let before = layer.tiles.get(*tid).cloned();
            match old {
                Some(t) => layer.tiles.set(*tid, t.clone()),
                None => {
                    layer.tiles.remove(*tid);
                }
            }
            inv.push((*lid, *tid, before));
        }
        UndoGroup {
            label: self.label,
            tiles: inv,
        }
    }
}

/// 撤销/重做栈。按内存限额淘汰最旧的历史（无限画布不能按步数限）。
pub struct History {
    undo: Vec<UndoGroup>,
    redo: Vec<UndoGroup>,
    limit: usize,
}

impl History {
    pub fn new(memory_limit: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: memory_limit,
        }
    }

    fn total_bytes(stacks: &[UndoGroup]) -> usize {
        stacks.iter().map(|g| g.bytes()).sum()
    }

    fn evict(&mut self) {
        while Self::total_bytes(&self.undo) > self.limit && self.undo.len() > 1 {
            self.undo.remove(0);
        }
    }

    /// 提交新的撤销组，清空重做栈。
    pub fn push(&mut self, group: UndoGroup) {
        self.redo.clear();
        self.undo.push(group);
        self.evict();
    }

    pub fn pop_undo(&mut self) -> Option<UndoGroup> {
        self.undo.pop()
    }

    /// 撤销后重做归来：压回撤销栈，不清空重做栈。
    pub fn push_undo(&mut self, group: UndoGroup) {
        self.undo.push(group);
    }

    pub fn push_redo(&mut self, group: UndoGroup) {
        self.redo.push(group);
    }

    pub fn pop_redo(&mut self) -> Option<UndoGroup> {
        self.redo.pop()
    }

    pub fn set_memory_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.evict();
    }

    pub fn memory_limit(&self) -> usize {
        self.limit
    }

    pub fn memory_used(&self) -> usize {
        Self::total_bytes(&self.undo) + Self::total_bytes(&self.redo)
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }
}

/// 一笔进行中的撤销采集器：首次写某瓦片前记录旧快照。
pub struct StrokeRecorder {
    layer: LayerId,
    seen: std::collections::HashSet<TileId>,
    tiles: Vec<(LayerId, TileId, Option<TileRef>)>,
}

impl StrokeRecorder {
    pub fn new(layer: LayerId) -> Self {
        Self {
            layer,
            seen: std::collections::HashSet::new(),
            tiles: Vec::new(),
        }
    }

    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// 在写入瓦片前调用；同一瓦片只记录一次。
    pub fn capture(&mut self, grid: &TileGrid, id: TileId) {
        if self.seen.insert(id) {
            self.tiles.push((self.layer, id, grid.get(id).cloned()));
        }
    }

    pub fn finish(self, label: &'static str) -> UndoGroup {
        UndoGroup {
            label,
            tiles: self.tiles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile::TileData;
    use std::sync::Arc;

    fn paint(layers: &mut LayerStack, lid: LayerId, tid: TileId, v: u8) {
        let layer = layers.get_mut(lid);
        let t = layer.tiles.get_or_create_mut(tid);
        t.pixels_mut()[0..4].copy_from_slice(&[v, v, v, 255]);
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut layers = LayerStack::new();
        let lid = layers.insert(None);
        let tid = TileId { x: 0, y: 0 };

        // 笔画前记录旧瓦片（不存在 → None），写入后提交
        let mut rec = StrokeRecorder::new(lid);
        rec.capture(&layers.get(lid).tiles, tid);
        paint(&mut layers, lid, tid, 200);
        let mut hist = History::new(usize::MAX);
        hist.push(rec.finish("Stroke"));
        assert_eq!(hist.undo_len(), 1);

        // 撤销：瓦片回到不存在
        let g = hist.pop_undo().unwrap();
        let inv = g.apply_to(&mut layers);
        hist.push_redo(inv);
        assert!(!layers.get(lid).tiles.contains(tid));

        // 重做：瓦片恢复
        let g = hist.pop_redo().unwrap();
        let inv2 = g.apply_to(&mut layers);
        assert_eq!(layers.get(lid).tiles.get(tid).unwrap().pixels()[0], 200);
        drop(inv2);
    }

    #[test]
    fn evict_oldest_by_memory() {
        let mut hist = History::new(TILE_BYTES); // 只够留一组
        let mut layers = LayerStack::new();
        let lid = layers.insert(None);
        let mut g1 = UndoGroup {
            label: "A",
            tiles: vec![(
                lid,
                TileId { x: 0, y: 0 },
                Some(Arc::new(TileData::transparent())),
            )],
        };
        g1.tiles.push((lid, TileId { x: 1, y: 0 }, None));
        hist.push(g1);
        let g2 = UndoGroup {
            label: "B",
            tiles: vec![(
                lid,
                TileId { x: 2, y: 0 },
                Some(Arc::new(TileData::transparent())),
            )],
        };
        hist.push(g2);
        // 第一组被淘汰（保留至少一组）
        assert_eq!(hist.undo_len(), 1);
    }

    #[test]
    fn push_clears_redo() {
        let mut hist = History::new(usize::MAX);
        let mut layers = LayerStack::new();
        let _lid = layers.insert(None);
        hist.push(UndoGroup {
            label: "A",
            tiles: vec![],
        });
        let g = hist.pop_undo().unwrap();
        hist.push_redo(g);
        assert_eq!(hist.redo_len(), 1);
        hist.push(UndoGroup {
            label: "B",
            tiles: vec![],
        });
        assert_eq!(hist.redo_len(), 0);
    }
}
