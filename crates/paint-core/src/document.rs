use crate::history::History;
use crate::layer::{Layer, LayerId, LayerStack};
use crate::viewport::Viewport;
use crate::Color;

/// 画布文档：图层栈 + 视口 + 撤销历史 + 背景色。
pub struct Document {
    layers: LayerStack,
    viewport: Viewport,
    history: History,
    background: Color,
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
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_doc_has_active_layer() {
        let d = Document::new(1024);
        assert_eq!(d.layers().len(), 1);
        assert!(d.layers().contains(d.active_layer()));
        assert_eq!(d.history().undo_len(), 0);
    }
}
