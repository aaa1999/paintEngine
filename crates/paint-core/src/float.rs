//! 内容级变换：浮动层（Floating）与 2D 仿射。
//!
//! 变换期间被"提升"的内容脱离图层，以 [`Floating`] 形式由合成器
//! 叠加渲染（预览）；提交时按累积仿射盖章回图层，整组入撤销。

use crate::layer::LayerId;
use crate::tile::TileGrid;

/// 行主序 2D 仿射：`x' = a*x + b*y + e; y' = c*x + d*y + f`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine2 {
    pub const IDENTITY: Affine2 = Affine2 {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// 平移矩阵。
    pub fn translation(tx: f64, ty: f64) -> Self {
        Self {
            e: tx,
            f: ty,
            ..Self::IDENTITY
        }
    }

    /// 旋转矩阵。
    pub fn rotation(rad: f64) -> Self {
        let (s, c) = (rad.sin(), rad.cos());
        Self {
            a: c,
            b: -s,
            c: s,
            d: c,
            ..Self::IDENTITY
        }
    }

    /// 缩放矩阵。
    pub fn scale(sx: f64, sy: f64) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    /// self ∘ other（先应用 other 再应用 self）。
    pub fn then(&self, other: &Affine2) -> Affine2 {
        Affine2 {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.a * other.e + self.b * other.f + self.e,
            f: self.c * other.e + self.d * other.f + self.f,
        }
    }

    /// 应用变换到点。
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.b * y + self.e,
            self.c * x + self.d * y + self.f,
        )
    }

    /// 数值逆（行列式为零时返回单位）。
    pub fn invert(&self) -> Affine2 {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return Self::IDENTITY;
        }
        let inv = 1.0 / det;
        let (a, b, c, d) = (self.d * inv, -self.b * inv, -self.c * inv, self.a * inv);
        Affine2 {
            a,
            b,
            c,
            d,
            e: -(a * self.e + b * self.f),
            f: -(c * self.e + d * self.f),
        }
    }

    /// 围绕点 p 应用 op（平移到原点 → op → 平移回）。
    pub fn around(op: &Affine2, p: (f64, f64)) -> Affine2 {
        Affine2::translation(p.0, p.1)
            .then(op)
            .then(&Affine2::translation(-p.0, -p.1))
    }
}

/// 变换中的浮动内容。
#[derive(Debug, Clone)]
pub struct Floating {
    /// 提升的内容（预乘 RGBA，画布对齐的瓦片坐标——提升时恒等仿射原位渲染）。
    pub tiles: TileGrid,
    /// 目标图层。
    pub layer: LayerId,
    /// 累积仿射：源画布坐标 → 当前画布坐标。
    pub affine: Affine2,
    /// 旋转/缩放锚点（源画布坐标；内容包围盒中心，随内容一起变换）。
    pub pivot: (f64, f64),
}

impl Floating {
    /// 围绕"当前内容中心"（源锚点经仿射映射）旋转。
    pub fn rotate(&mut self, delta_rad: f64) {
        let cur = self.affine.apply(self.pivot.0, self.pivot.1);
        self.affine = Affine2::around(&Affine2::rotation(delta_rad), cur).then(&self.affine);
    }

    /// 围绕当前内容中心缩放。
    pub fn scale(&mut self, factor: f64) {
        let cur = self.affine.apply(self.pivot.0, self.pivot.1);
        self.affine = Affine2::around(&Affine2::scale(factor, factor), cur).then(&self.affine);
    }

    /// 平移累积。
    pub fn translate(&mut self, dx: f64, dy: f64) {
        self.affine = Affine2::translation(dx, dy).then(&self.affine);
    }

    /// 浮动内容的当前屏幕级包围盒（画布坐标）：各瓦片 4 角经仿射取 AABB。
    pub fn canvas_bbox(&self) -> Option<crate::geometry::Rect> {
        let mut acc: Option<crate::geometry::Rect> = None;
        for id in self.tiles.ids() {
            let (ox, oy) = id.origin();
            let corners = [
                self.affine.apply(ox as f64, oy as f64),
                self.affine.apply(ox as f64 + 256.0, oy as f64),
                self.affine.apply(ox as f64, oy as f64 + 256.0),
                self.affine.apply(ox as f64 + 256.0, oy as f64 + 256.0),
            ];
            let x0 = corners.iter().map(|p| p.0).fold(f64::MAX, f64::min).floor() as i32;
            let y0 = corners.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor() as i32;
            let x1 = corners.iter().map(|p| p.0).fold(f64::MIN, f64::max).ceil() as i64;
            let y1 = corners.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil() as i64;
            let r = crate::geometry::Rect::new(
                x0,
                y0,
                (x1 - x0 as i64).max(1) as u32,
                (y1 - y0 as i64).max(1) as u32,
            );
            acc = Some(match acc {
                Some(a) => a.union(&r),
                None => r,
            });
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_compose_and_invert() {
        let t = Affine2::translation(10.0, -5.0);
        let r = Affine2::rotation(std::f64::consts::FRAC_PI_2);
        let m = t.then(&r); // 先旋转再平移
        let (x, y) = m.apply(1.0, 0.0); // 旋转→(0,1)，平移→(10,-4)
        assert!((x - 10.0).abs() < 1e-12 && (y + 4.0).abs() < 1e-12);
        let inv = m.invert();
        let (bx, by) = inv.apply(x, y);
        assert!((bx - 1.0).abs() < 1e-9 && (by - 0.0).abs() < 1e-9);
    }

    #[test]
    fn rotate_around_pivot_keeps_pivot() {
        let mut fl = Floating {
            tiles: TileGrid::new(),
            layer: LayerId::from_raw(0),
            affine: Affine2::translation(100.0, 50.0),
            pivot: (10.0, 10.0),
        };
        let before = fl.affine.apply(fl.pivot.0, fl.pivot.1);
        fl.rotate(0.7);
        fl.scale(1.3);
        let after = fl.affine.apply(fl.pivot.0, fl.pivot.1);
        assert!(
            (before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9,
            "旋转/缩放保持当前中心: {before:?} vs {after:?}"
        );
    }
}
