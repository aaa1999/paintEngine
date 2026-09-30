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

/// 纹理笔刷尖：灰度图（0=不盖 255=全盖），最近邻采样。
#[derive(Debug, Clone)]
pub struct TipTexture {
    pub data: Vec<u8>,
    pub size: u32,
}

impl TipTexture {
    /// 从 PNG 构造（取亮度）。
    pub fn from_png(bytes: &[u8]) -> Option<Self> {
        let (rgba, w, h) = crate::io::decode_png(bytes).ok()?;
        if w == 0 || h == 0 || w != h {
            return None;
        }
        let mut data = vec![0u8; (w * h) as usize];
        for (d, px) in data.iter_mut().zip(rgba.as_chunks::<4>().0) {
            *d = ((px[0] as u32 * 30 + px[1] as u32 * 59 + px[2] as u32 * 11) / 100) as u8;
        }
        Some(Self { data, size: w })
    }

    #[inline]
    /// 纹理采样（最近邻）。
    pub fn sample(&self, x: u32, y: u32) -> f32 {
        self.data[(y * self.size + x) as usize] as f32 / 255.0
    }
}

/// 一次印章。坐标为画布像素。
#[derive(Debug, Clone)]
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
    /// 纹理笔刷尖（Some 时 alpha 蒙版来自尖图采样，取代径向曲线）。
    pub tip: Option<std::sync::Arc<TipTexture>>,
    /// 尖图随机散布强度 0..1（相对半径的比例偏移，dab 坐标哈希定种子）。
    pub scatter: f32,
    /// 各向异性：短轴/长轴半径比（1.0 = 圆形；tilt 笔刷 < 1）。
    pub aspect: f32,
    /// 长轴方向（弧度；长轴 ⟂ 笔倾斜方向，模拟笔尖排线）。
    pub angle: f32,
    /// 双重笔尖（Box 控制热结构尺寸；None = 单笔尖）。
    pub dual: Option<Box<crate::brush::DualBrush>>,
}

/// 样条控制点：EMA 平滑后的位置 + 该点的目标半径/倾斜。
#[derive(Debug, Clone, Copy)]
struct CurvePt {
    pos: (f64, f64),
    radius: f32,
    tilt: (f32, f32),
}

/// 一笔的进行时状态（平滑位置、间距游标、样条缓冲）。
#[derive(Debug, Clone)]
pub struct StrokeState {
    /// EMA 平滑后的当前位置。
    smooth: (f64, f64),
    /// 最近一次原始样本位置（稳定器收笔追赶的目标）。
    raw: (f64, f64),
    /// 插值中的倾斜向量（弧度分量，(0,0) = 笔直立无倾斜）。
    tilt: (f32, f32),
    /// 上一个已发射 dab 的位置。
    since_dab: (f64, f64),
    last_radius: f32,
    /// 样条待定稿点（Catmull-Rom 过点曲线）：段 p1→p2 需右邻 p3 才能定稿，
    /// 故定稿滞后一个采样点；每次 extend 后裁剪保留尾部 2 点。
    curve: Vec<CurvePt>,
    /// curve[0] 的左邻点（首段重复自身）。
    curve_prev: CurvePt,
}

impl StrokeState {
    /// 初始状态（位置 x,y 半径 radius）。
    pub fn new(x: f64, y: f64, radius: f32) -> Self {
        let p0 = CurvePt {
            pos: (x, y),
            radius,
            tilt: (0.0, 0.0),
        };
        Self {
            smooth: (x, y),
            raw: (x, y),
            tilt: (0.0, 0.0),
            since_dab: (x, y),
            last_radius: radius,
            curve: vec![p0],
            curve_prev: p0,
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

/// Catmull-Rom 样条（张力 0.5）：过 p1、p2 的段，p0/p3 提供端点切向。
/// 转弯处 dab 链沿曲线行进——消除"输入点折线"的折角痕迹。
fn catmull_rom(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let t2 = t * t;
    let t3 = t2 * t;
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
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
    /// 纹理笔刷尖（Image tip 快捷方式；与 brush_tip 二选一）。
    pub tip: Option<std::sync::Arc<TipTexture>>,
    /// 尖图散布强度 0..1。
    pub scatter: f32,
    /// 笔倾斜灵敏度 0..1：倾斜→各向异性笔形（书法效果），0 关闭。
    pub tilt_sensitivity: f32,
    /// 笔尖形状（None = 由 hardness 决定的圆头；Some 时覆盖）。
    pub brush_tip: Option<crate::brush::BrushTip>,
    /// 双重笔尖。
    pub dual: crate::brush::DualBrush,
}

impl Default for RoundBrush {
    fn default() -> Self {
        Self {
            size: 12.0,
            hardness: 0.5,
            opacity: 1.0,
            flow: 1.0,
            spacing: 0.07,
            smoothing: 0.5,
            stabilizer: 0.0,
            pressure_gamma: 1.0,
            color: Color::BLACK,
            mode: DabMode::Buildup,
            tip: None,
            scatter: 0.0,
            tilt_sensitivity: 0.0,
            brush_tip: None,
            dual: crate::brush::DualBrush::default(),
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

    fn make_dab(&self, x: f64, y: f64, radius: f32, tilt: (f32, f32)) -> Dab {
        // 倾斜 → 各向异性：长轴 ⟂ 倾斜方向（笔尖排线），强度受灵敏度调制
        let sens = self.tilt_sensitivity.clamp(0.0, 1.0);
        let mag = ((tilt.0.hypot(tilt.1)) / (std::f32::consts::FRAC_PI_2)) * sens;
        let mag = mag.clamp(0.0, 0.98);
        let (aspect, angle) = if mag > 0.01 {
            (
                1.0 / (1.0 + mag * 1.5),
                tilt.1.atan2(tilt.0) + std::f32::consts::FRAC_PI_2,
            )
        } else {
            (1.0, 0.0)
        };
        Dab {
            x,
            y,
            radius,
            hardness: self.hardness.clamp(0.0, 1.0),
            color: self.color,
            alpha: self.dab_alpha(),
            mode: self.mode,
            erase: false,
            tip: self.tip.clone(),
            scatter: self.scatter,
            aspect,
            angle,
            dual: if self.dual.mode == crate::brush::DualMode::Off {
                None
            } else {
                Some(Box::new(self.dual.clone()))
            },
        }
    }
}

impl RoundBrush {
    /// 从当前游标向目标点按间距步进发射 dab（半径/倾斜沿线插值）。
    fn walk_toward(
        &self,
        state: &mut StrokeState,
        tx: f64,
        ty: f64,
        t_radius: f32,
        t_tilt: (f32, f32),
        dabs: &mut Vec<Dab>,
    ) {
        let (mut px, mut py) = state.since_dab;
        let mut radius = state.last_radius;
        let mut tilt = state.tilt;
        loop {
            let step = (self.spacing * (radius + t_radius)).max(0.75) as f64;
            let dx = tx - px;
            let dy = ty - py;
            let remain = (dx * dx + dy * dy).sqrt();
            if remain < f64::EPSILON || step > remain {
                break;
            }
            let f = step / remain;
            px = lerp(px, tx, f);
            py = lerp(py, ty, f);
            radius = lerp(radius as f64, t_radius as f64, f) as f32;
            tilt.0 += (t_tilt.0 - tilt.0) * f as f32;
            tilt.1 += (t_tilt.1 - tilt.1) * f as f32;
            dabs.push(self.make_dab(px, py, radius, tilt));
        }
        state.since_dab = (px, py);
        state.last_radius = radius;
        state.tilt = tilt;
    }

    /// 定稿一段样条（p1→p2，p0/p3 为邻点）：稠密采样后沿精确弧长步进。
    /// 退化段（近零长）直线步进；稠密点钳制进段 AABB（防端点切向越冲）。
    fn finalize_segment(
        &self,
        state: &mut StrokeState,
        p0: CurvePt,
        p1: CurvePt,
        p2: CurvePt,
        p3: CurvePt,
        dabs: &mut Vec<Dab>,
    ) {
        let dist = (p2.pos.0 - p1.pos.0).hypot(p2.pos.1 - p1.pos.1);
        if dist < 1e-6 {
            self.walk_toward(state, p2.pos.0, p2.pos.1, p2.radius, p2.tilt, dabs);
            return;
        }
        let step_hint = (self.spacing * (p1.radius + p2.radius)).max(0.75) as f64;
        let n = ((dist / step_hint * 2.0) as usize).clamp(8, 64);
        // 越冲余量：段长的 1/4 + 0.5px（端点重复/近重复时切向过猛）
        let m = dist * 0.25 + 0.5;
        let (x_lo, x_hi) = (p1.pos.0.min(p2.pos.0) - m, p1.pos.0.max(p2.pos.0) + m);
        let (y_lo, y_hi) = (p1.pos.1.min(p2.pos.1) - m, p1.pos.1.max(p2.pos.1) + m);
        for i in 1..=n {
            let t = i as f64 / n as f64;
            let x = catmull_rom(p0.pos.0, p1.pos.0, p2.pos.0, p3.pos.0, t).clamp(x_lo, x_hi);
            let y = catmull_rom(p0.pos.1, p1.pos.1, p2.pos.1, p3.pos.1, t).clamp(y_lo, y_hi);
            let r = lerp(p1.radius as f64, p2.radius as f64, t) as f32;
            let tilt = (
                lerp(p1.tilt.0 as f64, p2.tilt.0 as f64, t) as f32,
                lerp(p1.tilt.1 as f64, p2.tilt.1 as f64, t) as f32,
            );
            self.walk_toward(state, x, y, r, tilt, dabs);
        }
    }
}

impl StrokeGen for RoundBrush {
    fn begin(&self, state: &mut StrokeState, sample: &PointerSample) -> Vec<Dab> {
        let r = self.radius_at(sample.pressure);
        *state = StrokeState::new(sample.x, sample.y, r);
        state.tilt = sample.tilt.unwrap_or((0.0, 0.0));
        let tilt = state.tilt;
        vec![self.make_dab(sample.x, sample.y, r, tilt)]
    }

    fn extend(&self, state: &mut StrokeState, sample: &PointerSample) -> Vec<Dab> {
        state.raw = (sample.x, sample.y);
        let tilt_target = sample.tilt.unwrap_or((0.0, 0.0));
        // 稳定器与轻平滑叠加：stabilizer 主导时每事件只前进 (1-stab) 比例，
        // 高频输入下表现为强磁吸；收笔由 end() 直线追赶补齐
        let stab = self.stabilizer.clamp(0.0, 0.98) as f64;
        let k = (1.0 - self.smoothing.clamp(0.0, 0.95) as f64) * (1.0 - stab);
        state.smooth.0 = lerp(state.smooth.0, sample.x, k);
        state.smooth.1 = lerp(state.smooth.1, sample.y, k);

        let pt = CurvePt {
            pos: state.smooth,
            radius: self.radius_at(sample.pressure),
            tilt: tilt_target,
        };
        let mut dabs = Vec::new();
        // 缓冲满 2 点且有右邻（新点）→ 定稿首段（滞后一个采样点，~8-16ms）
        if state.curve.len() == 2 {
            let (p1, p2) = (state.curve[0], state.curve[1]);
            let left = state.curve_prev;
            self.finalize_segment(state, left, p1, p2, pt, &mut dabs);
            state.curve_prev = p1;
            state.curve.remove(0);
        }
        state.curve.push(pt);
        dabs
    }

    fn end(&self, state: &mut StrokeState) -> Vec<Dab> {
        let mut dabs = Vec::new();
        // 末段定稿（右端切向重复自身 → 自然收直）
        if state.curve.len() == 2 {
            let (p1, p2) = (state.curve[0], state.curve[1]);
            let left = state.curve_prev;
            self.finalize_segment(state, left, p1, p2, p2, &mut dabs);
            state.curve.truncate(1);
        }
        // 稳定器滞后补偿：从当前游标到最终原始样本直线补 dab
        let (tx, ty) = state.raw;
        self.walk_toward(
            state,
            tx,
            ty,
            state.last_radius,
            state.tilt,
            &mut dabs,
        );
        let (lx, ly) = state.since_dab;
        if (tx - lx).hypot(ty - ly) > 1e-9 {
            dabs.push(self.make_dab(tx, ty, state.last_radius, state.tilt));
            state.since_dab = (tx, ty);
        }
        dabs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PointerKind;

    fn sample_xy(x: f64, y: f64) -> PointerSample {
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
    fn spline_rounds_sharp_corner() {
        // 直角折线输入（无平滑/稳定器）：dab 链沿样条行进——
        // 相邻 dab 方向的单步转角被限制；折线链在角点会一次跳 ~90°
        let brush = RoundBrush {
            smoothing: 0.0,
            stabilizer: 0.0,
            size: 10.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(0.0, 0.0, 5.0);
        StrokeGen::begin(&brush, &mut st, &sample_xy(0.0, 0.0));
        let mut dabs = Vec::new();
        // 水平段 (0,0)→(100,0)，每 10px 一点
        for x in (10..=100).step_by(10) {
            dabs.extend(StrokeGen::extend(&brush, &mut st, &sample_xy(x as f64, 0.0)));
        }
        // 垂直段 (100,0)→(100,100)
        for y in (10..=100).step_by(10) {
            dabs.extend(StrokeGen::extend(&brush, &mut st, &sample_xy(100.0, y as f64)));
        }
        dabs.extend(StrokeGen::end(&brush, &mut st));
        assert!(dabs.len() > 50, "应有充分 dab: {}", dabs.len());
        // 单步转向角：样条应把 90° 摊到转角附近多个步上
        let mut max_turn = 0.0f64;
        for w in dabs.windows(3) {
            let (ax, ay) = (w[1].x - w[0].x, w[1].y - w[0].y);
            let (bx, by) = (w[2].x - w[1].x, w[2].y - w[1].y);
            let (la, lb) = (ax.hypot(ay), bx.hypot(by));
            if la < 1e-9 || lb < 1e-9 {
                continue;
            }
            let cos = ((ax * bx + ay * by) / (la * lb)).clamp(-1.0, 1.0);
            max_turn = max_turn.max(cos.acos().to_degrees());
        }
        assert!(
            max_turn < 45.0,
            "样条应圆化转角（单步转角 {max_turn:.1}°；折线链为 ~90°）"
        );
        // 转角确实被圆化：路径在角点 (100,0) 附近内切（到角点距离 > 步长）
        let near_corner: Vec<(f64, f64)> = dabs
            .iter()
            .filter(|d| (d.x - 100.0).abs() < 15.0 && (d.y.abs() < 15.0))
            .map(|d| (d.x, d.y))
            .collect();
        assert!(near_corner.len() >= 3, "转角附近应有过渡 dab");
    }

    #[test]
    fn spacing_splits_line() {
        // 测试确定性：关平滑；半径 5，步长 0.15*10 = 1.5px
        let brush = RoundBrush {
            smoothing: 0.0,
            spacing: 0.15, // 显式间距保持数值确定（默认已改 0.07）
            size: 10.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(0.0, 0.0, 5.0);
        let first = StrokeGen::begin(&brush, &mut st, &sample(10.0, Some(1.0)));
        assert_eq!(first.len(), 1);

        // 样条定稿滞后一个采样点：补充第三点（同目标）触发段定稿
        let mut dabs = StrokeGen::extend(&brush, &mut st, &sample(100.0, Some(1.0)));
        dabs.extend(StrokeGen::extend(&brush, &mut st, &sample(100.0, Some(1.0))));
        dabs.extend(StrokeGen::end(&brush, &mut st));
        // 90px / 1.5px ≈ 60 个
        assert!((52..=70).contains(&dabs.len()), "got {}", dabs.len());
        for (i, d) in dabs.iter().enumerate() {
            assert!((d.y - 50.0).abs() < 1e-9);
            if i > 0 {
                assert!(d.x >= dabs[i - 1].x, "x 应单调不减");
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

        let mut dabs = StrokeGen::extend(&brush, &mut st, &sample(50.0, Some(1.0)));
        dabs.extend(StrokeGen::extend(&brush, &mut st, &sample(50.0, Some(1.0))));
        dabs.extend(StrokeGen::end(&brush, &mut st));
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
        StrokeGen::extend(&brush, &mut st, &PointerSample::mouse(60.0, 0.0));
        StrokeGen::extend(&brush, &mut st, &PointerSample::mouse(100.0, 0.0));
        StrokeGen::end(&brush, &mut st);
        // 平滑后游标应落后于原始输入但已前进
        assert!(st.since_dab.0 <= 100.0 && st.since_dab.0 > 0.0);
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

#[cfg(test)]
mod tip_tests {
    use super::*;
    use crate::input::PointerKind;
    use std::sync::Arc;

    #[test]
    fn tip_from_png_and_stamp() {
        // 8×8 左半白右半黑的尖图
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let v = if x < 4 { 255 } else { 0 };
                let i = (y * 8 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let png = crate::io::encode_png(&rgba, 8, 8).unwrap();
        let tip = TipTexture::from_png(&png).unwrap();
        assert_eq!(tip.size, 8);
        assert_eq!(tip.sample(0, 0), 1.0);
        assert_eq!(tip.sample(7, 0), 0.0);

        // 盖章：中心 dab 半径 8 → 左半有墨右半无
        let brush = RoundBrush {
            size: 16.0,
            smoothing: 0.0,
            tip: Some(Arc::new(tip)),
            ..RoundBrush::default()
        };
        let sample = PointerSample {
            x: 32.0,
            y: 32.0,
            pressure: Some(1.0),
            tilt: None,
            kind: PointerKind::Pen,
            id: 0,
            t_us: 0,
        };
        let mut st = StrokeState::new(32.0, 32.0, 8.0);
        let dabs = StrokeGen::begin(&brush, &mut st, &sample);
        assert_eq!(dabs.len(), 1);
        let d = &dabs[0];
        assert!(d.tip.is_some());
        // 蒙版值：dab 左侧 (x < 32) 应有非零 alpha，右侧无
        // （真正的像素验证在 paint-render 侧；这里验证 dab 携带 tip）
    }
}

#[cfg(test)]
mod tilt_stroke_tests {
    use super::*;
    use crate::input::PointerKind;

    fn pen_sample(x: f64, y: f64, tilt: (f32, f32)) -> PointerSample {
        PointerSample {
            x,
            y,
            pressure: Some(1.0),
            tilt: Some(tilt),
            kind: PointerKind::Pen,
            id: 0,
            t_us: 0,
        }
    }

    #[test]
    fn tilt_produces_anisotropic_dab() {
        let brush = RoundBrush {
            smoothing: 0.0,
            tilt_sensitivity: 1.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(50.0, 50.0, 6.0);
        let dabs = StrokeGen::begin(&brush, &mut st, &pen_sample(50.0, 50.0, (0.6, 0.0)));
        let d = &dabs[0];
        // 倾斜沿 +x → 长轴 ⟂ x（angle≈π/2），aspect < 1
        assert!(d.aspect < 0.9, "aspect={}", d.aspect);
        assert!((d.angle - std::f32::consts::FRAC_PI_2).abs() < 0.01);

        // 无倾斜（鼠标）→ 圆形
        let mut st2 = StrokeState::new(0.0, 0.0, 6.0);
        let d2 = StrokeGen::begin(&brush, &mut st2, &PointerSample::mouse(0.0, 0.0));
        assert_eq!(d2[0].aspect, 1.0);

        // 灵敏度 0 → 倾斜不影响
        let off = RoundBrush {
            smoothing: 0.0,
            tilt_sensitivity: 0.0,
            ..RoundBrush::default()
        };
        let mut st3 = StrokeState::new(0.0, 0.0, 6.0);
        let d3 = StrokeGen::begin(&off, &mut st3, &pen_sample(0.0, 0.0, (1.2, 0.0)));
        assert_eq!(d3[0].aspect, 1.0);
    }

    #[test]
    fn tilt_interpolates_along_stroke() {
        let brush = RoundBrush {
            smoothing: 0.0,
            tilt_sensitivity: 1.0,
            size: 10.0,
            ..RoundBrush::default()
        };
        let mut st = StrokeState::new(10.0, 50.0, 5.0);
        StrokeGen::begin(&brush, &mut st, &pen_sample(10.0, 50.0, (0.0, 0.0)));
        let mut dabs =
            StrokeGen::extend(&brush, &mut st, &pen_sample(60.0, 50.0, (1.4, 0.0)));
        dabs.extend(
            StrokeGen::extend(&brush, &mut st, &pen_sample(60.0, 50.0, (1.4, 0.0))),
        );
        dabs.extend(StrokeGen::end(&brush, &mut st));
        assert!(!dabs.is_empty());
        // 中途 dab 的 aspect 应介于两端之间（首个接近 1，渐向 <0.7）
        let first = dabs.first().unwrap().aspect;
        let last = dabs.last().unwrap().aspect;
        assert!(first > last, "倾斜沿笔画增强: {first} → {last}");
        assert!(last < 0.75);
    }
}
