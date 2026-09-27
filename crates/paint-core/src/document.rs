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
    /// 内容级变换中的浮动层（合成时叠加渲染于全部图层之上）。
    floating: Option<crate::float::Floating>,
    /// 固定画布尺寸（None = 无限画布）。原点恒为 (0,0)。
    canvas: Option<crate::geometry::Rect>,
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
            floating: None,
            canvas: None,
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
            floating: None,
            canvas: None,
        }
    }

    /// 固定画布尺寸（None = 无限画布）。
    pub fn canvas(&self) -> Option<crate::geometry::Rect> {
        self.canvas
    }

    /// 设置/清除画布尺寸。
    pub fn set_canvas(&mut self, rect: Option<crate::geometry::Rect>) {
        self.canvas = rect;
    }

    /// 变换中的浮动层（只读）。
    pub fn floating(&self) -> Option<&crate::float::Floating> {
        self.floating.as_ref()
    }

    /// 浮动层（可变）。
    pub fn floating_mut(&mut self) -> Option<&mut crate::float::Floating> {
        self.floating.as_mut()
    }

    /// 设置/清除浮动层。
    pub fn set_floating(&mut self, f: Option<crate::float::Floating>) {
        self.floating = f;
    }

    /// 像素级选区（R 通道网格；None = 无选区）。
    pub fn selection(&self) -> Option<&crate::tile::TileGrid> {
        self.selection.as_ref()
    }

    /// 返回是否有变化。
    pub fn set_selection(&mut self, sel: Option<crate::tile::TileGrid>) -> bool {
        let changed = self.selection.is_some() != sel.is_some();
        self.selection = sel;
        changed
    }

    /// 图层栈（只读）。
    pub fn layers(&self) -> &LayerStack {
        &self.layers
    }

    /// 图层栈（可变）。
    pub fn layers_mut(&mut self) -> &mut LayerStack {
        &mut self.layers
    }

    /// 指定图层（可变）。
    pub fn layer_mut(&mut self, id: LayerId) -> &mut Layer {
        self.layers.get_mut(id)
    }

    /// 视口（只读）。
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// 视口（可变——引擎经 revision 检测变更）。
    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }

    /// 活动图层 id（空栈 panic——先 try_active）。
    pub fn active_layer(&self) -> LayerId {
        self.layers.active()
    }

    /// 撤销历史（只读）。
    pub fn history(&self) -> &History {
        &self.history
    }

    /// 撤销历史（可变）。
    pub fn history_mut(&mut self) -> &mut History {
        &mut self.history
    }

    /// 全文档瓦片内存（图层 + 蒙版，字节）。
    pub fn tile_memory_bytes(&self) -> usize {
        self.layers
            .iter()
            .map(|l| {
                l.tiles.memory_bytes() + l.mask.as_ref().map(|m| m.memory_bytes()).unwrap_or(0)
            })
            .sum()
    }

    /// 总内存（瓦片 + 撤销历史）。
    pub fn total_memory_bytes(&self) -> usize {
        self.tile_memory_bytes() + self.history.memory_used()
    }

    /// 背景色。
    pub fn background(&self) -> Color {
        self.background
    }

    /// 设置背景色。
    pub fn set_background(&mut self, c: Color) {
        self.background = c;
    }

    /// 点阵网格开关。
    pub fn show_grid(&self) -> bool {
        self.show_grid
    }

    /// 设置网格开关。
    pub fn set_show_grid(&mut self, on: bool) {
        self.show_grid = on;
    }

    /// 提交撤销组（一笔结束、一次图层操作）。
    pub fn commit(&mut self, group: crate::history::UndoGroup) {
        self.history.push(group);
    }

    /// 撤销（应用逆操作组）。
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

    /// 重做。
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

    /// 删除图层（入撤销）。
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

    /// 复制图层（入撤销）。
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
