//! paint-android：Android 壳（cdylib + JNI）。
//!
//! 职责与 paint-wasm 对称：
//! - 呈现：`AndroidBitmap_lockPixels` 直接把引擎帧写进 Kotlin 侧
//!   传入的 ARGB_8888 Bitmap（预乘一致，零额外拷贝）
//! - 输入：Kotlin `PaintEngineView` 把 MotionEvent（含历史点展开、
//!   压感、笔悬停）翻译成 PlatformEvent 转发进来
//!
//! JNI 命名对应 `com.paintengine.android.PaintEngineView` 的
//! `external fun` 声明。引擎句柄以 jlong（裸指针）往返，
//! 单 UI 线程使用。

use jni::objects::{JByteArray, JClass, JObject};
use jni::sys::{jboolean, jbyte, jbyteArray, jdouble, jint, jlong};
use jni::JNIEnv;
use paint_core::input::{PointerKind, PointerPhase, PointerSample};
use paint_core::render::{EngineConfig, Surface};
use paint_core::{Engine, PlatformEvent, Rect, Tool};
use paint_render::SoftwareRenderer;

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
        fn __android_log_print(prio: i32, tag: *const u8, fmt: *const u8, ...)
            -> i32;
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

/// logcat 日志器（nativeCreate 时安装一次）。
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
static LOGGER_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeCreate(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    init_logcat_logger();
    let e = Engine::new(Box::new(SoftwareRenderer::new()), EngineConfig::default());
    Box::into_raw(Box::new(e)) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeDestroy(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        drop(unsafe { Box::from_raw(handle as *mut Engine) });
    }
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeResize(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    w: jint,
    h: jint,
    scale: jdouble,
) {
    if w > 0 && h > 0 {
        engine(handle).handle_event(PlatformEvent::Resize {
            w: w as u32,
            h: h as u32,
            scale: scale as f32,
        });
    }
}

/// `pressure < 0` 表示无压感（手指/鼠标按满压处理）；
/// `tilt_x/tilt_y = NaN` 表示无倾斜数据。
#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativePointer(
    _env: JNIEnv,
    _class: JClass,
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

fn ppressure(v: f64) -> f32 {
    (v as f32).clamp(0.0, 1.0)
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativePenInRange(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    in_range: jboolean,
) {
    engine(handle).handle_event(PlatformEvent::PenInRange(in_range != 0));
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeFocus(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    focused: jboolean,
) {
    engine(handle).handle_event(PlatformEvent::Focus(focused != 0));
}

/// 合成到 `bitmap`（ARGB_8888，与引擎预乘 RGBA 布局一致）。
/// 返回 false 表示位图格式不受支持。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeRender(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    bitmap: JObject,
) -> jboolean {
    let mut env = env;
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

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetTool(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    code: jint,
) {
    engine(handle).set_tool(tool_from_code(code));
}

/// 文字工具锚点（画布坐标）。无锚点返回 null；一次性取走。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeTakeTextAnchor(
    env: JNIEnv,
    _class: JClass,
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeDrawText(
    mut env: JNIEnv,
    _class: JClass,
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetTextFont(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    font: JByteArray,
) {
    if let Ok(bytes) = env.convert_byte_array(&font) {
        engine(handle).set_text_font(bytes);
    }
}

/// 新增文字对象（非破坏，可重编辑）。`raster = null` → 引擎 swash 渲染。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeAddTextObject(
    mut env: JNIEnv,
    _class: JClass,
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
    engine(handle)
        .add_text_object((x, y), &text, size as f32, None)
        as jboolean
}

/// 命中的文字对象信息（预填编辑框）：[text, size, rrggbb]；无则 null。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeTakeTextEdit(
    mut env: JNIEnv,
    _class: JClass,
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeUpdateTextObject(
    mut env: JNIEnv,
    _class: JClass,
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeDeleteTextObject(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).delete_text_object() as jboolean
}

/// 呈现帧计数（单调；监控关闭时冻结）。壳层采样差值算帧率。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeRenderCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    engine(handle).render_count() as jlong
}

/// 帧率监控开关。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetFpsMonitor(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    on: jboolean,
) {
    engine(handle).set_fps_monitor(on != 0);
}

/// 视口定位：把画布坐标移到屏幕中心（小地图拖动）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeViewportCenterOn(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    x: jdouble,
    y: jdouble,
) {
    engine(handle).viewport_center_on(x, y);
}

/// 可见画布区域 AABB：(x, y, w, h)。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeViewportRect(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jni::sys::jfloatArray {
    let (x, y, w, h) = engine(handle).viewport_rect();
    let vals = [x as f32, y as f32, w as f32, h as f32];
    match env.new_float_array(4) {
        Ok(mut a) => {
            if env.set_float_array_region(&mut a, 0, &vals).is_ok() {
                a.as_raw()
            } else {
                std::ptr::null_mut()
            }
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// 小地图：返回 Object[2] = {byte[] png, float[7] meta(ow,oh,bx,by,bw,bh,占位)}。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeMinimapPng(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    max_w: jint,
    max_h: jint,
) -> jni::sys::jobjectArray {
    let Some((png, ow, oh, bx, by, bw, bh)) =
        engine(handle).minimap_png(max_w.max(1) as u32, max_h.max(1) as u32)
    else {
        return std::ptr::null_mut();
    };
    let arr = match env.new_object_array(2, "java/lang/Object", JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };
    let Ok(bytes) = env.byte_array_from_slice(&png) else {
        return std::ptr::null_mut();
    };
    let _ = env.set_object_array_element(&arr, 0, bytes);
    let meta = [ow as f32, oh as f32, bx as f32, by as f32, bw as f32, bh as f32, 0.0];
    if let Ok(mut m) = env.new_float_array(7) {
        if env.set_float_array_region(&mut m, 0, &meta).is_ok() {
            let _ = env.set_object_array_element(&arr, 1, m);
        }
    }
    arr.as_raw()
}

/// 日志级别设置（0=Off 1=Error 2=Warn 3=Info 4=Debug 5=Trace；adb 调试用）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetLogLevel(
    _env: JNIEnv,
    _class: JClass,
    level: jint,
) {
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeEditCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jlong {
    engine(handle).edit_count() as jlong
}

/// 存 .ora 工程字节（自动保存用）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSaveOra(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jbyteArray {
    let Some(bytes) = engine(handle).save_ora() else {
        return std::ptr::null_mut();
    };
    match env.byte_array_from_slice(&bytes) {
        Ok(arr) => arr.as_raw().cast(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 载入 .ora 替换当前文档。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeLoadOra(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    data: JByteArray,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(&data) else {
        return 0;
    };
    engine(handle).load_ora(&bytes) as jboolean
}

/// 预设名列表（内置 6 支 + 用户自定义）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativePresetNames(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jni::sys::jobjectArray {
    let names = engine(handle).preset_names();
    let arr = match env.new_object_array(
        names.len() as i32,
        "java/lang/String",
        JObject::null(),
    ) {
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeApplyPreset(
    mut env: JNIEnv,
    _class: JClass,
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
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeImportImage(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    data: JByteArray,
) -> jboolean {
    let Ok(bytes) = env.convert_byte_array(&data) else {
        return 0;
    };
    engine(handle).import_image(&bytes).is_some() as jboolean
}

/// 直行 RGBA → 浮动层（PDF 等壳层渲染的位图走此通道，可拖拽放置）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativePasteRgba(
    env: JNIEnv,
    _class: JClass,
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
    engine(handle)
        .paste_rgba_float(&bytes, w as u32, h as u32) as jboolean
}

// ── 浮动内容变换（附件放置交互）──

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeTransforming(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).transforming() as jboolean
}

/// 屏幕位移 → 画布位移（经视口逆变换，旋转/缩放下方向正确）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeTransformTranslateScreen(
    _env: JNIEnv,
    _class: JClass,
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

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeTransformRotate(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    delta_deg: jdouble,
) {
    engine(handle)
        .transform_rotate(delta_deg.to_radians());
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeTransformScale(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    factor: jdouble,
) {
    engine(handle).transform_scale(factor);
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeCommitTransform(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).commit_transform() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeCancelTransform(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).cancel_transform() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetBrushSize(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    size: jdouble,
) {
    engine(handle).brush_mut().size = (size as f32).clamp(1.0, 512.0);
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetBrushColor(
    _env: JNIEnv,
    _class: JClass,
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

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeUndo(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).undo() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeRedo(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).redo() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeAddLayer(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).add_layer().is_some() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeMergeDown(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).merge_down() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeFlatten(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jboolean {
    engine(handle).flatten() as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeLayerCount(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    engine(handle).layer_count() as jint
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeActiveLayerIndex(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jint {
    engine(handle)
        .active_layer_index()
        .map_or(-1, |i| i as jint)
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSelectLayerIndex(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    index: jint,
) -> jboolean {
    if index < 0 {
        return 0;
    }
    engine(handle).select_layer_index(index as usize) as jboolean
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeLayerNameAt(
    env: JNIEnv,
    _class: JClass,
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

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeFitToContent(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    engine(handle).fit_to_content(48.0);
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeZoom100(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    engine(handle).zoom_100();
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetShowGrid(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    show: jboolean,
) {
    engine(handle).set_show_grid(show != 0);
}

/// 导出 PNG（可见内容包围盒，透明背景）。
#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeExportPng(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jbyteArray {
    let Some(png) = engine(handle).export_png(None, 1.0, true) else {
        return std::ptr::null_mut();
    };
    match env.byte_array_from_slice(&png) {
        Ok(arr) => arr.as_raw().cast(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeImportPng(
    env: JNIEnv,
    _class: JClass,
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
