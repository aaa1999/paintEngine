/// 指针类型。Eraser 指数位笔的橡皮端（压感橡皮在 P1 工具化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    Pen,
    Eraser,
    Touch,
    Mouse,
}

/// 归一化指针采样。坐标为屏幕物理像素；
/// 画布换算由 [`crate::viewport::Viewport`] 负责。
#[derive(Debug, Clone, Copy)]
pub struct PointerSample {
    pub x: f64,
    pub y: f64,
    /// 归一化 0..=1。鼠标无压感 → None（按满压处理）。
    pub pressure: Option<f32>,
    /// 弧度。
    pub tilt: Option<(f32, f32)>,
    pub kind: PointerKind,
    /// 指针唯一标识（多指追踪与手势识别用）。
    pub id: u64,
    /// 微秒时间戳。Web `pointerrawupdate` 与 Android 历史点
    /// 都会一次给一串不同时刻的采样，稳定/速度计算依赖它。
    pub t_us: u64,
}

impl PointerSample {
    pub fn mouse(x: f64, y: f64) -> Self {
        Self {
            x,
            y,
            pressure: None,
            tilt: None,
            kind: PointerKind::Mouse,
            id: 0,
            t_us: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    Cancel,
}

/// 平台无关事件。各平台壳负责把原生事件转成这个表示。
#[derive(Debug)]
pub enum PlatformEvent {
    Pointer {
        phase: PointerPhase,
        sample: PointerSample,
    },
    /// 数位笔在感应范围内。true 期间忽略触摸（手掌拒绝）。
    PenInRange(bool),
    Resize {
        w: u32,
        h: u32,
        /// DPI 缩放（物理/逻辑像素）。
        scale: f32,
    },
    /// 失焦时取消进行中的笔画。
    Focus(bool),
}
