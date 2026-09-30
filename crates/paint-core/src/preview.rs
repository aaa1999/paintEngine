//! 形状工具拖拽预览：直接写入合成帧缓冲的轻量光栅化。
//!
//! 预览是"显示层"概念——不落瓦片、不入撤销，只在 [`Engine::render`]
//! 合成后叠加到帧上；提交时由正规 dab 管线（`Renderer::stamp_dabs`）
//! 盖进图层。因此这里的 dab 光栅化是简化版（圆头 + 硬度渐变，
//! 无纹理尖/双重笔尖/buildup），预览与最终结果的细微差异可接受。
//!
//! 帧缓冲为 RGBA8 预乘、行主序，所有函数只处理 `region` 内的像素。

use crate::color::Color;
use crate::geometry::Rect;
use crate::stroke::Dab;

/// 单个 dab 盖进平铺帧缓冲（预乘 over）。
fn stamp_dab(target: &mut [u8], w: u32, region: Rect, dab: &Dab) {
    let r = dab.radius.max(0.5) as f64;
    let x0 = (dab.x - r).floor().max(region.x as f64) as i32;
    let y0 = (dab.y - r).floor().max(region.y as f64) as i32;
    let x1 = (dab.x + r).ceil().min(region.x2() as f64) as i32;
    let y1 = (dab.y + r).ceil().min(region.y2() as f64) as i32;
    // 硬度决定实心核比例：h=1 全实心，h=0 全柔边
    let core = r * dab.hardness.clamp(0.0, 1.0) as f64;
    let edge = (r - core).max(0.5);
    let a = dab.alpha.clamp(0.0, 1.0);
    let (sr, sg, sb) = (dab.color.r, dab.color.g, dab.color.b);
    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as f64 + 0.5 - dab.x;
            let dy = py as f64 + 0.5 - dab.y;
            let dist = dx.hypot(dy);
            if dist >= r {
                continue;
            }
            let cov = ((r - dist) / edge).clamp(0.0, 1.0) as f32;
            let sa = cov * a;
            if sa <= 0.0 {
                continue;
            }
            let i = ((py as u32 * w + px as u32) * 4) as usize;
            if i + 3 >= target.len() {
                continue;
            }
            let p = &mut target[i..i + 4];
            for k in 0..3 {
                let src = [sr, sg, sb][k] as f32 * sa;
                p[k] = (src + p[k] as f32 * (1.0 - sa) + 0.5) as u8;
            }
            p[3] = (255.0 * sa + p[3] as f32 * (1.0 - sa) + 0.5) as u8;
        }
    }
}

/// dab 链盖进帧缓冲（只处理 `region`）。
pub fn stamp_dabs_flat(target: &mut [u8], w: u32, region: Rect, dabs: &[Dab]) {
    for dab in dabs {
        stamp_dab(target, w, region, dab);
    }
}

/// 半透明矩形填充（预乘 over）。
pub fn fill_rect_flat(
    target: &mut [u8],
    w: u32,
    region: Rect,
    rect: Rect,
    color: Color,
    alpha: f32,
) {
    let Some(r) = rect.intersect(&region) else {
        return;
    };
    let a = alpha.clamp(0.0, 1.0);
    for y in r.y..r.y2() as i32 {
        for x in r.x..r.x2() as i32 {
            let i = ((y as u32 * w + x as u32) * 4) as usize;
            if i + 3 >= target.len() {
                continue;
            }
            let p = &mut target[i..i + 4];
            for k in 0..3 {
                let src = [color.r, color.g, color.b][k] as f32 * a;
                p[k] = (src + p[k] as f32 * (1.0 - a) + 0.5) as u8;
            }
            p[3] = (255.0 * a + p[3] as f32 * (1.0 - a) + 0.5) as u8;
        }
    }
}

/// 半透明椭圆填充（扫描线，预乘 over）。
#[allow(clippy::too_many_arguments)]
pub fn fill_ellipse_flat(
    target: &mut [u8],
    w: u32,
    region: Rect,
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    color: Color,
    alpha: f32,
) {
    let (rx, ry) = (rx.max(0.5), ry.max(0.5));
    let a = alpha.clamp(0.0, 1.0);
    let y0 = (cy - ry).floor().max(region.y as f64) as i32;
    let y1 = (cy + ry).ceil().min(region.y2() as f64) as i32;
    for py in y0..y1 {
        let dy = py as f64 + 0.5 - cy;
        let t = 1.0 - (dy / ry) * (dy / ry);
        if t <= 0.0 {
            continue;
        }
        let half = (t.sqrt() * rx).round() as i64;
        let xc = cx.round() as i64;
        let x0 = (xc - half).max(region.x as i64);
        let x1 = (xc + half).min(region.x2());
        for px in x0..x1 {
            let i = (((py as i64) as u32 * w + px as u32) * 4) as usize;
            if i + 3 >= target.len() {
                continue;
            }
            let p = &mut target[i..i + 4];
            for k in 0..3 {
                let src = [color.r, color.g, color.b][k] as f32 * a;
                p[k] = (src + p[k] as f32 * (1.0 - a) + 0.5) as u8;
            }
            p[3] = (255.0 * a + p[3] as f32 * (1.0 - a) + 0.5) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32) -> Vec<u8> {
        vec![0; (w * h * 4) as usize]
    }

    fn alpha_at(f: &[u8], w: u32, x: u32, y: u32) -> u8 {
        f[((y * w + x) * 4 + 3) as usize]
    }

    #[test]
    fn dab_stamp_center_solid_edge_fades() {
        let mut f = frame(32, 32);
        let dab = Dab {
            x: 16.0,
            y: 16.0,
            radius: 8.0,
            hardness: 0.5,
            color: Color::BLACK,
            alpha: 1.0,
            mode: crate::stroke::DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 1.0,
            angle: 0.0,
            dual: None,
        };
        stamp_dabs_flat(&mut f, 32, Rect::new(0, 0, 32, 32), &[dab]);
        assert_eq!(alpha_at(&f, 32, 16, 16), 255, "中心实心");
        let edge = alpha_at(&f, 32, 23, 16);
        assert!(edge > 0 && edge < 255, "硬边外渐变: {edge}");
        assert_eq!(alpha_at(&f, 32, 30, 16), 0, "半径外不受影响");
    }

    #[test]
    fn rect_fill_clipped_to_region() {
        let mut f = frame(16, 16);
        fill_rect_flat(
            &mut f,
            16,
            Rect::new(0, 0, 16, 16),
            Rect::new(2, 2, 8, 8),
            Color::BLACK,
            1.0,
        );
        assert_eq!(alpha_at(&f, 16, 4, 4), 255, "矩形内");
        assert_eq!(alpha_at(&f, 16, 1, 1), 0, "矩形外");
        // region 外的矩形部分被裁剪
        let mut g = frame(16, 16);
        fill_rect_flat(
            &mut g,
            16,
            Rect::new(0, 0, 4, 16),
            Rect::new(2, 2, 8, 8),
            Color::BLACK,
            1.0,
        );
        assert_eq!(alpha_at(&g, 16, 3, 4), 255, "region 内");
        assert_eq!(alpha_at(&g, 16, 5, 4), 0, "region 外");
    }

    #[test]
    fn ellipse_fill_center_only() {
        let mut f = frame(32, 32);
        fill_ellipse_flat(&mut f, 32, Rect::new(0, 0, 32, 32), 16.0, 16.0, 8.0, 4.0, Color::BLACK, 1.0);
        assert_eq!(alpha_at(&f, 32, 16, 16), 255, "中心");
        assert_eq!(alpha_at(&f, 32, 16, 21), 0, "纵向半径外");
    }
}
