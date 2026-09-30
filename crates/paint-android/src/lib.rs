//! paint-android：Android 壳（cdylib + JNI）。
//!
//! 职责与 paint-wasm 对称：
//! - 呈现：`AndroidBitmap_lockPixels` 直接把引擎帧写进 Kotlin 侧
//!   传入的 ARGB_8888 Bitmap（预乘一致，零额外拷贝）
//! - 输入：Kotlin `PaintEngineView` 把 MotionEvent（含历史点展开、
//!   压感、笔悬停）翻译成 PlatformEvent 转发进来
//!
//! ## 方法绑定：`JNI_OnLoad` + `RegisterNatives`
//!
//! 本 crate **不导出任何 `Java_<类全名>_<方法>` 名字混淆符号**。全部
//! native 方法在库加载时（`JNI_OnLoad`）经 `RegisterNatives` 一次性
//! 注册到 `JNI_CLASS` 常量指定的类上：
//!
//! - 类名只出现在 `JNI_CLASS` 一个常量里——fork 改包名/改类名时改
//!   这一处并重编即可，无需再同步 46 个符号名
//! - 方法描述符集中列在 `native_methods()` 的注册表中，与 Kotlin 侧
//!   `external fun` 声明逐字对应（签名清单见 `ANDROID_API.md`）
//!
//! 引擎句柄以 jlong（裸指针）往返，单 UI 线程使用。

use jni::objects::{JByteArray, JObject};
use jni::sys::{jboolean, jbyte, jbyteArray, jdouble, jfloat, jint, jlong};
use jni::{JNIEnv, JavaVM, NativeMethod};
use paint_core::input::{PointerKind, PointerPhase, PointerSample};
use paint_core::render::{EngineConfig, Surface};
use paint_core::{Engine, PlatformEvent, Rect, Tool};
use paint_render::SoftwareRenderer;

/// native 方法注册的目标类（Kotlin 侧 `PaintEngineView` 的全名，
/// `/` 分隔）。改名只需改这一处。
const JNI_CLASS: &str = "com/paintengine/android/PaintEngineView";

// 与 Kotlin 侧约定的常量（见 PaintEngineView.kt）
const PHASE_DOWN: jint = 0;
const PHASE_MOVE: jint = 1;
const PHASE_UP: jint = 2;
#[allow(dead_code)]
const PHASE_CANCEL: jint = 3;

const KIND_PEN: jint = 0;
const KIND_ERASER: jint = 1;
const KIND_TOUCH: jint = 2;
#[allow(dead_code)]
const KIND_MOUSE: jint = 3;

// ── logcat 日志后端（零依赖：直连 liblog）──

#[cfg(target_os = "android")]
mod logcat {
    use std::sync::atomic::{AtomicU8, Ordering};

    // android/log.h 优先级
    const VERBOSE: i32 = 2;
    const INFO: i32 = 4;
    const WARN: i32 = 5;
    const ERROR: i32 = 6;

    #[link(name = "log")]
    extern "C" {
        fn __android_log_print(prio: i32, tag: *const u8, fmt: *const u8, ...) -> i32;
    }

    /// 当前级别（log::Level 的 u8 表示；Off=0..Error=1..Trace=5 → 映射为阈值）。
    static LEVEL: AtomicU8 = AtomicU8::new(3); // 默认 Info（log crate: Info=3）

    pub fn set_level(level: u8) {
        LEVEL.store(level, Ordering::Relaxed);
    }

    pub fn enabled(level: u8) -> bool {
        level <= LEVEL.load(Ordering::Relaxed)
    }

    pub fn write(level: u8, target: &str, msg: &str) {
        let prio = match level {
            1 => ERROR,
            2 => WARN,
            3 => INFO,
            _ => VERBOSE, // 4=Debug 5=Trace
        };
        let mut line = format!("[{target}] {msg}");
        line.push('\0'); // __android_log_print 的 %s 需要 C 字符串
        unsafe {
            __android_log_print(
                prio,
                b"paintEngine\0".as_ptr(),
                b"%s\0".as_ptr(),
                line.as_ptr(),
            );
        }
    }
}

/// logcat 日志器（JNI_OnLoad 时安装一次）。
struct LogcatLogger;

impl log::Log for LogcatLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        #[cfg(target_os = "android")]
        {
            logcat::enabled(m.level() as u8)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = m;
            false
        }
    }
    fn log(&self, r: &log::Record) {
        #[cfg(target_os = "android")]
        {
            if self.enabled(r.metadata()) {
                logcat::write(r.level() as u8, r.target(), &r.args().to_string());
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = r;
        }
    }
    fn flush(&self) {}
}

static LOGCAT: LogcatLogger = LogcatLogger;
static LOGGER_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn init_logcat_logger() {
    if !LOGGER_INSTALLED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let _ = log::set_logger(&LOGCAT);
        log::set_max_level(log::LevelFilter::Info);
        log::info!("[app] Android 壳日志就绪（logcat 过滤 tag: paintEngine）");
    }
}

fn engine(handle: jlong) -> &'static mut Engine {
    assert!(handle != 0, "无效引擎句柄");
    // SAFETY: 句柄来自 Box::into_raw，UI 单线程使用，生命周期与 View 一致
    unsafe { &mut *(handle as *mut Engine) }
}

// ── native 实现（经 RegisterNatives 绑定，不导出符号）──
// 约定：函数名 = Kotlin external fun 的 snake_case；
// env 之后第一个参数是实例方法的 this。除注明外全部单 UI 线程调用。

/// 创建引擎实例，返回 jlong 句柄（`Box::into_raw`）。
extern "system" fn native_create(_env: JNIEnv, _this: JObject) -> jlong {
    init_logcat_logger();
    let e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    Box::into_raw(Box::new(e)) as jlong
}

/// 销毁引擎（View 脱离窗口时）。
extern "system" fn native_destroy(_env: JNIEnv, _this: JObject, handle: jlong) {
    if handle != 0 {
        drop(unsafe { Box::from_raw(handle as *mut Engine) });
    }
}

/// 视口尺寸变更（物理像素 + density）。
extern "system" fn native_resize(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    w: jint,
    h: jint,
    scale: jfloat,
) {
    if w > 0 && h > 0 {
        engine(handle).handle_event(PlatformEvent::Resize {
            w: w as u32,
            h: h as u32,
            scale,
        });
    }
}

/// `pressure < 0` 表示无压感（手指/鼠标按满压处理）；
/// `tilt_x/tilt_y = NaN` 表示无倾斜数据。
#[allow(clippy::too_many_arguments)]
extern "system" fn native_pointer(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    phase: jint,
    id: jint,
    x: jdouble,
    y: jdouble,
    pressure: jdouble,
    tilt_x: jdouble,
    tilt_y: jdouble,
    kind: jint,
    t_us: jlong,
) {
    let kind = match kind {
        KIND_PEN => PointerKind::Pen,
        KIND_ERASER => PointerKind::Eraser,
        KIND_TOUCH => PointerKind::Touch,
        _ => PointerKind::Mouse,
    };
    let phase = match phase {
        PHASE_DOWN => PointerPhase::Down,
        PHASE_MOVE => PointerPhase::Move,
        PHASE_UP => PointerPhase::Up,
        _ => PointerPhase::Cancel,
    };
    let sample = PointerSample {
        x,
        y,
        pressure: if pressure < 0.0 {
            None
        } else {
            Some(ppressure(pressure))
        },
        tilt: if tilt_x.is_finite() && tilt_y.is_finite() {
            Some((tilt_x as f32, tilt_y as f32))
        } else {
            None
        },
        kind,
        id: id as u64,
        t_us: t_us.max(0) as u64,
    };
    engine(handle).handle_event(PlatformEvent::Pointer { phase, sample });
}

fn ppressure(v: jdouble) -> f32 {
    (v as f32).clamp(0.0, 1.0)
}

/// 数位笔悬停状态（引擎据此做手掌拒绝）。
extern "system" fn native_pen_in_range(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    in_range: jboolean,
) {
    engine(handle).handle_event(PlatformEvent::PenInRange(in_range != 0));
}

/// 窗口焦点变更。
extern "system" fn native_focus(_env: JNIEnv, _this: JObject, handle: jlong, focused: jboolean) {
    engine(handle).handle_event(PlatformEvent::Focus(focused != 0));
}

/// 合成到 `bitmap`（ARGB_8888，与引擎预乘 RGBA 布局一致）。
/// 返回 false 表示位图格式不受支持。
extern "system" fn native_render(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    bitmap: JObject,
) -> jboolean {
    let Some(mut locked) = lock_bitmap(&mut env, &bitmap) else {
        return 0;
    };
    engine(handle).render(&mut locked);
    locked.unlock(&mut env, &bitmap);
    1
}

// 与 Kotlin 侧约定的工具码（见 PaintEngineView.kt TOOL_* 常量）
#[allow(dead_code)]
const TOOL_BRUSH: jint = 0;
const TOOL_ERASER: jint = 1;
const TOOL_MASK: jint = 2;
const TOOL_LINE: jint = 3;
const TOOL_RECT: jint = 4;
const TOOL_ELLIPSE: jint = 5;
const TOOL_LINE_FILL: jint = 6;
const TOOL_RECT_FILL: jint = 7;
const TOOL_ELLIPSE_FILL: jint = 8;
const TOOL_TEXT: jint = 9;
const TOOL_FILL: jint = 10;

/// 工具码 → 引擎 Tool。
fn tool_from_code(code: jint) -> Tool {
    use paint_core::ShapeKind;
    match code {
        TOOL_ERASER => Tool::Eraser,
        TOOL_MASK => Tool::Mask,
        TOOL_LINE | TOOL_LINE_FILL => Tool::Shape {
            kind: ShapeKind::Line,
            fill: code == TOOL_LINE_FILL,
        },
        TOOL_RECT | TOOL_RECT_FILL => Tool::Shape {
            kind: ShapeKind::Rect,
            fill: code == TOOL_RECT_FILL,
        },
        TOOL_ELLIPSE | TOOL_ELLIPSE_FILL => Tool::Shape {
            kind: ShapeKind::Ellipse,
            fill: code == TOOL_ELLIPSE_FILL,
        },
        TOOL_TEXT => Tool::Text,
        TOOL_FILL => Tool::Fill { tolerance: 32 },
        _ => Tool::Brush,
    }
}

extern "system" fn native_set_tool(_env: JNIEnv, _this: JObject, handle: jlong, code: jint) {
    engine(handle).set_tool(tool_from_code(code));
}

/// 文字工具锚点（画布坐标）。无锚点返回 null；一次性取走。
extern "system" fn native_take_text_anchor(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jni::sys::jfloatArray {
    let Some((x, y)) = engine(handle).take_text_anchor() else {
        return std::ptr::null_mut();
    };
    let arr = [x as f32, y as f32];
    match env.new_float_array(2) {
        Ok(mut a) => {
            if env.set_float_array_region(&mut a, 0, &arr).is_ok() {
                a.as_raw()
            } else {
                std::ptr::null_mut()
            }
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// 文字落墨（字体字节由 Kotlin 侧从 /system/fonts 加载）。
extern "system" fn native_draw_text(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    font: JByteArray,
    text: jni::objects::JString,
    x: jdouble,
    y: jdouble,
    size: jdouble,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(&font) else {
        return 0;
    };
    let Ok(text) = env.get_string(&text) else {
        return 0;
    };
    let text: String = text.into();
    engine(handle)
        .draw_text(&bytes, &text, x as i64, y as i64, size as f32)
        .is_some() as jboolean
}

/// 设置文字字体（对象光栅化用；启动时加载系统字体一次）。
extern "system" fn native_set_text_font(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    font: JByteArray,
) {
    if let Ok(bytes) = env.convert_byte_array(&font) {
        engine(handle).set_text_font(bytes);
    }
}

/// 新增文字对象（非破坏，可重编辑）。`raster = null` → 引擎 swash 渲染。
extern "system" fn native_add_text_object(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    text: jni::objects::JString,
    x: jdouble,
    y: jdouble,
    size: jdouble,
) -> jboolean {
    let Ok(text) = env.get_string(&text) else {
        return 0;
    };
    let text: String = text.into();
    engine(handle).add_text_object((x, y), &text, size as f32, None) as jboolean
}

/// 命中的文字对象信息（预填编辑框）：[text, size, rrggbb]；无则 null。
extern "system" fn native_take_text_edit(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jni::sys::jobjectArray {
    let Some((text, size, color)) = engine(handle).take_text_edit() else {
        return std::ptr::null_mut();
    };
    let arr = match env.new_object_array(3, "java/lang/String", JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };
    let fields = [
        text,
        format!("{}", size as i32),
        format!("{:02X}{:02X}{:02X}", color.r, color.g, color.b),
    ];
    for (i, f) in fields.iter().enumerate() {
        if let Ok(s) = env.new_string(f) {
            let _ = env.set_object_array_element(&arr, i as i32, s);
        }
    }
    arr.as_raw()
}

/// 更新命中的文字对象（内容/字号）。
extern "system" fn native_update_text_object(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    text: jni::objects::JString,
    size: jdouble,
) -> jboolean {
    let Ok(text) = env.get_string(&text) else {
        return 0;
    };
    let text: String = text.into();
    engine(handle).update_text_object(&text, size as f32, None) as jboolean
}

/// 删除命中的文字对象。
extern "system" fn native_delete_text_object(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    engine(handle).delete_text_object() as jboolean
}

/// 呈现帧计数（单调；监控关闭时冻结）。壳层采样差值算帧率。
extern "system" fn native_render_count(_env: JNIEnv, _this: JObject, handle: jlong) -> jlong {
    engine(handle).render_count() as jlong
}

/// 帧率监控开关。
extern "system" fn native_set_fps_monitor(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    on: jboolean,
) {
    engine(handle).set_fps_monitor(on != 0);
}

/// 日志级别设置（0=Off 1=Error 2=Warn 3=Info 4=Debug 5=Trace；adb 调试用）。
/// 未在 Kotlin 侧声明 external fun——预留接口。
#[allow(dead_code)]
extern "system" fn native_set_log_level(_env: JNIEnv, _this: JObject, level: jint) {
    let lv = match level {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Warn,
        3 => log::LevelFilter::Info,
        4 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };
    log::set_max_level(lv);
    #[cfg(target_os = "android")]
    logcat::set_level(lv as u8);
    log::info!("[app] 日志级别切换为 {lv:?}");
}

/// 编辑计数（自动保存脏检查：与上次保存时不同即有未保存修改）。
extern "system" fn native_edit_count(_env: JNIEnv, _this: JObject, handle: jlong) -> jlong {
    engine(handle).edit_count() as jlong
}

/// 存 .ora 工程字节（自动保存用）。
extern "system" fn native_save_ora(env: JNIEnv, _this: JObject, handle: jlong) -> jbyteArray {
    let Some(bytes) = engine(handle).save_ora() else {
        return std::ptr::null_mut();
    };
    match env.byte_array_from_slice(&bytes) {
        Ok(arr) => arr.as_raw().cast(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 载入 .ora 替换当前文档。
extern "system" fn native_load_ora(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    data: JByteArray,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(&data) else {
        return 0;
    };
    engine(handle).load_ora(&bytes) as jboolean
}

/// 预设名列表（内置 6 支 + 用户自定义）。
extern "system" fn native_preset_names(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jni::sys::jobjectArray {
    let names = engine(handle).preset_names();
    let arr = match env.new_object_array(names.len() as i32, "java/lang/String", JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };
    for (i, name) in names.iter().enumerate() {
        if let Ok(s) = env.new_string(name) {
            let _ = env.set_object_array_element(&arr, i as i32, s);
        }
    }
    arr.as_raw()
}

/// 应用笔刷预设（按名）。
extern "system" fn native_apply_preset(
    mut env: JNIEnv,
    _this: JObject,
    handle: jlong,
    name: jni::objects::JString,
) -> jboolean {
    let Ok(name) = env.get_string(&name) else {
        return 0;
    };
    let name: String = name.into();
    engine(handle).apply_preset(&name) as jboolean
}

/// 附件导入：PNG/JPEG/WebP/SVG 自动识别 → 新图层（视野中心）。
extern "system" fn native_import_image(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    data: JByteArray,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(&data) else {
        return 0;
    };
    engine(handle).import_image(&bytes).is_some() as jboolean
}

/// 直行 RGBA → 浮动层（PDF 等壳层渲染的位图走此通道，可拖拽放置）。
extern "system" fn native_paste_rgba(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    rgba: JByteArray,
    w: jint,
    h: jint,
) -> jboolean {
    if w <= 0 || h <= 0 {
        return 0;
    }
    let Ok(bytes) = env.convert_byte_array(&rgba) else {
        return 0;
    };
    let need = w as usize * h as usize * 4;
    if bytes.len() < need {
        return 0;
    }
    engine(handle).paste_rgba_float(&bytes, w as u32, h as u32) as jboolean
}

// ── 浮动内容变换（附件放置交互）──

extern "system" fn native_transforming(_env: JNIEnv, _this: JObject, handle: jlong) -> jboolean {
    engine(handle).transforming() as jboolean
}

/// 屏幕位移 → 画布位移（经视口逆变换，旋转/缩放下方向正确）。
extern "system" fn native_transform_translate_screen(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    dx: jdouble,
    dy: jdouble,
) {
    let e = engine(handle);
    let vp = e.document().viewport().clone();
    let (cx0, cy0) = vp.screen_to_canvas(0.0, 0.0);
    let (cx1, cy1) = vp.screen_to_canvas(dx, dy);
    e.transform_translate(cx1 - cx0, cy1 - cy0);
}

extern "system" fn native_transform_rotate(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    delta_deg: jdouble,
) {
    engine(handle).transform_rotate(delta_deg.to_radians());
}

extern "system" fn native_transform_scale(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    factor: jdouble,
) {
    engine(handle).transform_scale(factor);
}

extern "system" fn native_commit_transform(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    engine(handle).commit_transform() as jboolean
}

extern "system" fn native_cancel_transform(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
) -> jboolean {
    engine(handle).cancel_transform() as jboolean
}

extern "system" fn native_set_brush_size(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    size: jdouble,
) {
    engine(handle).brush_mut().size = (size as f32).clamp(1.0, 512.0);
}

extern "system" fn native_set_brush_color(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    r: jint,
    g: jint,
    b: jint,
) {
    engine(handle).brush_mut().color = paint_core::Color {
        r: r.clamp(0, 255) as u8,
        g: g.clamp(0, 255) as u8,
        b: b.clamp(0, 255) as u8,
    };
}

extern "system" fn native_undo(_env: JNIEnv, _this: JObject, handle: jlong) -> jboolean {
    engine(handle).undo() as jboolean
}

extern "system" fn native_redo(_env: JNIEnv, _this: JObject, handle: jlong) -> jboolean {
    engine(handle).redo() as jboolean
}

extern "system" fn native_add_layer(_env: JNIEnv, _this: JObject, handle: jlong) -> jboolean {
    engine(handle).add_layer().is_some() as jboolean
}

extern "system" fn native_merge_down(_env: JNIEnv, _this: JObject, handle: jlong) -> jboolean {
    engine(handle).merge_down() as jboolean
}

extern "system" fn native_flatten(_env: JNIEnv, _this: JObject, handle: jlong) -> jboolean {
    engine(handle).flatten() as jboolean
}

extern "system" fn native_layer_count(_env: JNIEnv, _this: JObject, handle: jlong) -> jint {
    engine(handle).layer_count() as jint
}

extern "system" fn native_active_layer_index(_env: JNIEnv, _this: JObject, handle: jlong) -> jint {
    engine(handle)
        .active_layer_index()
        .map_or(-1, |i| i as jint)
}

extern "system" fn native_select_layer_index(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    index: jint,
) -> jboolean {
    if index < 0 {
        return 0;
    }
    engine(handle).select_layer_index(index as usize) as jboolean
}

extern "system" fn native_layer_name_at(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    index: jint,
) -> jni::sys::jstring {
    if index < 0 {
        return std::ptr::null_mut();
    }
    match engine(handle).layer_name(index as usize) {
        Some(name) => match env.new_string(name) {
            Ok(s) => s.as_raw(),
            Err(_) => std::ptr::null_mut(),
        },
        None => std::ptr::null_mut(),
    }
}

extern "system" fn native_fit_to_content(_env: JNIEnv, _this: JObject, handle: jlong) {
    engine(handle).fit_to_content(48.0);
}

extern "system" fn native_zoom_100(_env: JNIEnv, _this: JObject, handle: jlong) {
    engine(handle).zoom_100();
}

extern "system" fn native_set_show_grid(
    _env: JNIEnv,
    _this: JObject,
    handle: jlong,
    show: jboolean,
) {
    engine(handle).set_show_grid(show != 0);
}

/// 导出 PNG（可见内容包围盒，透明背景）。
extern "system" fn native_export_png(env: JNIEnv, _this: JObject, handle: jlong) -> jbyteArray {
    let Some(png) = engine(handle).export_png(None, 1.0, true) else {
        return std::ptr::null_mut();
    };
    match env.byte_array_from_slice(&png) {
        Ok(arr) => arr.as_raw().cast(),
        Err(_) => std::ptr::null_mut(),
    }
}

extern "system" fn native_import_png(
    env: JNIEnv,
    _this: JObject,
    handle: jlong,
    data: JByteArray,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(&data) else {
        return 0;
    };
    engine(handle).import_png(&bytes).is_some() as jboolean
}

#[allow(dead_code)]
fn unused(_: jbyte) {}

// ── 方法注册表与库加载入口 ──

/// 构建注册表：名字 →（JNI 描述符, 实现指针）。
///
/// 名字/描述符必须与 `PaintEngineView.kt` 的 `external fun` 声明逐字
/// 一致；[self::tests] 做结构完整性检查（重复名/空指针/描述符形状）。
fn native_methods() -> Vec<NativeMethod> {
    use std::ffi::c_void;
    fn entry(name: &str, sig: &str, f: *mut c_void) -> NativeMethod {
        NativeMethod {
            name: name.into(),
            sig: sig.into(),
            fn_ptr: f,
        }
    }
    vec![
        entry("nativeCreate", "()J", native_create as *mut c_void),
        entry("nativeDestroy", "(J)V", native_destroy as *mut c_void),
        entry("nativeResize", "(JIIF)V", native_resize as *mut c_void),
        entry(
            "nativePointer",
            "(JIIDDDDDIJ)V",
            native_pointer as *mut c_void,
        ),
        entry(
            "nativePenInRange",
            "(JZ)V",
            native_pen_in_range as *mut c_void,
        ),
        entry("nativeFocus", "(JZ)V", native_focus as *mut c_void),
        entry(
            "nativeRender",
            "(JLandroid/graphics/Bitmap;)Z",
            native_render as *mut c_void,
        ),
        entry("nativeSetTool", "(JI)V", native_set_tool as *mut c_void),
        entry(
            "nativeTakeTextAnchor",
            "(J)[F",
            native_take_text_anchor as *mut c_void,
        ),
        entry(
            "nativeDrawText",
            "(J[BLjava/lang/String;DDD)Z",
            native_draw_text as *mut c_void,
        ),
        entry(
            "nativeSetTextFont",
            "(J[B)V",
            native_set_text_font as *mut c_void,
        ),
        entry(
            "nativeAddTextObject",
            "(JLjava/lang/String;DDD)Z",
            native_add_text_object as *mut c_void,
        ),
        entry(
            "nativeTakeTextEdit",
            "(J)[Ljava/lang/String;",
            native_take_text_edit as *mut c_void,
        ),
        entry(
            "nativeUpdateTextObject",
            "(JLjava/lang/String;D)Z",
            native_update_text_object as *mut c_void,
        ),
        entry(
            "nativeDeleteTextObject",
            "(J)Z",
            native_delete_text_object as *mut c_void,
        ),
        entry("nativeEditCount", "(J)J", native_edit_count as *mut c_void),
        entry(
            "nativeRenderCount",
            "(J)J",
            native_render_count as *mut c_void,
        ),
        entry(
            "nativeSetFpsMonitor",
            "(JZ)V",
            native_set_fps_monitor as *mut c_void,
        ),
        entry("nativeSaveOra", "(J)[B", native_save_ora as *mut c_void),
        entry("nativeLoadOra", "(J[B)Z", native_load_ora as *mut c_void),
        entry(
            "nativeSetBrushSize",
            "(JD)V",
            native_set_brush_size as *mut c_void,
        ),
        entry(
            "nativeSetBrushColor",
            "(JIII)V",
            native_set_brush_color as *mut c_void,
        ),
        entry(
            "nativePresetNames",
            "(J)[Ljava/lang/String;",
            native_preset_names as *mut c_void,
        ),
        entry(
            "nativeApplyPreset",
            "(JLjava/lang/String;)Z",
            native_apply_preset as *mut c_void,
        ),
        entry("nativeUndo", "(J)Z", native_undo as *mut c_void),
        entry("nativeRedo", "(J)Z", native_redo as *mut c_void),
        entry("nativeAddLayer", "(J)Z", native_add_layer as *mut c_void),
        entry("nativeMergeDown", "(J)Z", native_merge_down as *mut c_void),
        entry("nativeFlatten", "(J)Z", native_flatten as *mut c_void),
        entry(
            "nativeLayerCount",
            "(J)I",
            native_layer_count as *mut c_void,
        ),
        entry(
            "nativeActiveLayerIndex",
            "(J)I",
            native_active_layer_index as *mut c_void,
        ),
        entry(
            "nativeSelectLayerIndex",
            "(JI)Z",
            native_select_layer_index as *mut c_void,
        ),
        entry(
            "nativeLayerNameAt",
            "(JI)Ljava/lang/String;",
            native_layer_name_at as *mut c_void,
        ),
        entry(
            "nativeFitToContent",
            "(J)V",
            native_fit_to_content as *mut c_void,
        ),
        entry("nativeZoom100", "(J)V", native_zoom_100 as *mut c_void),
        entry(
            "nativeSetShowGrid",
            "(JZ)V",
            native_set_show_grid as *mut c_void,
        ),
        entry("nativeExportPng", "(J)[B", native_export_png as *mut c_void),
        entry(
            "nativeImportPng",
            "(J[B)Z",
            native_import_png as *mut c_void,
        ),
        entry(
            "nativeImportImage",
            "(J[B)Z",
            native_import_image as *mut c_void,
        ),
        entry(
            "nativePasteRgba",
            "(J[BII)Z",
            native_paste_rgba as *mut c_void,
        ),
        entry(
            "nativeTransforming",
            "(J)Z",
            native_transforming as *mut c_void,
        ),
        entry(
            "nativeTransformTranslateScreen",
            "(JDD)V",
            native_transform_translate_screen as *mut c_void,
        ),
        entry(
            "nativeTransformRotate",
            "(JD)V",
            native_transform_rotate as *mut c_void,
        ),
        entry(
            "nativeTransformScale",
            "(JD)V",
            native_transform_scale as *mut c_void,
        ),
        entry(
            "nativeCommitTransform",
            "(J)Z",
            native_commit_transform as *mut c_void,
        ),
        entry(
            "nativeCancelTransform",
            "(J)Z",
            native_cancel_transform as *mut c_void,
        ),
    ]
}

/// 库加载入口（`System.loadLibrary` 触发）：注册全部 native 方法。
///
/// 返回 `JNI_VERSION_1_6` 表示就绪；注册失败返回 `JNI_ERR`，宿主将
/// 收到 `UnsatisfiedLinkError`（类名/签名与 Kotlin 声明不匹配时）。
#[no_mangle]
pub extern "system" fn JNI_OnLoad(vm: JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    init_logcat_logger();
    let Ok(mut env) = vm.get_env() else {
        return jni::sys::JNI_ERR;
    };
    match env.register_native_methods(JNI_CLASS, &native_methods()) {
        Ok(()) => {
            log::info!("[app] RegisterNatives 完成：{} 个方法 → {JNI_CLASS}", 46);
            jni::sys::JNI_VERSION_1_6
        }
        Err(e) => {
            log::error!("[app] RegisterNatives 失败（类名或签名不匹配？）：{e}");
            // 打印 pending exception 明细（NoSuchMethodError 会点名错配的方法）
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
            }
            jni::sys::JNI_ERR
        }
    }
}

// ── AndroidBitmap 锁像素呈现 ──

#[cfg(target_os = "android")]
mod jnigraphics {
    use jni::objects::JObject;
    use jni::sys::{jint, jobject, JNIEnv as RawEnv};
    use jni::JNIEnv;
    use std::ffi::c_void;

    #[repr(C)]
    pub struct AndroidBitmapInfo {
        pub width: u32,
        pub height: u32,
        pub stride: u32,
        pub format: jint,
        pub flags: u32,
    }

    pub const ANDROID_BITMAP_FORMAT_RGBA_8888: jint = 1;
    const ANDROID_BITMAP_RESUT_SUCCESS: jint = 0;

    #[link(name = "jnigraphics")]
    extern "C" {
        fn AndroidBitmap_getInfo(
            env: *mut RawEnv,
            bitmap: jobject,
            info: *mut AndroidBitmapInfo,
        ) -> jint;
        fn AndroidBitmap_lockPixels(
            env: *mut RawEnv,
            bitmap: jobject,
            addr: *mut *mut c_void,
        ) -> jint;
        fn AndroidBitmap_unlockPixels(env: *mut RawEnv, bitmap: jobject) -> jint;
    }

    /// 锁定 ARGB_8888 位图像素。返回 (基地址, stride 字节)。
    pub fn lock(env: &mut JNIEnv, bitmap: &JObject) -> Option<(*mut u8, usize)> {
        let mut info = AndroidBitmapInfo {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            flags: 0,
        };
        let raw = env.get_raw();
        if unsafe { AndroidBitmap_getInfo(raw, bitmap.as_raw(), &mut info) }
            != ANDROID_BITMAP_RESUT_SUCCESS
        {
            return None;
        }
        if info.format != ANDROID_BITMAP_FORMAT_RGBA_8888 {
            return None;
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        if unsafe { AndroidBitmap_lockPixels(raw, bitmap.as_raw(), &mut ptr) }
            != ANDROID_BITMAP_RESUT_SUCCESS
            || ptr.is_null()
        {
            return None;
        }
        Some((ptr as *mut u8, info.stride as usize))
    }

    pub fn unlock(env: &mut JNIEnv, bitmap: &JObject) {
        unsafe { AndroidBitmap_unlockPixels(env.get_raw(), bitmap.as_raw()) };
    }
}

/// 位图呈现目标：引擎帧按行拷入（考虑 stride 对齐）。
#[cfg(target_os = "android")]
struct BitmapSurface {
    base: *mut u8,
    stride: usize,
}

#[cfg(target_os = "android")]
impl BitmapSurface {
    fn lock(env: &mut JNIEnv, bitmap: &JObject) -> Option<Self> {
        let (base, stride) = jnigraphics::lock(env, bitmap)?;
        Some(Self { base, stride })
    }

    fn unlock(self, env: &mut JNIEnv, bitmap: &JObject) {
        jnigraphics::unlock(env, bitmap);
    }
}

// SAFETY: 锁定期内仅 UI 线程访问
#[cfg(target_os = "android")]
unsafe impl Send for BitmapSurface {}

#[cfg(target_os = "android")]
impl Surface for BitmapSurface {
    fn present_cpu(&mut self, rgba: &[u8], size: (u32, u32), _dirty: Option<Rect>) {
        let w = size.0 as usize;
        let h = size.1 as usize;
        if rgba.len() < w * h * 4 {
            return;
        }
        for row in 0..h {
            let src = &rgba[row * w * 4..][..w * 4];
            // SAFETY: lock 保证 [base, base+h*stride) 可写
            let dst =
                unsafe { std::slice::from_raw_parts_mut(self.base.add(row * self.stride), w * 4) };
            dst.copy_from_slice(src);
        }
    }
}

#[cfg(target_os = "android")]
fn lock_bitmap(env: &mut JNIEnv, bitmap: &JObject) -> Option<BitmapSurface> {
    BitmapSurface::lock(env, bitmap)
}

/// 非 Android 目标（host 单测/检查）下的空实现，保证 cargo check 通过。
#[cfg(not(target_os = "android"))]
struct DummySurface;

#[cfg(not(target_os = "android"))]
impl DummySurface {
    fn unlock(self, _env: &mut JNIEnv, _bitmap: &JObject) {}
}

#[cfg(not(target_os = "android"))]
impl Surface for DummySurface {
    fn present_cpu(&mut self, _rgba: &[u8], _size: (u32, u32), _dirty: Option<Rect>) {}
}

#[cfg(not(target_os = "android"))]
fn lock_bitmap(_env: &mut JNIEnv, _bitmap: &JObject) -> Option<DummySurface> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注册表结构完整性：数量、名字唯一、指针非空、描述符形状合法
    ///（以 `(` 开头、含返回类型、无空白）。不触 JVM，host 可跑。
    #[test]
    fn method_table_well_formed() {
        let methods = native_methods();
        assert_eq!(
            methods.len(),
            46,
            "与 PaintEngineView.kt 的 external fun 数量一致"
        );
        let mut names: Vec<String> = methods
            .iter()
            .map(|m| m.name.to_str().unwrap_or_default().to_owned())
            .collect();
        names.sort_unstable();
        assert!(names.windows(2).all(|w| w[0] != w[1]), "存在重复方法名");
        for m in &methods {
            let name = m.name.to_str().unwrap_or_default();
            let sig = m.sig.to_str().unwrap_or_default();
            assert!(!m.fn_ptr.is_null(), "{name} 实现指针为空");
            assert!(sig.starts_with('('), "{name} 描述符缺 '('：{sig}");
            assert!(sig.len() >= 3, "{name} 描述符过短：{sig}");
            assert!(
                !sig.chars().any(char::is_whitespace),
                "{name} 描述符含空白：{sig}"
            );
        }
    }

    /// 目标类常量是合法二进制名（`/` 分隔，无 `.`）。
    #[test]
    fn jni_class_name_slash_separated() {
        assert!(!JNI_CLASS.contains('.'));
        assert!(JNI_CLASS.split('/').all(|p| !p.is_empty()));
    }
}
