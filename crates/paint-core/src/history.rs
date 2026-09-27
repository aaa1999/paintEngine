use crate::layer::{Layer, LayerId, LayerStack};
use crate::tile::{TileGrid, TileId, TileRef, TILE_BYTES};

/// 单条撤销操作。组内按"前向记录顺序"存储，
/// [`UndoGroup::apply_to`] 以逆序应用并生成对称的逆操作组。
#[derive(Clone)]
pub enum UndoOp {
    /// 恢复瓦片旧内容。`None` 表示该瓦片此前不存在。
    Tiles(Vec<(LayerId, TileId, Option<TileRef>)>),
    /// 把携带数据的图层放回 index（撤销"移除图层"用）。
    InsertLayer {
        index: usize,
        id: LayerId,
        layer: Layer,
    },
    /// 移除图层（撤销"新增图层"用；数据在应用时捕获）。
    RemoveLayer { id: LayerId },
    /// 把图层移动到索引 `to`（撤销"移动图层"用）。
    MoveLayer { id: LayerId, to: usize },
}

impl UndoOp {
    fn approx_bytes(&self) -> usize {
        match self {
            UndoOp::Tiles(v) => v.iter().filter(|t| t.2.is_some()).count() * TILE_BYTES,
            // 近似记账：携带的图层按瓦片数计（Arc 可能与他人共享，宁多勿少）
            UndoOp::InsertLayer { layer, .. } => layer.tiles.len() * TILE_BYTES,
            _ => 0,
        }
    }
}

/// 一组可原子撤销的操作。
#[derive(Clone)]
pub struct UndoGroup {
    pub label: &'static str,
    pub ops: Vec<UndoOp>,
}

impl std::fmt::Debug for UndoGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UndoGroup")
            .field("label", &self.label)
            .field("ops", &self.ops.len())
            .finish()
    }
}

impl UndoGroup {
    pub fn bytes(&self) -> usize {
        self.ops.iter().map(|op| op.approx_bytes()).sum()
    }

    /// 应用到图层栈，返回逆操作（供 redo）。ops 逆序应用，
    /// 逆操作收集后反转，保证 redo 重放前向顺序。
    pub fn apply_to(&self, layers: &mut LayerStack) -> UndoGroup {
        let mut inverse: Vec<UndoOp> = Vec::with_capacity(self.ops.len());
        for op in self.ops.iter().rev() {
            match op {
                UndoOp::Tiles(list) => {
                    let mut inv = Vec::with_capacity(list.len());
                    for (lid, tid, old) in list {
                        let Some(layer) = layers.try_get_mut(*lid) else {
                            continue; // 图层已不存在：瓦片条目无意义
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
                    inverse.push(UndoOp::Tiles(inv));
                }
                UndoOp::InsertLayer { index, id, layer } => {
                    layers.insert_entry(*index, *id, layer.clone());
                    inverse.push(UndoOp::RemoveLayer { id: *id });
                }
                UndoOp::RemoveLayer { id } => {
                    if let Some((index, layer)) = layers.remove(*id) {
                        inverse.push(UndoOp::InsertLayer {
                            index,
                            id: *id,
                            layer,
                        });
                    }
                }
                UndoOp::MoveLayer { id, to } => {
                    if let Some((from, _)) = layers.move_layer(*id, *to) {
                        inverse.push(UndoOp::MoveLayer { id: *id, to: from });
                    }
                }
            }
        }
        inverse.reverse();
        UndoGroup {
            label: self.label,
            ops: inverse,
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
            ops: vec![UndoOp::Tiles(self.tiles)],
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

        let mut rec = StrokeRecorder::new(lid);
        rec.capture(&layers.get(lid).tiles, tid);
        paint(&mut layers, lid, tid, 200);
        let mut hist = History::new(usize::MAX);
        hist.push(rec.finish("Stroke"));
        assert_eq!(hist.undo_len(), 1);

        let g = hist.pop_undo().unwrap();
        let inv = g.apply_to(&mut layers);
        hist.push_redo(inv);
        assert!(!layers.get(lid).tiles.contains(tid));

        let g = hist.pop_redo().unwrap();
        let inv2 = g.apply_to(&mut layers);
        assert_eq!(layers.get(lid).tiles.get(tid).unwrap().pixels()[0], 200);
        drop(inv2);
    }

    #[test]
    fn layer_add_remove_undo() {
        let mut layers = LayerStack::new();
        let a = layers.insert(None);
        let b = layers.insert(None);
        assert_eq!(layers.len(), 2);

        // 前向：移除 a，撤销组携带逆操作（把 a 放回原位）
        let (_, layer_a) = layers.remove(a).unwrap();
        let group = UndoGroup {
            label: "RemoveLayer",
            ops: vec![UndoOp::InsertLayer {
                index: 0,
                id: a,
                layer: layer_a,
            }],
        };
        let mut hist = History::new(usize::MAX);
        hist.push(group);

        assert_eq!(layers.len(), 1);
        assert_eq!(layers.active(), b);

        let g = hist.pop_undo().unwrap();
        let inv = g.apply_to(&mut layers);
        hist.push_redo(inv);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers.position(a), Some(0));

        let g = hist.pop_redo().unwrap();
        let inv2 = g.apply_to(&mut layers);
        assert_eq!(layers.len(), 1);
        assert!(!layers.contains(a));
        drop(inv2);
    }

    #[test]
    fn move_layer_undo() {
        let mut layers = LayerStack::new();
        let a = layers.insert(None);
        let _b = layers.insert(None);
        layers.move_layer(a, 1); // a,b → b,a
        assert_eq!(layers.position(a), Some(1));
        let group = UndoGroup {
            label: "Reorder",
            ops: vec![UndoOp::MoveLayer { id: a, to: 0 }],
        };
        let mut hist = History::new(usize::MAX);
        hist.push(group);
        let g = hist.pop_undo().unwrap();
        let inv = g.apply_to(&mut layers);
        hist.push_redo(inv);
        assert_eq!(layers.position(a), Some(0));
        let g2 = hist.pop_redo().unwrap().apply_to(&mut layers); // redo
        assert_eq!(layers.position(a), Some(1));
        drop(g2);
    }

    #[test]
    fn evict_oldest_by_memory() {
        let mut hist = History::new(TILE_BYTES);
        let mut layers = LayerStack::new();
        let lid = layers.insert(None);
        let g1 = UndoGroup {
            label: "A",
            ops: vec![UndoOp::Tiles(vec![(
                lid,
                TileId { x: 0, y: 0 },
                Some(Arc::new(TileData::transparent())),
            )])],
        };
        hist.push(g1);
        let g2 = UndoGroup {
            label: "B",
            ops: vec![UndoOp::Tiles(vec![(
                lid,
                TileId { x: 2, y: 0 },
                Some(Arc::new(TileData::transparent())),
            )])],
        };
        hist.push(g2);
        assert_eq!(hist.undo_len(), 1, "第一组被淘汰");
    }

    #[test]
    fn push_clears_redo() {
        let mut hist = History::new(usize::MAX);
        let mut layers = LayerStack::new();
        let _lid = layers.insert(None);
        hist.push(UndoGroup {
            label: "A",
            ops: vec![],
        });
        let g = hist.pop_undo().unwrap();
        hist.push_redo(g);
        assert_eq!(hist.redo_len(), 1);
        hist.push(UndoGroup {
            label: "B",
            ops: vec![],
        });
        assert_eq!(hist.redo_len(), 0);
    }
}
