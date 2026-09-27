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

/// `pressure < 0` 表示无压感（手指/鼠标按满压处理）。
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
        tilt: None, // M3 后续：getAxisValue(MOTION_EVENT_AXIS_TILT) 接入
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

#[no_mangle]
pub extern "system" fn Java_com_paintengine_android_PaintEngineView_nativeSetTool(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    eraser: jboolean,
) {
    engine(handle).set_tool(if eraser != 0 {
        Tool::Eraser
    } else {
        Tool::Brush
    });
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
