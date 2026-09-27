use crate::color::Color;
use crate::input::PointerSample;

/// dab（笔尖印章）的合成方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DabMode {
    /// 逐次叠加：每个 dab 独立 source-over，重叠处加深。
    Buildup,
    /// 整笔同浓：重叠处取最大覆盖（无独立笔画缓冲时的近似，
    /// 同色笔在 M1 表现正确）。
    Wash,
}

/// 一次印章。坐标为画布像素。
#[derive(Debug, Clone, Copy)]
pub struct Dab {
    pub x: f64,
    pub y: f64,
    pub radius: f32,
    /// 0（全软）..=1（硬边）。
    pub hardness: f32,
    pub color: Color,
    /// 本 dab 的整体不透明度 0..=1。
    pub alpha: f32,
    pub mode: DabMode,
    /// true = dst-out（橡皮擦），忽略 color/mode。
    pub erase: bool,
}

/// 一笔的进行时状态（平滑位置、间距游标）。
#[derive(Debug, Clone)]
pub struct StrokeState {
    /// EMA 平滑后的当前位置。
    smooth: (f64, f64),
    /// 最近一次原始样本位置（稳定器收笔追赶的目标）。
    raw: (f64, f64),
    /// 上一个已发射 dab 的位置。
    since_dab: (f64, f64),
    last_radius: f32,
}

impl StrokeState {
    pub fn new(x: f64, y: f64, radius: f32) -> Self {
        Self {
            smooth: (x, y),
            raw: (x, y),
            since_dab: (x, y),
            last_radius: radius,
        }
    }

    /// 平滑后的当前位置（调试/测试用）。
    pub fn position(&self) -> (f64, f64) {
        self.smooth
    }
}

/// 采样流 → dab 流：间距切分、压感映射、平滑。
/// 自定义笔型（纹理笔等 P2）实现此 trait 即可接入。
pub trait StrokeGen {
    fn begin(&self, state: &mut StrokeState, sample: &PointerSample) -> Vec<Dab>;
    fn extend(&self, state: &mut StrokeState, sample: &PointerSample) -> Vec<Dab>;
    fn end(&self, _state: &mut StrokeState) -> Vec<Dab> {
        Vec::new()
    }
}

fn lerp(a: f64, b: f64, f: f64) -> f64 {
    a + (b - a) * f
}

/// 内置圆头笔。参数语义对齐主流绘画软件：
/// `size` 满压直径；`opacity` 整笔不透明度上限；
/// `flow` 单 dab 不透明度；`spacing` 间距占直径比例。
#[derive(Debug, Clone)]
pub struct RoundBrush {
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub flow: f32,
    pub spacing: f32,
    /// 0..=0.95，EMA 位置平滑强度。
    pub smoothing: f32,
    /// 0..=0.98 磁吸稳定器：强滞后抑抖，收笔时直线追赶补齐终点。
    /// 0 = 关闭（保持原手感）。
    pub stabilizer: f32,
    /// 压感→半径的 gamma 曲线，1 为线性。
    pub pressure_gamma: f32,
    pub color: Color,
    pub mode: DabMode,
}

impl Default for RoundBrush {
    fn default() -> Self {
        Self {
            size: 12.0,
            hardness: 0.5,
            opacity: 1.0,
            flow: 1.0,
            spacing: 0.15,
            smoothing: 0.35,
            stabilizer: 0.0,
            pressure_gamma: 1.0,
            color: Color::BLACK,
            mode: DabMode::Buildup,
        }
    }
}

impl RoundBrush {
    fn radius_at(&self, pressure: Option<f32>) -> f32 {
        let p = pressure
            .unwrap_or(1.0)
            .clamp(0.0, 1.0)
            .powf(self.pressure_gamma);
        (self.size * 0.5 * p).max(0.5)
    }

    /// 单 dab 的 alpha：Buildup 逐 dab 叠加用 flow；
    /// Wash 整笔同浓，直接以 opacity 为目标覆盖。
    fn dab_alpha(&self) -> f32 {
        match self.mode {
            DabMode::Buildup => self.flow.clamp(0.0, 1.0),
            DabMode::Wash => self.opacity.clamp(0.0, 1.0),
        }
    }

    fn make_dab(&self, x: f64, y: f64, radius: f32) -> Dab {
        Dab {
            x,
            y,
            radius,
            hardness: self.hardness.clamp(0.0, 1.0),
            color: self.color,
            alpha: self.dab_alpha(),
            mode: self.mode,
            erase: false,
        }
    }
}

impl StrokeGen for RoundBrush {
    fn begin(&self, state: &mut StrokeState, sample: &PointerSample) -> Vec<Dab> {
        let r = self.radius_at(sample.pressure);
        *state = StrokeState::new(sample.x, sample.y, r);
        vec![self.make_dab(sample.x, sample.y, r)]
    }

    fn extend(&self, state: &mut StrokeState, sample: &PointerSample) -> Vec<Dab> {
        state.raw = (sample.x, sample.y);
        // 稳定器与轻平滑叠加：stabilizer 主导时每事件只前进 (1-stab) 比例，
        // 高频输入下表现为强磁吸；收笔由 end() 直线追赶补齐
        let stab = self.stabilizer.clamp(0.0, 0.98) as f64;
        let k = (1.0 - self.smoothing.clamp(0.0, 0.95) as f64) * (1.0 - stab);
        state.smooth.0 = lerp(state.smooth.0, sample.x, k);
        state.smooth.1 = lerp(state.smooth.1, sample.y, k);

        let target = self.radius_at(sample.pressure);
        let mut dabs = Vec::new();
        let (mut px, mut py) = state.since_dab;
        let mut radius = state.last_radius;

        loop {
            let step = (self.spacing * (radius + target)).max(0.75) as f64;
            let dx = state.smooth.0 - px;
            let dy = state.smooth.1 - py;
            let remain = (dx * dx + dy * dy).sqrt();
            if remain < f64::EPSILON || step > remain {
                break;
            }
            let f = step / remain;
            px = lerp(px, state.smooth.0, f);
            py = lerp(py, state.smooth.1, f);
            radius = lerp(radius as f64, target as f64, f) as f32;
            dabs.push(self.make_dab(px, py, radius));
        }

        state.since_dab = (px, py);
        state.last_radius = radius;
        dabs
    }

    fn end(&self, state: &mut StrokeState) -> Vec<Dab> {
        // 稳定器滞后补偿：从当前平滑位置到最终原始样本直线补 dab
        let (tx, ty) = state.raw;
        let mut px = state.since_dab.0;
        let mut py = state.since_dab.1;
        let radius = state.last_radius;
        let mut dabs = Vec::new();
        loop {
            let step = (self.spacing * (radius + state.last_radius)).max(0.75) as f64;
            let dx = tx - px;
            let dy = ty - py;
            let remain = (dx * dx + dy * dy).sqrt();
            if remain < f64::EPSILON || step > remain {
                break;
            }
            let f = step / remain;
            px = lerp(px, tx, f);
            py = lerp(py, ty, f);
            dabs.push(self.make_dab(px, py, radius));
        }
        dabs.push(self.make_dab(tx, ty, state.last_radius));
        state.since_dab = (tx, ty);
        dabs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PointerKind;

    fn sample(x: f64, pressure: Option<f32>) -> PointerSample {
        PointerSample {
            x,
            y: 50.0,
            pressure,
            tilt: None,
            kind: PointerKind::Pen,
            id: 0,
            t_us: 0,
        }
    }

    #[test]
    fn spacing_splits_line() {
        // 测试确定性：关平滑；半径 5，步长 0.15*10 = 1.5px
        let brush = RoundBrush {
            smoothing: 0.0,
            size: 10.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(0.0, 0.0, 5.0);
        let first = StrokeGen::begin(&brush, &mut st, &sample(10.0, Some(1.0)));
        assert_eq!(first.len(), 1);

        let dabs = StrokeGen::extend(&brush, &mut st, &sample(100.0, Some(1.0)));
        // 90px / 1.5px ≈ 60 个
        assert!((52..=68).contains(&dabs.len()), "got {}", dabs.len());
        for (i, d) in dabs.iter().enumerate() {
            assert!((d.y - 50.0).abs() < 1e-9);
            if i > 0 {
                assert!(d.x > dabs[i - 1].x, "x 应单调递增");
            }
        }
        assert!((dabs[0].x - 11.5).abs() < 1e-6);
        assert!((dabs.last().unwrap().x - 100.0).abs() < 1.5);
    }

    #[test]
    fn pressure_maps_radius() {
        let brush = RoundBrush {
            smoothing: 0.0,
            size: 20.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(0.0, 0.0, 10.0);
        StrokeGen::begin(&brush, &mut st, &sample(0.0, Some(0.25)));
        assert!((st_radius_hint(&st) - 2.5).abs() < 1e-4);

        let dabs = StrokeGen::extend(&brush, &mut st, &sample(50.0, Some(1.0)));
        let first_r = dabs.first().map(|d| d.radius).unwrap_or(0.0);
        let last_r = dabs.last().map(|d| d.radius).unwrap_or(0.0);
        assert!(
            first_r < last_r,
            "压感增大时半径应增大: {first_r} → {last_r}"
        );

        // 鼠标无压感按满压
        let mut st2 = StrokeState::new(0.0, 0.0, 10.0);
        let d = StrokeGen::begin(&brush, &mut st2, &sample(0.0, None)).remove(0);
        assert!((d.radius - 10.0).abs() < 1e-4);
    }

    fn st_radius_hint(st: &StrokeState) -> f32 {
        st.last_radius
    }

    #[test]
    fn smoothing_pulls_toward_raw() {
        let brush = RoundBrush::default(); // smoothing 0.35
        let mut st = StrokeState::new(0.0, 0.0, 6.0);
        StrokeGen::begin(&brush, &mut st, &PointerSample::mouse(0.0, 0.0));
        StrokeGen::extend(&brush, &mut st, &PointerSample::mouse(100.0, 0.0));
        // 平滑后游标应落后于原始输入但已前进
        assert!(st.since_dab.0 < 100.0 && st.since_dab.0 > 0.0);
    }
}

#[cfg(test)]
mod stabilizer_tests {
    use super::*;
    use crate::input::PointerKind;

    fn sample(x: f64, y: f64) -> PointerSample {
        PointerSample {
            x,
            y,
            pressure: Some(1.0),
            tilt: None,
            kind: PointerKind::Pen,
            id: 0,
            t_us: 0,
        }
    }

    /// 垂直抖动轨迹的 y 方差：稳定器应显著降低。
    #[test]
    fn stabilizer_reduces_jitter() {
        let brush_off = RoundBrush {
            smoothing: 0.0,
            stabilizer: 0.0,
            ..RoundBrush::default()
        };
        let brush_on = RoundBrush {
            smoothing: 0.0,
            stabilizer: 0.9,
            ..RoundBrush::default()
        };
        let run = |brush: &RoundBrush| -> (f64, usize) {
            let mut st = StrokeState::new(0.0, 50.0, 6.0);
            StrokeGen::begin(brush, &mut st, &sample(0.0, 50.0));
            let mut ys = vec![];
            let mut dabs = 0;
            for i in 0..400 {
                // 每 2px 前进 + 交替 ±3px 抖动
                let x = (i as f64) * 2.0;
                let y = 50.0 + if i % 2 == 0 { 3.0 } else { -3.0 };
                let d = StrokeGen::extend(brush, &mut st, &sample(x, y));
                dabs += d.len();
                ys.push(st.position().1);
            }
            let mean = ys.iter().sum::<f64>() / ys.len() as f64;
            let var = ys.iter().map(|y| (y - mean).powi(2)).sum::<f64>() / ys.len() as f64;
            (var, dabs)
        };
        let (var_off, _) = run(&brush_off);
        let (var_on, dabs_on) = run(&brush_on);
        assert!(
            var_on < var_off * 0.3,
            "稳定器应显著抑抖: off={var_off:.3} on={var_on:.3}"
        );
        assert!(dabs_on > 0);
    }

    /// 收笔追赶：结束后应发射到最终原始样本位置。
    #[test]
    fn catch_up_reaches_final_point() {
        let brush = RoundBrush {
            smoothing: 0.0,
            stabilizer: 0.95,
            size: 10.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(0.0, 0.0, 5.0);
        StrokeGen::begin(&brush, &mut st, &sample(0.0, 0.0));
        // 一次大跳：平滑位置严重滞后
        StrokeGen::extend(&brush, &mut st, &sample(500.0, 0.0));
        let dabs = StrokeGen::end(&brush, &mut st);
        let last = dabs.last().expect("应有追赶 dabs");
        assert!(
            (last.x - 500.0).abs() < 1e-6,
            "终点应补到原始样本: {}",
            last.x
        );
    }
}
