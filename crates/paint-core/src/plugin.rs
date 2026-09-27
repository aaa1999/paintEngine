//! 插件系统：宿主扩展点。
//!
//! M5 第一期：**编译期插件**（Rust trait 对象，宿主链接注册）。
//! 设计为运行时插件（WASM 模块/脚本）预留同一注册接口——
//! 宿主代码只依赖 [`PluginRegistry`]，不关心插件来源。
//!
//! 三个扩展点：
//! - [`TipPlugin`]：自定义笔尖形状（coverage 采样）
//! - [`FilterPlugin`]：自定义滤镜（逐像素 RGBA 变换）
//! - [`ToolPlugin`]：自定义工具（指针事件序列 → 引擎动作）
//!
//! 安全边界：插件只接触归一化坐标/像素缓冲/事件流，不接触
//! 引擎内部结构（撤销/图层栈由引擎侧代理执行）。

use crate::input::{PointerPhase, PointerSample};
use std::collections::HashMap;
use std::sync::Arc;

// ── 扩展点 trait ──

/// 自定义笔尖插件。
pub trait TipPlugin: Send + Sync {
    /// 插件名（注册键）。
    fn name(&self) -> &str;

    /// 笔尖采样：归一化坐标 (x,y) ∈ [-1,1]² → 覆盖率 0..1。
    /// 引擎侧会在各向异性（tilt 椭圆）换算后调用。
    fn coverage(&self, x: f32, y: f32, pressure: f32) -> f32;

    /// 可选：笔尖参数面板定义（宿主 UI 渲染用）。
    fn parameters(&self) -> Vec<PluginParam> {
        Vec::new()
    }
}

/// 自定义滤镜插件。
pub trait FilterPlugin: Send + Sync {
    fn name(&self) -> &str;

    /// 逐像素变换（直行 RGBA）。w/h 供邻域参考（本期仅逐像素）。
    fn apply(&self, pixels: &mut [u8], w: u32, h: u32, params: &PluginParams);

    fn parameters(&self) -> Vec<PluginParam> {
        Vec::new()
    }
}

/// 自定义工具插件（指针序列 → 引擎动作）。
pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &str;

    /// 指针事件（画布坐标）。返回动作列表由引擎执行
    /// （安全：插件不能直接改图层）。
    fn on_pointer(
        &self,
        phase: PointerPhase,
        sample: &PointerSample,
        ctx: &ToolContext,
    ) -> Vec<PluginAction>;

    fn parameters(&self) -> Vec<PluginParam> {
        Vec::new()
    }
}

// ── 参数与上下文 ──

/// 参数定义（宿主 UI 自动渲染控件）。
#[derive(Debug, Clone)]
pub struct PluginParam {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
}

#[derive(Debug, Clone)]
/// 参数 UI 类型（宿主自动渲染控件）。
pub enum ParamKind {
    /// 滑杆 min..max。
    Range { min: f32, max: f32, default: f32 },
    /// 开关。
    Toggle { default: bool },
    /// 选项。
    Choice {
        options: Vec<String>,
        default: usize,
    },
}

/// 插件参数运行时值。
#[derive(Debug, Clone, Default)]
pub struct PluginParams {
    values: HashMap<String, ParamValue>,
}

impl PluginParams {
    /// 设置参数值。
    pub fn set(&mut self, key: &str, v: ParamValue) {
        self.values.insert(key.to_string(), v);
    }

    /// 读数值（类型不匹配回退默认）。
    pub fn get_f32(&self, key: &str, default: f32) -> f32 {
        match self.values.get(key) {
            Some(ParamValue::Number(v)) => *v,
            _ => default,
        }
    }

    /// 读开关。
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.values.get(key) {
            Some(ParamValue::Bool(v)) => *v,
            _ => default,
        }
    }

    /// 读选项索引。
    pub fn get_choice(&self, key: &str, default: usize) -> usize {
        match self.values.get(key) {
            Some(ParamValue::Choice(v)) => *v,
            _ => default,
        }
    }
}

#[derive(Debug, Clone)]
/// 参数运行时值。
pub enum ParamValue {
    Number(f32),
    Bool(bool),
    Choice(usize),
}

/// 工具上下文（插件只读）。
pub struct ToolContext {
    pub active_layer: u64,
    pub brush_size: f32,
    pub brush_color: (u8, u8, u8),
    pub zoom: f64,
}

/// 插件请求引擎执行的动作（引擎侧安全执行）。
#[derive(Debug, Clone)]
pub enum PluginAction {
    /// 在图层 (x,y) 盖一个插件自定义笔尖的 dab。
    StampDab {
        x: f64,
        y: f64,
        radius: f32,
        alpha: f32,
        /// 使用哪个已注册笔尖插件。
        tip: String,
    },
    /// 请求撤销。
    Undo,
    /// 撤销组边界（自动：Down→Up 之间引擎自动成组，无需插件显式）。
    Noop,
}

// ── 注册表 ──

/// 插件注册表（Arc 共享，宿主持有）。
#[derive(Default, Clone)]
pub struct PluginRegistry {
    tips: HashMap<String, Arc<dyn TipPlugin>>,
    filters: HashMap<String, Arc<dyn FilterPlugin>>,
    tools: HashMap<String, Arc<dyn ToolPlugin>>,
}

impl PluginRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    // 注册
    /// 注册笔尖插件。
    pub fn register_tip(&mut self, p: Arc<dyn TipPlugin>) {
        self.tips.insert(p.name().to_string(), p);
    }

    /// 注册滤镜插件。
    pub fn register_filter(&mut self, p: Arc<dyn FilterPlugin>) {
        self.filters.insert(p.name().to_string(), p);
    }

    /// 注册工具插件。
    pub fn register_tool(&mut self, p: Arc<dyn ToolPlugin>) {
        self.tools.insert(p.name().to_string(), p);
    }

    // 查询
    /// 按名取笔尖。
    pub fn tip(&self, name: &str) -> Option<Arc<dyn TipPlugin>> {
        self.tips.get(name).cloned()
    }

    /// 按名取滤镜。
    pub fn filter(&self, name: &str) -> Option<Arc<dyn FilterPlugin>> {
        self.filters.get(name).cloned()
    }

    /// 按名取工具。
    pub fn tool(&self, name: &str) -> Option<Arc<dyn ToolPlugin>> {
        self.tools.get(name).cloned()
    }

    /// 笔尖名列表。
    pub fn tip_names(&self) -> Vec<String> {
        self.tips.keys().cloned().collect()
    }

    /// 滤镜名列表。
    pub fn filter_names(&self) -> Vec<String> {
        self.filters.keys().cloned().collect()
    }

    /// 工具名列表。
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 用注册的滤镜插件处理像素（Engine::apply_plugin_filter 的核心路径）。
    pub fn run_filter(
        &self,
        name: &str,
        pixels: &mut [u8],
        w: u32,
        h: u32,
        params: &PluginParams,
    ) -> bool {
        if let Some(f) = self.filters.get(name) {
            f.apply(pixels, w, h, params);
            true
        } else {
            false
        }
    }
}

// ── 内置示例插件（同时是 trait 用法的参考实现）──

/// 星形笔尖：N 角星，pressure 控制锐度。
pub struct StarTip {
    pub points: u32,
}

impl TipPlugin for StarTip {
    fn name(&self) -> &str {
        "star"
    }

    fn coverage(&self, x: f32, y: f32, pressure: f32) -> f32 {
        let n = self.points.max(3) as f32;
        let d = (x * x + y * y).sqrt();
        if d >= 1.0 {
            return 0.0;
        }
        // 星形半径调制：r(θ) = 0.5 + 0.5·|cos(nθ/2)|^sharpness
        let theta = y.atan2(x);
        let sharp = 1.0 + pressure * 3.0; // 压感越重越尖
        let r = 0.4 + 0.6 * (n * 0.5 * theta).cos().abs().powf(sharp);
        if d <= r {
            1.0
        } else {
            0.0
        }
    }

    fn parameters(&self) -> Vec<PluginParam> {
        vec![PluginParam {
            key: "points".into(),
            label: "角数".into(),
            kind: ParamKind::Range {
                min: 3.0,
                max: 12.0,
                default: self.points as f32,
            },
        }]
    }
}

/// 菱形笔尖。
pub struct DiamondTip;

impl TipPlugin for DiamondTip {
    fn name(&self) -> &str {
        "diamond"
    }

    fn coverage(&self, x: f32, y: f32, _pressure: f32) -> f32 {
        let d = x.abs() + y.abs(); // 菱形 L1 距离
        if d >= 1.0 {
            0.0
        } else {
            1.0 - d
        }
    }
}

/// 色彩平衡滤镜（R/G/B 通道偏移）。
pub struct ChannelShiftFilter;

impl FilterPlugin for ChannelShiftFilter {
    fn name(&self) -> &str {
        "channel_shift"
    }

    fn apply(&self, pixels: &mut [u8], _w: u32, _h: u32, params: &PluginParams) {
        let r_shift = params.get_f32("r", 0.0);
        let g_shift = params.get_f32("g", 0.0);
        let b_shift = params.get_f32("b", 0.0);
        for px in pixels.as_chunks_mut::<4>().0 {
            px[0] = ((px[0] as f32 + r_shift) as u16).min(255) as u8;
            px[1] = ((px[1] as f32 + g_shift) as u16).min(255) as u8;
            px[2] = ((px[2] as f32 + b_shift) as u16).min(255) as u8;
        }
    }

    fn parameters(&self) -> Vec<PluginParam> {
        vec![
            PluginParam {
                key: "r".into(),
                label: "R 偏移".into(),
                kind: ParamKind::Range {
                    min: -100.0,
                    max: 100.0,
                    default: 0.0,
                },
            },
            PluginParam {
                key: "g".into(),
                label: "G 偏移".into(),
                kind: ParamKind::Range {
                    min: -100.0,
                    max: 100.0,
                    default: 0.0,
                },
            },
            PluginParam {
                key: "b".into(),
                label: "B 偏移".into(),
                kind: ParamKind::Range {
                    min: -100.0,
                    max: 100.0,
                    default: 0.0,
                },
            },
        ]
    }
}

impl PluginRegistry {
    /// 内置示例插件集。
    pub fn with_builtin_examples() -> Self {
        let mut r = Self::new();
        r.register_tip(Arc::new(StarTip { points: 5 }));
        r.register_tip(Arc::new(DiamondTip));
        r.register_filter(Arc::new(ChannelShiftFilter));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_tip_coverage_shape() {
        let tip = StarTip { points: 5 };
        // 中心满
        assert_eq!(tip.coverage(0.0, 0.0, 1.0), 1.0);
        // 星角方向（θ=0 处 cos(0)=1 → r=1.0）：边界点覆盖
        assert_eq!(tip.coverage(0.95, 0.0, 1.0), 1.0, "星角方向覆盖");
        // 凹谷方向（θ=π/5，cos(nθ/2)=cos(π/2)=0 → r=0.4）：0.5 处已出
        // θ=π/n 是两角之间的谷
        let theta = std::f32::consts::PI / 5.0;
        let gx = theta.cos() * 0.5;
        let gy = theta.sin() * 0.5;
        assert_eq!(tip.coverage(gx, gy, 1.0), 0.0, "星谷方向 0.5 处不覆盖");
    }

    #[test]
    fn diamond_tip_coverage() {
        let tip = DiamondTip;
        assert_eq!(tip.coverage(0.0, 0.0, 1.0), 1.0);
        assert_eq!(tip.coverage(0.5, 0.5, 1.0), 0.0, "L1=1 边界");
        assert!(tip.coverage(0.3, 0.3, 1.0) > 0.0, "内部渐变");
    }

    #[test]
    fn registry_register_and_run() {
        let mut reg = PluginRegistry::new();
        reg.register_filter(Arc::new(ChannelShiftFilter));
        assert_eq!(reg.filter_names(), vec!["channel_shift"]);

        let mut pixels = vec![100u8, 100, 100, 255];
        let mut params = PluginParams::default();
        params.set("r", ParamValue::Number(50.0));
        assert!(reg.run_filter("channel_shift", &mut pixels, 1, 1, &params));
        assert_eq!(pixels[0], 150, "R 通道 +50");
        assert_eq!(pixels[1], 100, "G 不变");
        assert!(!reg.run_filter("不存在", &mut pixels, 1, 1, &params));
    }

    #[test]
    fn builtin_examples_available() {
        let reg = PluginRegistry::with_builtin_examples();
        assert!(reg.tip("star").is_some());
        assert!(reg.tip("diamond").is_some());
        assert!(reg.filter("channel_shift").is_some());
        assert!(reg.tip_names().len() >= 2);
    }

    #[test]
    fn params_default_fallback() {
        let p = PluginParams::default();
        assert_eq!(p.get_f32("x", 0.5), 0.5, "缺省回退");
        let mut p2 = PluginParams::default();
        p2.set("x", ParamValue::Number(1.5));
        assert_eq!(p2.get_f32("x", 0.5), 1.5);
        assert!(p2.get_bool("x", true), "类型不匹配回退默认");
    }
}
