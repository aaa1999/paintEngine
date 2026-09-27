use crate::geometry::Rect;

/// 视口：画布坐标 ↔ 屏幕物理像素的换算。
/// M1 仅平移+缩放；旋转/翻转（P2）只动这个模块。
///
/// 每次变更 `revision` 自增，引擎据此检测"绕过事件的视口修改"
/// 并触发全量重绘。
#[derive(Debug, Clone)]
pub struct Viewport {
    /// 屏幕像素每画布像素。
    zoom: f64,
    /// 画布原点在屏幕上的偏移（物理像素）。
    pan_x: f64,
    pan_y: f64,
    /// 旋转角（弧度，绕屏幕锚点应用）。
    rotation: f64,
    /// 水平翻转。
    flip_x: bool,
    rev: u64,
}

pub const MIN_ZOOM: f64 = 1.0 / 32.0;
pub const MAX_ZOOM: f64 = 64.0;

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            rotation: 0.0,
            flip_x: false,
            rev: 0,
        }
    }

    pub fn rotation(&self) -> f64 {
        self.rotation
    }

    pub fn flip_x(&self) -> bool {
        self.flip_x
    }

    /// 恒等变换（无旋转无翻转）——合成器据此启用双线性快速路径。
    pub fn transform_ident(&self) -> bool {
        self.rotation == 0.0 && !self.flip_x
    }

    /// 绕屏幕锚点旋转 delta 弧度（锚点下画布内容保持不动）。
    pub fn rotate_by(&mut self, anchor: (f64, f64), delta: f64) {
        // anchor_canvas_vec 返回的就是缩放后向量（F·R⁻¹(a−pan)）
        let (czx, czy) = self.anchor_canvas_vec(anchor);
        self.rotation += delta;
        let (c, s) = (self.rotation.cos(), self.rotation.sin());
        let fx = if self.flip_x { -czx } else { czx };
        self.pan_x = anchor.0 - (c * fx - s * czy);
        self.pan_y = anchor.1 - (s * fx + c * czy);
        self.rev += 1;
    }

    /// 绕屏幕锚点水平翻转。
    pub fn flip_x_at(&mut self, anchor: (f64, f64)) {
        // 保持 R 之后的向量不变：F·cz（按旧 flip 计算）在新变换下重现锚点
        let (czx, czy) = self.anchor_canvas_vec(anchor);
        let v = (if self.flip_x { -czx } else { czx }, czy);
        self.flip_x = !self.flip_x;
        let (c, s) = (self.rotation.cos(), self.rotation.sin());
        self.pan_x = anchor.0 - (c * v.0 - s * v.1);
        self.pan_y = anchor.1 - (s * v.0 + c * v.1);
        self.rev += 1;
    }

    /// 复位旋转/翻转（保持缩放与平移语义重置）。
    pub fn reset_transform(&mut self) {
        self.rotation = 0.0;
        self.flip_x = false;
        self.rev += 1;
    }

    /// 锚点相对 pan 的"缩放后画布向量"（含翻转的正向变换逆解）。
    fn anchor_canvas_vec(&self, anchor: (f64, f64)) -> (f64, f64) {
        let d = (anchor.0 - self.pan_x, anchor.1 - self.pan_y);
        let (c, s) = (self.rotation.cos(), self.rotation.sin());
        let rx = c * d.0 + s * d.1;
        let ry = -s * d.0 + c * d.1;
        (if self.flip_x { -rx } else { rx }, ry)
    }

    pub fn revision(&self) -> u64 {
        self.rev
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn pan(&self) -> (f64, f64) {
        (self.pan_x, self.pan_y)
    }

    pub fn pan_by(&mut self, dx: f64, dy: f64) {
        self.pan_x += dx;
        self.pan_y += dy;
        self.rev += 1;
    }

    /// 以屏幕点 `anchor` 为锚缩放：锚点下的画布内容保持不动。
    pub fn zoom_at(&mut self, anchor: (f64, f64), factor: f64) {
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let cx = (anchor.0 - self.pan_x) / self.zoom;
        let cy = (anchor.1 - self.pan_y) / self.zoom;
        self.pan_x = anchor.0 - cx * new_zoom;
        self.pan_y = anchor.1 - cy * new_zoom;
        self.zoom = new_zoom;
        self.rev += 1;
    }

    pub fn set_zoom(&mut self, zoom: f64) {
        let z = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if z != self.zoom {
            self.zoom = z;
            self.rev += 1;
        }
    }

    /// 视野适配：把 `content`（画布像素矩形）缩放平移到 `screen` 尺寸内，
    /// 四周留 `margin` 屏幕像素。无限画布导航的核心操作。
    pub fn fit_to(&mut self, content: Rect, screen: (f64, f64), margin: f64) {
        let cw = content.w.max(1) as f64;
        let ch = content.h.max(1) as f64;
        let avail_w = (screen.0 - margin * 2.0).max(1.0);
        let avail_h = (screen.1 - margin * 2.0).max(1.0);
        self.zoom = (avail_w / cw).min(avail_h / ch).clamp(MIN_ZOOM, MAX_ZOOM);
        // 内容中心对齐屏幕中心
        self.pan_x = screen.0 / 2.0 - (content.x as f64 + cw / 2.0) * self.zoom;
        self.pan_y = screen.1 / 2.0 - (content.y as f64 + ch / 2.0) * self.zoom;
        self.rev += 1;
    }

    /// 画布原点 (0,0) 居中、100% 缩放（空画布的合理初始视野）。
    pub fn center_origin(&mut self, screen: (f64, f64)) {
        self.zoom = 1.0;
        self.pan_x = screen.0 / 2.0;
        self.pan_y = screen.1 / 2.0;
        self.rev += 1;
    }

    /// 完整变换：canvas = F⁻¹·R⁻¹·(screen - pan) / zoom。
    pub fn screen_to_canvas(&self, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = self.anchor_canvas_vec((x, y));
        (rx / self.zoom, ry / self.zoom)
    }

    /// 完整变换：screen = R·F·canvas·zoom + pan。
    pub fn canvas_to_screen(&self, x: f64, y: f64) -> (f64, f64) {
        let mut px = x * self.zoom;
        if self.flip_x {
            px = -px;
        }
        let py = y * self.zoom;
        let (c, s) = (self.rotation.cos(), self.rotation.sin());
        (c * px - s * py + self.pan_x, s * px + c * py + self.pan_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_roundtrip() {
        let mut vp = Viewport::new();
        vp.pan_by(100.0, -50.0);
        vp.zoom_at((320.0, 240.0), 2.5);
        let (cx, cy) = vp.screen_to_canvas(123.0, 456.0);
        let (sx, sy) = vp.canvas_to_screen(cx, cy);
        assert!((sx - 123.0).abs() < 1e-9 && (sy - 456.0).abs() < 1e-9);
    }

    #[test]
    fn zoom_anchor_stable() {
        let mut vp = Viewport::new();
        vp.pan_by(-500.0, 300.0);
        let anchor = (200.0, 150.0);
        let before = vp.screen_to_canvas(anchor.0, anchor.1);
        vp.zoom_at(anchor, 3.0);
        let after = vp.screen_to_canvas(anchor.0, anchor.1);
        assert!((before.0 - after.0).abs() < 1e-9);
        assert!((before.1 - after.1).abs() < 1e-9);
    }

    #[test]
    fn fit_to_centers_and_scales() {
        let mut vp = Viewport::new();
        // 内容 200x100，位于画布 (1000, 2000)
        vp.fit_to(Rect::new(1000, 2000, 200, 100), (800.0, 600.0), 50.0);
        // 可用 700x500 → zoom = min(700/200, 500/100) = 3.5
        assert!((vp.zoom() - 3.5).abs() < 1e-9);
        let (cx, cy) = vp.screen_to_canvas(400.0, 300.0); // 屏幕中心
        assert!(
            (cx - 1100.0).abs() < 1e-9 && (cy - 2050.0).abs() < 1e-9,
            "内容中心应在屏幕中心: {cx},{cy}"
        );
    }

    #[test]
    fn center_origin_puts_origin_at_center() {
        let mut vp = Viewport::new();
        vp.pan_by(-500.0, 300.0);
        vp.center_origin((800.0, 600.0));
        assert_eq!(vp.zoom(), 1.0);
        let (cx, cy) = vp.screen_to_canvas(400.0, 300.0);
        assert!(cx.abs() < 1e-9 && cy.abs() < 1e-9);
    }

    #[test]
    fn rotation_roundtrip_and_anchor() {
        let mut vp = Viewport::new();
        vp.pan_by(120.0, -80.0);
        vp.set_zoom(2.0);
        vp.rotate_by((300.0, 200.0), 0.7);
        vp.flip_x_at((300.0, 200.0));
        assert!(!vp.transform_ident());
        let (cx, cy) = vp.screen_to_canvas(123.0, 456.0);
        let (sx, sy) = vp.canvas_to_screen(cx, cy);
        assert!((sx - 123.0).abs() < 1e-9 && (sy - 456.0).abs() < 1e-9);
        // 旋转的锚点稳定性
        let (ax, ay) = vp.screen_to_canvas(300.0, 200.0);
        vp.rotate_by((300.0, 200.0), -1.3);
        let (bx, by) = vp.screen_to_canvas(300.0, 200.0);
        assert!(
            (ax - bx).abs() < 1e-9 && (ay - by).abs() < 1e-9,
            "旋转应保锚点"
        );
        // 翻转镜像语义：x 取反、y 不变；翻两次复原
        vp.flip_x_at((300.0, 200.0));
        let (cx, cy) = vp.screen_to_canvas(300.0, 200.0);
        assert!(
            (cx + bx).abs() < 1e-9 && (cy - by).abs() < 1e-9,
            "翻转应镜像 x"
        );
        vp.flip_x_at((300.0, 200.0));
        let (dx, dy) = vp.screen_to_canvas(300.0, 200.0);
        assert!((dx - bx).abs() < 1e-9 && (dy - by).abs() < 1e-9);
        vp.reset_transform();
        assert!(vp.transform_ident());
    }

    #[test]
    fn zoom_clamped() {
        let mut vp = Viewport::new();
        vp.set_zoom(1e9);
        assert_eq!(vp.zoom(), MAX_ZOOM);
        vp.set_zoom(1e-9);
        assert_eq!(vp.zoom(), MIN_ZOOM);
    }
}
