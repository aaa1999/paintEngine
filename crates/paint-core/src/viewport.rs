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
    fn zoom_clamped() {
        let mut vp = Viewport::new();
        vp.set_zoom(1e9);
        assert_eq!(vp.zoom(), MAX_ZOOM);
        vp.set_zoom(1e-9);
        assert_eq!(vp.zoom(), MIN_ZOOM);
    }
}
