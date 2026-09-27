use crate::tile::TileGrid;

/// 图层句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

impl LayerId {
    pub fn to_raw(self) -> u64 {
        self.0
    }

    pub fn from_raw(v: u64) -> Self {
        LayerId(v)
    }
}

/// 图层混合模式。像素公式见 blend.rs（W3C Compositing and Blending Level 1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    pub const ALL: [BlendMode; 12] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            BlendMode::Normal => "正常",
            BlendMode::Multiply => "正片叠底",
            BlendMode::Screen => "滤色",
            BlendMode::Overlay => "叠加",
            BlendMode::Darken => "变暗",
            BlendMode::Lighten => "变亮",
            BlendMode::ColorDodge => "颜色减淡",
            BlendMode::ColorBurn => "颜色加深",
            BlendMode::HardLight => "强光",
            BlendMode::SoftLight => "柔光",
            BlendMode::Difference => "差值",
            BlendMode::Exclusion => "排除",
        }
    }
}

/// 单个图层：稀疏瓦片网格 + 属性。
#[derive(Clone)]
pub struct Layer {
    pub name: String,
    pub opacity: f32,
    pub visible: bool,
    pub blend_mode: BlendMode,
    pub tiles: TileGrid,
    /// 图层蒙版（灰度存瓦片 R 通道，1:1 对齐像素层；None = 无蒙版）。
    pub mask: Option<TileGrid>,
    /// 剪贴层：本层有效 alpha 受下方第一个非剪贴层的像素 alpha 约束。
    pub clipped: bool,
    /// 图层组标签（同名层属于同组，UI 折叠显示/批量操作）。
    pub group: Option<String>,
}

impl Layer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            opacity: 1.0,
            visible: true,
            blend_mode: BlendMode::Normal,
            tiles: TileGrid::new(),
            mask: None,
            clipped: false,
            group: None,
        }
    }
}

#[derive(Clone)]
struct Entry {
    id: LayerId,
    layer: Layer,
}

/// 图层栈。索引 0 为最底层，末尾为最上层（可见面）。
/// Clone 为浅拷贝语义：瓦片通过 Arc 共享，写入时 COW。
#[derive(Clone)]
pub struct LayerStack {
    entries: Vec<Entry>,
    next_id: u64,
    active: Option<LayerId>,
}

impl Default for LayerStack {
    fn default() -> Self {
        Self::new()
    }
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

    /// 分配新图层 id（insert_layer_obj 等自带对象的插入用）。
    pub fn alloc_id(&mut self) -> LayerId {
        let id = LayerId(self.next_id);
        self.next_id += 1;
        id
    }

    /// 以指定 id 在指定索引插入（撤销回放用）；推进 id 计数器防复用。
    pub fn insert_entry(&mut self, index: usize, id: LayerId, layer: Layer) {
        let idx = index.min(self.entries.len());
        self.entries.insert(idx, Entry { id, layer });
        if id.to_raw() >= self.next_id {
            self.next_id = id.to_raw() + 1;
        }
        if self.active.is_none() {
            self.active = Some(id);
        }
    }

    /// 移除图层，返回 (索引, 图层)。若活动图层被移除则改选邻近图层。
    pub fn remove(&mut self, id: LayerId) -> Option<(usize, Layer)> {
        let pos = self.position(id)?;
        let entry = self.entries.remove(pos);
        if self.active == Some(id) {
            // 优先选移除后顶到原位置的层，其次下层，栈空则为 None
            self.active = self
                .entries
                .get(pos)
                .or_else(|| self.entries.get(pos.saturating_sub(1)))
                .map(|e| e.id);
        }
        Some((pos, entry.layer))
    }

    /// 移动图层到索引 `to`（0 = 底层），返回 (原索引, 调整后索引)。
    pub fn move_layer(&mut self, id: LayerId, to: usize) -> Option<(usize, usize)> {
        let from = self.position(id)?;
        let to = to.min(self.entries.len() - 1);
        if from == to {
            return Some((from, to));
        }
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        Some((from, to))
    }

    /// 深拷贝语义的副本（瓦片 Arc 共享 + COW），插到源层之上并激活。
    pub fn duplicate(&mut self, id: LayerId) -> Option<LayerId> {
        let pos = self.position(id)?;
        let src = self.entries[pos].layer.clone();
        let new_id = LayerId(self.next_id);
        self.next_id += 1;
        let mut copy = Layer::new(format!("{} 副本", self.entries[pos].layer.name));
        copy.opacity = src.opacity;
        copy.blend_mode = src.blend_mode;
        copy.tiles = src.tiles;
        self.entries.insert(
            pos + 1,
            Entry {
                id: new_id,
                layer: copy,
            },
        );
        self.active = Some(new_id);
        Some(new_id)
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

    pub fn try_active(&self) -> Option<LayerId> {
        self.active
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

    /// 设置图层组标签。
    pub fn set_group(&mut self, id: LayerId, group: Option<String>) {
        if let Some(l) = self.try_get_mut(id) {
            l.group = group;
        }
    }

    /// 批量设置组内图层可见性。
    pub fn set_group_visible(&mut self, group: &str, visible: bool) -> usize {
        let mut n = 0;
        for e in &mut self.entries {
            if e.layer.group.as_deref() == Some(group) {
                e.layer.visible = visible;
                n += 1;
            }
        }
        n
    }

    /// 组内图层 id 列表。
    pub fn group_layers(&self, group: &str) -> Vec<LayerId> {
        self.entries
            .iter()
            .filter(|e| e.layer.group.as_deref() == Some(group))
            .map(|e| e.id)
            .collect()
    }

    /// 所有组名（有序去重）。
    pub fn group_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .iter()
            .filter_map(|e| e.layer.group.clone())
            .collect();
        names.dedup();
        names
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

    #[test]
    fn remove_reselects_active() {
        let mut s = LayerStack::new();
        let a = s.insert(None);
        let b = s.insert(None);
        let c = s.insert(None);
        assert_eq!(s.active(), c);
        s.remove(c);
        assert_eq!(s.active(), b, "移除顶层后选下层");
        s.remove(a);
        assert_eq!(s.active(), b);
        s.remove(b);
        assert_eq!(s.try_active(), None);
        assert!(s.is_empty());
    }

    #[test]
    fn move_layer_reorders() {
        let mut s = LayerStack::new();
        let a = s.insert(None);
        let b = s.insert(None);
        let c = s.insert(None);
        // a,b,c（底→顶）。把 a 移到顶
        s.move_layer(a, 2);
        assert_eq!(s.position(b), Some(0));
        assert_eq!(s.position(c), Some(1));
        assert_eq!(s.position(a), Some(2));
        s.move_layer(a, 0);
        assert_eq!(s.position(a), Some(0));
    }

    #[test]
    fn duplicate_shares_tiles_cow() {
        use crate::tile::{TileData, TileRef};
        use std::sync::Arc;
        let mut s = LayerStack::new();
        let a = s.insert(None);
        {
            let t = s
                .get_mut(a)
                .tiles
                .get_or_create_mut(crate::tile::TileId { x: 0, y: 0 });
            t.pixels_mut()[0..4].copy_from_slice(&[10, 20, 30, 255]);
        }
        let dup = s.duplicate(a).unwrap();
        let snap: TileRef = s
            .get(dup)
            .tiles
            .get(crate::tile::TileId { x: 0, y: 0 })
            .unwrap()
            .clone();
        // 写副本不影响的源（Arc 共享 + COW）
        s.get_mut(dup)
            .tiles
            .get_or_create_mut(crate::tile::TileId { x: 0, y: 0 })
            .pixels_mut()[0] = 99;
        assert_eq!(
            s.get(a)
                .tiles
                .get(crate::tile::TileId { x: 0, y: 0 })
                .unwrap()
                .pixels()[0],
            10
        );
        drop(Arc::new(TileData::transparent())); // 引用 tile 模块符号
        drop(snap);
    }

    #[test]
    fn insert_entry_restores_id_space() {
        let mut s = LayerStack::new();
        let a = s.insert(None);
        let b = s.insert(None);
        let (pos, layer) = s.remove(a).unwrap();
        // 用旧 id 重新插入
        s.insert_entry(pos, a, layer);
        assert!(s.contains(a));
        // 新建图层不会复用 a/b 的 id
        let c = s.insert(None);
        assert_ne!(c, a);
        assert_ne!(c, b);
    }
}
