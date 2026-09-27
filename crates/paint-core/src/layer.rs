use crate::tile::TileGrid;

/// 图层句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

impl LayerId {
    pub fn to_raw(self) -> u64 {
        self.0
    }
}

/// M1 仅实现 Normal；12 种混合模式在 P1 补全。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
}

/// 单个图层：稀疏瓦片网格 + 属性。
pub struct Layer {
    pub name: String,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: BlendMode,
    pub tiles: TileGrid,
}

impl Layer {
    fn new(name: String) -> Self {
        Self {
            name,
            opacity: 1.0,
            visible: true,
            blend_mode: BlendMode::Normal,
            tiles: TileGrid::new(),
        }
    }
}

struct Entry {
    id: LayerId,
    layer: Layer,
}

/// 图层栈。索引 0 为最底层，末尾为最上层（可见面）。
pub struct LayerStack {
    entries: Vec<Entry>,
    next_id: u64,
    active: Option<LayerId>,
}

impl LayerStack {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 0,
            active: None,
        }
    }

    /// 插入新图层并激活。`above: None` 插到最顶层；
    /// `Some(id)` 插到该层之上（z 序更高）。
    pub fn insert(&mut self, above: Option<LayerId>) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        let idx = above
            .and_then(|a| self.position(a))
            .map_or(self.entries.len(), |p| p + 1);
        let layer = Layer::new(format!("图层 {}", id.to_raw() + 1));
        self.entries.insert(idx, Entry { id, layer });
        self.active = Some(id);
        id
    }

    pub fn position(&self, id: LayerId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    pub fn get(&self, id: LayerId) -> &Layer {
        &self.entries[self.position(id).expect("图层不存在")].layer
    }

    pub fn get_mut(&mut self, id: LayerId) -> &mut Layer {
        let p = self.position(id).expect("图层不存在");
        &mut self.entries[p].layer
    }

    /// 图层可能已被删除的场合（撤销组回放等）。
    pub fn try_get_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        let p = self.position(id)?;
        Some(&mut self.entries[p].layer)
    }

    pub fn contains(&self, id: LayerId) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    pub fn active(&self) -> LayerId {
        self.active.expect("空图层栈没有活动图层")
    }

    pub fn set_active(&mut self, id: LayerId) {
        assert!(self.contains(id), "图层不存在");
        self.active = Some(id);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 自底向上迭代（合成顺序）。
    pub fn iter(&self) -> impl Iterator<Item = &Layer> {
        self.entries.iter().map(|e| &e.layer)
    }

    /// 自底向上的 (id, layer) 迭代。
    pub fn iter_with_id(&self) -> impl Iterator<Item = (LayerId, &Layer)> {
        self.entries.iter().map(|e| (e.id, &e.layer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_positions() {
        let mut s = LayerStack::new();
        let a = s.insert(None);
        assert_eq!(s.len(), 1);
        let b = s.insert(None);
        let c = s.insert(Some(a)); // 插到 a 之上
        assert_eq!(s.position(a), Some(0));
        assert_eq!(s.position(c), Some(1));
        assert_eq!(s.position(b), Some(2));
        assert_eq!(s.active(), c);
        s.set_active(a);
        assert_eq!(s.active(), a);
        assert_eq!(s.get(a).name, "图层 1");
        assert!(s.contains(b));
    }
}
