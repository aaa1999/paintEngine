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
            rev: 0,
        }
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

    pub fn screen_to_canvas(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.pan_x) / self.zoom, (y - self.pan_y) / self.zoom)
    }

    pub fn canvas_to_screen(&self, x: f64, y: f64) -> (f64, f64) {
        (x * self.zoom + self.pan_x, y * self.zoom + self.pan_y)
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
    fn zoom_clamped() {
        let mut vp = Viewport::new();
        vp.set_zoom(1e9);
        assert_eq!(vp.zoom(), MAX_ZOOM);
        vp.set_zoom(1e-9);
        assert_eq!(vp.zoom(), MIN_ZOOM);
    }
}
