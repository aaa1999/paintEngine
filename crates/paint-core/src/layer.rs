use crate::tile::TileGrid;
use crate::Color;

/// 图层句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerId(u64);

impl LayerId {
    /// 转 u64（序列化用）。
    pub fn to_raw(self) -> u64 {
        self.0
    }

    /// 从 u64 构造。
    pub fn from_raw(v: u64) -> Self {
        LayerId(v)
    }
}

/// 非破坏性图层调整：合成时对图层像素应用（直行域），不修改
/// 存储像素——随时可调/可关/可清零。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerAdjustment {
    /// 亮度偏移 -100..100。
    pub brightness: f32,
    /// 对比度 -100..100（中心 0.5）。
    pub contrast: f32,
    /// 饱和度 -100..100。
    pub saturation: f32,
    /// 色相偏移 -180..180。
    pub hue: f32,
    /// 调整强度 0..1（线性插值到原值）。
    pub strength: f32,
}

impl Default for LayerAdjustment {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            hue: 0.0,
            strength: 1.0,
        }
    }
}

impl LayerAdjustment {
    /// 调整参数全为零（无效果）。
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 0.0 && self.saturation == 0.0 && self.hue == 0.0
    }

    /// 直行 RGB（0..1）应用调整，返回新值。
    pub fn apply_rgb(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        let mut r = r;
        let mut g = g;
        let mut b = b;
        let br = self.brightness / 100.0;
        if br != 0.0 {
            r += br;
            g += br;
            b += br;
        }
        let c = 1.0 + self.contrast / 100.0;
        if c != 1.0 {
            r = (r - 0.5) * c + 0.5;
            g = (g - 0.5) * c + 0.5;
            b = (b - 0.5) * c + 0.5;
        }
        if self.hue != 0.0 || self.saturation != 0.0 {
            let (h, s, l) = rgb_to_hsl(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
            let h = (h + self.hue / 360.0).fract();
            let s = (s * (1.0 + self.saturation / 100.0)).clamp(0.0, 1.0);
            let (r2, g2, b2) = hsl_to_rgb(h, s, l);
            r = r2;
            g = g2;
            b = b2;
        }
        (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
    }

    /// 预乘 RGBA 单像素应用。
    pub fn apply_pixel(&self, px: &mut [u8]) {
        if self.is_identity() {
            return;
        }
        let a = px[3] as f32 / 255.0;
        if a == 0.0 {
            return;
        }
        let r = px[0] as f32 / a / 255.0;
        let g = px[1] as f32 / a / 255.0;
        let b = px[2] as f32 / a / 255.0;
        let (nr, ng, nb) = self.apply_rgb(r, g, b);
        let t = self.strength.clamp(0.0, 1.0);
        let r2 = r + (nr - r) * t;
        let g2 = g + (ng - g) * t;
        let b2 = b + (nb - b) * t;
        px[0] = (r2 * a * 255.0 + 0.5) as u8;
        px[1] = (g2 * a * 255.0 + 0.5) as u8;
        px[2] = (b2 * a * 255.0 + 0.5) as u8;
    }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = (if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    }) / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let conv = |mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (conv(h + 1.0 / 3.0), conv(h), conv(h - 1.0 / 3.0))
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

    /// 混合模式名（中文）。
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

/// 矢量对象：源数据持久保存于图层，合成时光栅化（`obj_tiles` 缓存）。
/// 非破坏——可重新编辑（改文字/移动/删除），编辑 = 重建缓存而非改像素。
#[derive(Debug, Clone, PartialEq)]
pub enum DrawObject {
    /// 文字。`bbox` 命中测试用；`raster` 为光栅缓存（Web 壳层渲染
    /// 的文字无引擎字体，必须随对象携带才能在撤销/载入后重现）。
    Text {
        pos: (f64, f64),
        text: String,
        size: f32,
        color: Color,
        raster: Option<std::sync::Arc<TextRaster>>,
        bbox: Option<crate::geometry::Rect>,
    },
    /// 几何形状（枚举就位；绘制 UI 后续接入）。
    Shape {
        kind: crate::shape::ShapeKind,
        a: (f64, f64),
        b: (f64, f64),
        fill: bool,
        color: Color,
        width: f32,
        bbox: Option<crate::geometry::Rect>,
    },
}

/// 文字对象的光栅缓存：预乘 RGBA + 相对 `pos` 的偏移。
#[derive(Debug, Clone, PartialEq)]
pub struct TextRaster {
    pub premul: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// 光栅左上角相对对象 `pos` 的偏移（画布像素）。
    pub dx: i64,
    pub dy: i64,
}

impl DrawObject {
    /// 命中测试包围盒（未光栅化过 = 无 bbox = 不命中）。
    pub fn bbox(&self) -> Option<crate::geometry::Rect> {
        match self {
            DrawObject::Text { bbox, .. } => *bbox,
            DrawObject::Shape { bbox, .. } => *bbox,
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
    /// 图层组标签（同名层属于同组，UI 折叠显示）。
    pub group: Option<String>,
    /// 非破坏性调整（合成时应用，不改像素）。
    pub adjustment: Option<LayerAdjustment>,
    /// 矢量对象列表（空 = 纯栅格层，行为与历史版本一致）。
    pub objects: Vec<DrawObject>,
    /// 对象光栅化缓存（对象编辑时整体重建）。
    pub obj_tiles: TileGrid,
    /// 对象缓存待重建（引擎持渲染器侧执行；撤销/重做/载入后置位）。
    pub obj_stale: bool,
    /// 合成内容缓存：tiles + obj_tiles 预乘 over 合并（对象在栅格之上）。
    /// 增量维护——与 tiles Arc 共享，无对象重叠的瓦片零拷贝。
    pub merged: TileGrid,
}

impl Layer {
    /// 新建图层。
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
            adjustment: None,
            objects: Vec::new(),
            obj_tiles: TileGrid::new(),
            obj_stale: false,
            merged: TileGrid::new(),
        }
    }

    /// 合成内容：无对象 = 原始瓦片；有对象 = 合并缓存。
    /// 合成器（CPU/GPU）统一从此读取，两端零感知。
    pub fn content(&self) -> &TileGrid {
        if self.objects.is_empty() {
            &self.tiles
        } else {
            &self.merged
        }
    }

    /// 单瓦片重合并：merged[tid] = tiles[tid] 被 obj_tiles[tid] 覆盖。
    /// 无对象瓦片重叠时与 tiles Arc 共享（零拷贝）。
    fn merge_tile(&mut self, tid: crate::tile::TileId) {
        use std::sync::Arc;
        match self.obj_tiles.get(tid) {
            None => {
                // 无对象覆盖：直接共享基础瓦片
                match self.tiles.get(tid) {
                    Some(t) => self.merged.set(tid, Arc::clone(t)),
                    None => {
                        self.merged.remove(tid);
                    }
                }
            }
            Some(ot) => {
                let mut px = match self.tiles.get(tid) {
                    Some(t) => (**t).clone(),
                    None => crate::tile::TileData::transparent(),
                };
                over_pixels(px.pixels_mut(), ot.pixels());
                self.merged.set(tid, Arc::new(px));
            }
        }
    }

    /// 基础瓦片在 `ids` 内变化后同步合并缓存（笔画/填充/撤销等写路径调用）。
    pub fn sync_tiles(&mut self, ids: impl Iterator<Item = crate::tile::TileId>) {
        if self.objects.is_empty() {
            return;
        }
        for tid in ids {
            self.merge_tile(tid);
        }
    }

    /// 对象列表变化后：置缓存待重建标记 + 清空合并缓存
    /// （重建在引擎侧 rasterize_objects 完成后回调 sync_all）。
    pub fn objects_edited(&mut self) {
        if !self.objects.is_empty() || !self.obj_tiles.is_empty() {
            self.obj_stale = true;
        }
    }

    /// 对象缓存重建完成：全量同步合并缓存。
    pub fn sync_all_objects(&mut self) {
        self.obj_stale = false;
        if self.objects.is_empty() {
            self.merged = self.tiles.clone();
            return;
        }
        // 覆盖 obj_tiles ∪ 旧 merged 中对象涉及的瓦片
        let ids: Vec<crate::tile::TileId> = self
            .obj_tiles
            .ids()
            .chain(self.merged.ids())
            .collect();
        for tid in ids {
            self.merge_tile(tid);
        }
    }
}

/// 预乘 over：dst = src over dst（逐像素，256×256 瓦片内联热路径）。
pub(crate) fn over_pixels(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
        let sa = s[3] as u32;
        if sa == 0 {
            continue;
        }
        if sa == 255 {
            d.copy_from_slice(s);
            continue;
        }
        for k in 0..3 {
            d[k] = (s[k] as u32 + d[k] as u32 * (255 - sa) / 255) as u8;
        }
        d[3] = (sa + d[3] as u32 * (255 - sa) / 255) as u8;
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

    /// 栈位置（0 = 底层）。
    pub fn position(&self, id: LayerId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    /// 按 id 取图层（不存在 panic）。
    pub fn get(&self, id: LayerId) -> &Layer {
        self.try_get(id).expect("图层不存在（调用方保证）")
    }

    /// 不存在返回 None。
    pub fn try_get(&self, id: LayerId) -> Option<&Layer> {
        self.position(id).map(|p| &self.entries[p].layer)
    }

    /// 按 id 取可变图层（不存在 panic）。
    pub fn get_mut(&mut self, id: LayerId) -> &mut Layer {
        self.try_get_mut(id).expect("图层不存在（调用方保证）")
    }

    /// 图层可能已被删除的场合（撤销组回放等）。
    pub fn try_get_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        let p = self.position(id)?;
        Some(&mut self.entries[p].layer)
    }

    /// 图层是否存在。
    pub fn contains(&self, id: LayerId) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    /// 活动图层 id（空栈 panic）。
    pub fn active(&self) -> LayerId {
        self.active
            .expect("空图层栈没有活动图层（调用方应先 try_active）")
    }

    /// 活动图层 id（空栈 None）。
    pub fn try_active(&self) -> Option<LayerId> {
        self.active
    }

    /// 设置活动图层。
    pub fn set_active(&mut self, id: LayerId) {
        assert!(self.contains(id), "图层不存在");
        self.active = Some(id);
    }

    /// 图层数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空栈。
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
