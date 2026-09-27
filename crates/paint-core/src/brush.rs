//! 笔刷引擎：笔尖抽象 + 双重笔尖。
//!
//! BrushTip 是笔尖形状的采样接口（输入 dab 内归一化坐标 → 输出
//! 覆盖率 0..1）；render_dab 将 tip 采样结果与压感/流量结合，
//! 产出 stamp 需要的逐像素 alpha 行——但 stamp 热路径不接受回调
//! （性能），因此 tip 采样在 stamp.rs 里以 match 分支内联。
//! 此模块提供：tip 形状数学（供 stamp 内联）、dual brush 组合
//! 语义、以及独立 tip 采样函数（测试/UI 预览用）。

use crate::stroke::Dab;

/// 笔尖形状。
#[derive(Debug, Clone, PartialEq)]
pub enum BrushTip {
    /// 圆头（各向异性椭圆）。hardness 0..1。
    Round { hardness: f32 },
    /// 方形（带角度旋转、圆角）。corner 0..1（0=直角 1=全圆）。
    Square { corner: f32 },
    /// 纹理图（灰度 PNG，白=覆盖黑=不盖）。旋转跟随笔向或固定。
    Image {
        data: Vec<u8>,
        size: u32,
        /// 随笔旋转（false = 固定方向）。
        follow_stroke: bool,
    },
}

impl BrushTip {
    /// 归一化坐标 (x,y) ∈ [-1,1]²（考虑 aspect/angle 已由调用方换算）
    /// → 覆盖率 0..1。
    pub fn coverage(&self, x: f32, y: f32) -> f32 {
        match self {
            BrushTip::Round { hardness } => {
                let d = (x * x + y * y).sqrt();
                if d >= 1.0 {
                    0.0
                } else if d <= *hardness {
                    1.0
                } else {
                    (1.0 - d) / (1.0 - hardness).max(1e-6)
                }
            }
            BrushTip::Square { corner } => {
                // SDF 方形（含圆角）
                let (ax, ay) = (x.abs(), y.abs());
                let r = corner.clamp(0.0, 1.0) * 0.5;
                let (qx, qy) = ((ax - (1.0 - r)).max(0.0), (ay - (1.0 - r)).max(0.0));
                let d = (qx * qx + qy * qy).sqrt() - r;
                if d <= 0.0 {
                    1.0
                } else if d >= 0.1 {
                    0.0
                } else {
                    1.0 - d / 0.1
                }
            }
            BrushTip::Image { data, size, .. } => {
                if *size == 0 {
                    return 0.0;
                }
                let nx = ((x * 0.5 + 0.5) * *size as f32) as i32;
                let ny = ((y * 0.5 + 0.5) * *size as f32) as i32;
                if nx < 0 || ny < 0 || nx >= *size as i32 || ny >= *size as i32 {
                    return 0.0;
                }
                data[(ny * *size as i32 + nx) as usize] as f32 / 255.0
            }
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            BrushTip::Round { .. } => "圆头",
            BrushTip::Square { .. } => "方头",
            BrushTip::Image { .. } => "纹理",
        }
    }
}

/// 双重笔尖组合模式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DualMode {
    /// 关闭。
    Off,
    /// 交集（副笔裁剪主笔）。
    Intersect,
    /// 并集叠加。
    Union,
    /// 差集（主笔减副笔）。
    Subtract,
}

/// 双重笔尖参数（挂在 Brush 上）。
#[derive(Debug, Clone)]
pub struct DualBrush {
    pub mode: DualMode,
    pub tip: BrushTip,
    /// 副笔尺寸比例（相对主笔 0..1）。
    pub size_ratio: f32,
    /// 副笔旋转偏移（弧度）。
    pub angle_offset: f32,
    /// 副笔间距比例（更密=更明显纹理）。
    pub spacing_ratio: f32,
}

impl Default for DualBrush {
    fn default() -> Self {
        Self {
            mode: DualMode::Off,
            tip: BrushTip::Square { corner: 0.0 },
            size_ratio: 0.8,
            angle_offset: 0.0,
            spacing_ratio: 1.0,
        }
    }
}

impl DualBrush {
    /// 双笔尖合成覆盖率：主覆盖率 × 副笔在该点的覆盖率（按组合模式）。
    /// 主笔坐标 (mx,my)、副笔坐标 (sx,sy) 均为归一化系。
    pub fn combine(&self, main_cov: f32, dual_x: f32, dual_y: f32) -> f32 {
        if self.mode == DualMode::Off {
            return main_cov;
        }
        let dual = self.tip.coverage(dual_x, dual_y);
        match self.mode {
            DualMode::Off => main_cov,
            DualMode::Intersect => main_cov * dual,
            DualMode::Union => (main_cov + dual - main_cov * dual).min(1.0),
            DualMode::Subtract => (main_cov * (1.0 - dual)).max(0.0),
        }
    }
}

/// 独立 dab 覆盖率采样（UI 预览/测试用；热路径在 stamp.rs 内联）。
pub fn dab_coverage(dab: &Dab, px: f64, py: f64) -> f32 {
    let tip = match &dab.tip {
        Some(t) => BrushTip::Image {
            data: t.data.clone(),
            size: t.size,
            follow_stroke: true,
        },
        None => BrushTip::Round {
            hardness: dab.hardness,
        },
    };
    // 画布坐标 → 归一化（各向异性）
    let dx0 = px - dab.x;
    let dy0 = py - dab.y;
    let aspect = dab.aspect.clamp(0.05, 1.0);
    let (ca, sa) = if aspect < 1.0 {
        (dab.angle.cos() as f64, dab.angle.sin() as f64)
    } else {
        (1.0, 0.0)
    };
    let dx = ca * dx0 + sa * dy0;
    let dy = -sa * dx0 + ca * dy0;
    let nx = (dx / dab.radius.max(0.001) as f64) as f32;
    let ny = (dy / (dab.radius.max(0.001) * aspect) as f64) as f32;
    tip.coverage(nx, ny)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_tip_coverage() {
        let tip = BrushTip::Round { hardness: 0.5 };
        assert_eq!(tip.coverage(0.0, 0.0), 1.0, "中心满覆盖");
        assert_eq!(tip.coverage(1.1, 0.0), 0.0, "外部零");
        let half = tip.coverage(0.75, 0.0);
        assert!(
            half > 0.0 && half < 1.0,
            "硬度 0.5 下 0.75 处半覆盖: {half}"
        );
    }

    #[test]
    fn square_tip_coverage() {
        let tip = BrushTip::Square { corner: 0.0 };
        assert_eq!(tip.coverage(0.5, 0.5), 1.0, "方形内");
        assert_eq!(tip.coverage(0.99, 0.99), 1.0, "角内");
        assert_eq!(tip.coverage(0.5, 1.2), 0.0, "方形外");
        // 圆角版：角被圆化
        let rounded = BrushTip::Square { corner: 1.0 };
        let corner_cov = rounded.coverage(0.95, 0.95);
        assert!(corner_cov < 1.0, "全圆角时角不覆盖: {corner_cov}");
    }

    #[test]
    fn image_tip_coverage() {
        // 4×4 左白右黑
        let mut data = vec![0u8; 16];
        for y in 0..4 {
            for x in 0..4 {
                data[y * 4 + x] = if x < 2 { 255 } else { 0 };
            }
        }
        let tip = BrushTip::Image {
            data,
            size: 4,
            follow_stroke: true,
        };
        assert_eq!(tip.coverage(-0.5, 0.0), 1.0, "左半覆盖");
        assert_eq!(tip.coverage(0.5, 0.0), 0.0, "右半不覆盖");
    }

    #[test]
    fn dual_brush_combine() {
        let mut dual = DualBrush {
            mode: DualMode::Intersect,
            tip: BrushTip::Square { corner: 0.0 },
            ..DualBrush::default()
        };
        // 主笔全覆盖，副笔方形：中心(方形内)=1，(0.9,0.95)(方形外)=0
        assert_eq!(dual.combine(1.0, 0.0, 0.0), 1.0);
        assert_eq!(dual.combine(1.0, 1.2, 0.0), 0.0, "方形外裁剪为 0");

        dual.mode = DualMode::Union;
        assert!(dual.combine(0.0, 0.0, 0.0) > 0.99, "并集补全覆盖");

        dual.mode = DualMode::Subtract;
        assert_eq!(dual.combine(1.0, 0.0, 0.0), 0.0, "差集全减");
        assert_eq!(dual.combine(1.0, 1.5, 0.0), 1.0, "副笔外不受影响");

        dual.mode = DualMode::Off;
        assert_eq!(dual.combine(0.3, 0.0, 0.0), 0.3, "关闭时主笔直通");
    }

    #[test]
    fn dab_coverage_anisotropic() {
        let dab = Dab {
            x: 50.0,
            y: 50.0,
            radius: 10.0,
            hardness: 1.0,
            color: crate::color::Color::BLACK,
            alpha: 1.0,
            mode: crate::stroke::DabMode::Buildup,
            erase: false,
            tip: None,
            scatter: 0.0,
            aspect: 0.5,
            angle: 0.0,
            dual: None,
        };
        // 长轴 ±8 内、短轴 ±4 内
        assert_eq!(dab_coverage(&dab, 58.0, 50.0), 1.0);
        assert_eq!(dab_coverage(&dab, 50.0, 54.0), 1.0);
        assert_eq!(dab_coverage(&dab, 50.0, 56.0), 0.0, "短轴外");
    }
}
