use crate::history::{History, UndoGroup};
use crate::layer::{Layer, LayerId, LayerStack};
use crate::viewport::Viewport;
use crate::Color;

/// 画布文档：图层栈 + 视口 + 撤销历史 + 背景色。
pub struct Document {
    layers: LayerStack,
    viewport: Viewport,
    history: History,
    background: Color,
    /// 空白区点阵网格（无限画布的空间指示）。仅屏幕合成使用；
    /// 导出走独立文档，不受此开关影响。
    show_grid: bool,
    /// 像素级选区（R 通道；None = 无选区）。裁剪笔画写入范围。
    selection: Option<crate::tile::TileGrid>,
}

impl Document {
    /// 新建文档，含一个默认图层并激活。
    pub fn new(undo_memory_limit: usize) -> Self {
        let mut layers = LayerStack::new();
        layers.insert(None);
        Self {
            layers,
            viewport: Viewport::new(),
            history: History::new(undo_memory_limit),
            background: Color::WHITE,
            show_grid: false,
            selection: None,
        }
    }

    /// 以既有图层栈构造（导出用临时文档等场合）。默认不带网格，
    /// 保证导出画面干净。
    pub fn with_layers(layers: LayerStack) -> Self {
        Self {
            layers,
            viewport: Viewport::new(),
            history: History::new(usize::MAX),
            background: Color::WHITE,
            show_grid: false,
            selection: None,
        }
    }

    pub fn selection(&self) -> Option<&crate::tile::TileGrid> {
        self.selection.as_ref()
    }

    /// 返回是否有变化。
    pub fn set_selection(&mut self, sel: Option<crate::tile::TileGrid>) -> bool {
        let changed = self.selection.is_some() != sel.is_some();
        self.selection = sel;
        changed
    }

    pub fn layers(&self) -> &LayerStack {
        &self.layers
    }

    pub fn layers_mut(&mut self) -> &mut LayerStack {
        &mut self.layers
    }

    pub fn layer_mut(&mut self, id: LayerId) -> &mut Layer {
        self.layers.get_mut(id)
    }

    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }

    pub fn active_layer(&self) -> LayerId {
        self.layers.active()
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    pub fn background(&self) -> Color {
        self.background
    }

    pub fn set_background(&mut self, c: Color) {
        self.background = c;
    }

    pub fn show_grid(&self) -> bool {
        self.show_grid
    }

    pub fn set_show_grid(&mut self, on: bool) {
        self.show_grid = on;
    }

    /// 提交撤销组（一笔结束、一次图层操作）。
    pub fn commit(&mut self, group: crate::history::UndoGroup) {
        self.history.push(group);
    }

    pub fn undo(&mut self) -> bool {
        match self.history.pop_undo() {
            Some(g) => {
                let inv = g.apply_to(&mut self.layers);
                self.history.push_redo(inv);
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.history.pop_redo() {
            Some(g) => {
                let inv = g.apply_to(&mut self.layers);
                self.history.push_undo(inv);
                true
            }
            None => false,
        }
    }

    // ── 图层结构操作（均记入撤销历史）──

    /// 新建图层并激活。
    pub fn add_layer(&mut self, above: Option<LayerId>) -> Option<LayerId> {
        if let Some(a) = above {
            if !self.layers.contains(a) {
                return None;
            }
        }
        let id = self.layers.insert(above);
        self.commit(UndoGroup {
            label: "AddLayer",
            ops: vec![crate::history::UndoOp::RemoveLayer { id }],
        });
        Some(id)
    }

    /// 以既有图层对象插入（导入图像用），返回新 id。
    pub fn insert_layer_obj(&mut self, layer: Layer, index: usize) -> LayerId {
        let id = self.layers.alloc_id();
        self.layers.insert_entry(index, id, layer);
        self.layers.set_active(id);
        self.commit(UndoGroup {
            label: "ImportImage",
            ops: vec![crate::history::UndoOp::RemoveLayer { id }],
        });
        id
    }

    pub fn remove_layer(&mut self, id: LayerId) -> bool {
        let Some((index, layer)) = self.layers.remove(id) else {
            return false;
        };
        self.commit(UndoGroup {
            label: "RemoveLayer",
            ops: vec![crate::history::UndoOp::InsertLayer { index, id, layer }],
        });
        true
    }

    pub fn duplicate_layer(&mut self, id: LayerId) -> Option<LayerId> {
        let new_id = self.layers.duplicate(id)?;
        self.commit(UndoGroup {
            label: "DuplicateLayer",
            ops: vec![crate::history::UndoOp::RemoveLayer { id: new_id }],
        });
        Some(new_id)
    }

    /// 移动图层到索引 `to`（0 = 底层）。
    pub fn reorder_layer(&mut self, id: LayerId, to: usize) -> bool {
        let Some((from, to_eff)) = self.layers.move_layer(id, to) else {
            return false;
        };
        if from == to_eff {
            return true; // 位置未变，不产生历史
        }
        self.commit(UndoGroup {
            label: "ReorderLayer",
            ops: vec![crate::history::UndoOp::MoveLayer { id, to: from }],
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_doc_has_active_layer() {
        let d = Document::new(1024);
        assert_eq!(d.layers().len(), 1);
        assert_eq!(d.history().undo_len(), 0);
    }

    #[test]
    fn layer_ops_undoable() {
        let mut d = Document::new(usize::MAX);
        let a = d.active_layer();
        assert_eq!(d.layers().len(), 1);

        let b = d.add_layer(None).unwrap();
        assert_eq!(d.layers().len(), 2);
        assert_eq!(d.active_layer(), b);
        assert!(d.undo());
        assert_eq!(d.layers().len(), 1);
        assert_eq!(d.active_layer(), a, "撤销新建后活动层回落");
        assert!(d.redo());
        assert_eq!(d.layers().len(), 2);

        let _c = d.duplicate_layer(b).unwrap();
        assert_eq!(d.layers().len(), 3);
        assert!(d.undo());
        assert_eq!(d.layers().len(), 2);

        d.add_layer(None).unwrap(); // 3 层：a,b,new
        assert!(d.remove_layer(b));
        assert_eq!(d.layers().len(), 2);
        assert!(d.undo());
        assert_eq!(d.layers().len(), 3, "撤销删除恢复图层");
        assert_eq!(d.layers().position(b), Some(1));

        // 重排：把底层 a 移到顶
        assert!(d.reorder_layer(a, 2));
        assert_eq!(d.layers().position(a), Some(2));
        assert!(d.undo());
        assert_eq!(d.layers().position(a), Some(0));
    }

    #[test]
    fn remove_last_layer_ok() {
        let mut d = Document::new(usize::MAX);
        let a = d.active_layer();
        assert!(d.remove_layer(a));
        assert!(d.layers().is_empty());
        assert_eq!(d.layers().try_active(), None);
        assert!(d.undo());
        assert_eq!(d.layers().len(), 1);
    }
}
